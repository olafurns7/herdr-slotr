use crate::{
    config,
    state::{self, Holder, State},
    stats,
};
use anyhow::{Context, Result, bail, ensure};
use rustix::{
    fs::{FlockOperation, flock},
    process::{Pid, Signal, kill_process_group, test_kill_process_group},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const BUS_ERROR: &str =
    "no systemd user bus here; run it in a terminal with a systemd user session";
static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn handler(signal: i32) {
    SIGNAL.store(signal, Ordering::Relaxed);
}
unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}
pub fn signals() {
    // The handler only stores a lock-free atomic. Children reset handlers on exec.
    // HUP: closing the terminal tab that runs `slotr run` stops the unit,
    // unless HUP was inherited as ignored (nohup). /proc avoids a set-then-restore race.
    let hup_ignored = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("SigIgn:"))
                .and_then(|m| u64::from_str_radix(m.trim(), 16).ok())
        })
        .is_some_and(|mask| mask & 1 != 0);
    unsafe {
        if !hup_ignored {
            signal(1, handler);
        }
        signal(2, handler);
        signal(15, handler);
    }
}
pub fn cancelled() -> i32 {
    SIGNAL.load(Ordering::Relaxed)
}
pub fn now() -> f64 {
    let offset = if std::env::var("SLOTR_TEST").as_deref() == Ok("1") {
        fs::read_to_string(stats::setting("CLOCK_OFFSET_FILE", ""))
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let time = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    time.tv_sec as f64 + time.tv_nsec as f64 / 1_000_000_000.0 + offset
}
pub fn wall_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn code(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}
pub fn capture(command: &mut Command, timeout: Duration, read_output: bool) -> Result<Output> {
    let mut child = command
        .stdout(if read_output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if read_output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = vec![];
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let out = stdout.map(|pipe| reader(Box::new(pipe)));
    let err = stderr.map(|pipe| reader(Box::new(pipe)));
    let start = Instant::now();
    let status = loop {
        let status = child.try_wait()?;
        if let Some(status) = status {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = kill_group(&child, Signal::KILL);
            let _ = child.kill();
            let _ = child.wait();
            bail!("subprocess timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Descendants must close inherited pipes before the reader joins.
    let _ = kill_group(&child, Signal::KILL);
    Ok(Output {
        status,
        stdout: out
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(std::io::Error::other("output reader failed")))
            })
            .transpose()?
            .unwrap_or_default(),
        stderr: err
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(std::io::Error::other("error reader failed")))
            })
            .transpose()?
            .unwrap_or_default(),
    })
}
pub fn ctl(args: &[&str]) -> Result<Output> {
    let output = capture(
        Command::new(stats::setting("SYSTEMCTL", "systemctl"))
            .arg("--user")
            .args(args)
            .process_group(0),
        Duration::from_secs(5),
        true,
    )?;
    if String::from_utf8_lossy(&output.stderr)
        .to_lowercase()
        .contains("connect to")
        && String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("bus")
    {
        bail!(BUS_ERROR);
    }
    Ok(output)
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub live: bool,
    pub started: bool,
    pub anon_mib: Option<f64>,
    pub cpu_usec: Option<u64>,
}
pub fn observations(s: &State) -> Result<BTreeMap<String, Observation>> {
    let mut out = BTreeMap::new();
    for h in &s.holders {
        let active = ctl(&["is-active", &h.run])?;
        let live = ["active", "activating", "deactivating", "reloading"]
            .contains(&String::from_utf8_lossy(&active.stdout).trim());
        ensure!(
            live || matches!(active.status.code(), Some(0 | 3 | 4)),
            "cannot query unit {}",
            h.run
        );
        let cg = if live {
            let v = ctl(&["show", "--property=ControlGroup", "--value", &h.run])?;
            ensure!(v.status.success(), "cannot query cgroup {}", h.run);
            String::from_utf8_lossy(&v.stdout).trim().to_owned()
        } else {
            String::new()
        };
        let path = PathBuf::from(stats::setting("CGROUP_ROOT", "/sys/fs/cgroup"))
            .join(cg.trim_start_matches('/'));
        let field = |name: &str, key: &str| {
            fs::read_to_string(path.join(name)).ok().and_then(|s| {
                s.lines().find_map(|l| {
                    let mut parts = l.split_whitespace();
                    if parts.next()? == key {
                        parts.next()?.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
            })
        };
        out.insert(
            h.run.clone(),
            Observation {
                live,
                started: h.started,
                anon_mib: if cg.is_empty() {
                    None
                } else {
                    field("memory.stat", "anon").map(|n| n as f64 / 1048576.0)
                },
                cpu_usec: if cg.is_empty() {
                    None
                } else {
                    field("cpu.stat", "usage_usec")
                },
            },
        );
    }
    Ok(out)
}
pub fn reconcile(s: &mut State, obs: &BTreeMap<String, Observation>) {
    s.queue.retain(|q| state::ticket_live(q.enqueue_seq));
    s.holders.retain(|h| {
        obs.get(&h.run).is_none_or(|o| {
            o.live
                || o.started != h.started
                || (!h.started && state::ticket_live(h.request.enqueue_seq))
        })
    });
    for q in &mut s.queue {
        if q.stop_claimed_by.as_ref().is_some_and(|run| {
            !s.holders.iter().any(|h| &h.run == run)
                && s.last_stop.as_ref().is_none_or(|stop| &stop.run != run)
        }) {
            q.stop_claimed_by = None;
        }
    }
    // ponytail: live waiter scans are quadratic; use a set if queues become large.
    for h in &mut s.holders {
        h.warned_waiters
            .retain(|seq| s.queue.iter().any(|q| q.enqueue_seq == *seq));
    }
}
pub fn events_path() -> PathBuf {
    config::xdg("XDG_STATE_HOME", ".local/state").join("slotr/events.jsonl")
}
pub fn event(mut data: Value) -> Result<()> {
    data["at"] = json!(crate::timestamp::format(wall_now())?);
    let path = events_path();
    fs::create_dir_all(path.parent().unwrap())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let mut bytes = serde_json::to_vec(&data)?;
    bytes.push(b'\n');
    // One append write keeps concurrently emitted short records together.
    ensure!(file.write(&bytes)? == bytes.len(), "short event write");
    Ok(())
}
pub fn legacy(pool: &config::Pool, readonly: bool) -> Result<(bool, Option<File>)> {
    let Some(l) = &pool.legacy_lock else {
        return Ok((true, None));
    };
    let file = if readonly {
        match File::open(&l.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((true, None)),
            Err(e) => return Err(e.into()),
        }
    } else {
        state::writable(std::path::Path::new(&l.path))?
    };
    let operation = if l.mode == "shared" {
        FlockOperation::NonBlockingLockShared
    } else {
        FlockOperation::NonBlockingLockExclusive
    };
    match flock(&file, operation) {
        Ok(()) => Ok((true, Some(file))),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok((false, None)),
        Err(e) => Err(e.into()),
    }
}
pub fn ports_free(base: u32, probe: u32) -> bool {
    let mut sockets = vec![];
    for port in base..base + probe {
        for addr in ["127.0.0.1", "::1"] {
            match std::net::TcpListener::bind((addr, port as u16)) {
                Ok(socket) => sockets.push(socket),
                Err(e) if addr == "::1" && matches!(e.raw_os_error(), Some(97 | 99)) => {} // IPv6 disabled on this host.
                Err(_) => return false,
            }
        }
    }
    true
}
fn legacy_holder(pool: &config::Pool) -> Option<(i32, bool)> {
    let metadata = fs::metadata(&pool.legacy_lock.as_ref()?.path).ok()?;
    let target = format!(
        "{:02x}:{:02x}:{}",
        rustix::fs::major(metadata.dev()),
        rustix::fs::minor(metadata.dev()),
        metadata.ino()
    );
    fs::read_to_string(PathBuf::from(stats::setting("PROC_ROOT", "/proc")).join("locks"))
        .ok()?
        .lines()
        .find_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.len() > 5 && p[1] == "FLOCK" && p[5] == target)
                .then(|| p[4].parse().ok().map(|pid| (pid, p[3] == "WRITE")))
                .flatten()
        })
}
pub fn legacy_pid(pool: &config::Pool) -> Option<i32> {
    legacy_holder(pool).map(|(pid, _)| pid)
}
pub fn legacy_free(pool: &config::Pool) -> bool {
    legacy_holder(pool).is_none_or(|(_, exclusive)| {
        !exclusive
            && pool
                .legacy_lock
                .as_ref()
                .is_some_and(|l| l.mode == "shared")
    })
}
pub fn hook(cfg: &config::Config, name: &str, h: &Holder, reason: &str) {
    let argv = match name {
        "on_admit" => &cfg.hooks.on_admit,
        "on_warn" => &cfg.hooks.on_warn,
        _ => &cfg.hooks.on_stop,
    };
    if argv.is_empty()
        || (h.request.task.is_empty() && argv.iter().any(|s| s.contains("{task}")))
        || (h.request.pane.is_empty() && argv.iter().any(|s| s.contains("{pane}")))
    {
        return;
    }
    let slot = h.slot.to_string();
    let base = h.port_base.map(|p| p.to_string()).unwrap_or_default();
    let events = events_path().display().to_string();
    let values = [
        ("run", h.run.as_str()),
        ("pool", &h.request.pool),
        ("campaign", &h.request.campaign),
        ("purpose", &h.request.purpose),
        ("task", &h.request.task),
        ("pane", &h.request.pane),
        ("reason", reason),
        ("slot", &slot),
        ("port_base", &base),
        ("events", &events),
    ];
    // One pass: replacement text containing another placeholder remains literal.
    let expand = |s: &str| {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            rest = &rest[i..];
            if let Some(j) = rest.find('}') {
                let key = &rest[1..j];
                out.push_str(
                    values
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map_or(&rest[..=j], |(_, v)| *v),
                );
                rest = &rest[j + 1..];
            } else {
                break;
            }
        }
        out.push_str(rest);
        out
    };
    let args: Vec<_> = argv.iter().map(|s| expand(s)).collect();
    let result = capture(
        Command::new(&args[0]).args(&args[1..]).process_group(0),
        Duration::from_secs(20),
        false,
    );
    let (exit, error) = match result {
        Ok(o) => (Some(code(o.status)), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let _ = event(json!({"event":"hook","hook":name,"run":h.run,"exit":exit,"error":error}));
}
pub fn kill_group(child: &Child, signal: Signal) -> Result<()> {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
        match kill_process_group(pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn terminate(child: &mut Child, seconds: f64) -> Result<()> {
    kill_group(child, Signal::TERM)?;
    finish_termination(child, Instant::now() + Duration::from_secs_f64(seconds))
}
pub fn finish_termination(child: &mut Child, deadline: Instant) -> Result<()> {
    let pid = Pid::from_raw(child.id() as i32).context("invalid workload pid")?;
    while Instant::now() < deadline {
        let _ = child.try_wait()?;
        if matches!(test_kill_process_group(pid), Err(rustix::io::Errno::SRCH)) {
            child.wait().context("reap workload")?;
            return Ok(());
        }
        thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    kill_group(child, Signal::KILL)?;
    child.wait().context("reap workload")?;
    Ok(())
}
pub fn stop_event(
    cfg: &config::Config,
    h: &Holder,
    reason: &str,
    s: &State,
    obs: &BTreeMap<String, Observation>,
    sample: &stats::Stats,
) -> Result<()> {
    let holders:Vec<_>=s.holders.iter().map(|h|json!({"run":h.run,"pool":h.request.pool,"cost_mib":h.request.cost_mib,"anon_mib":obs.get(&h.run).and_then(|o|o.anon_mib)})).collect();
    let mut data =
        json!({"event":"stop","run":h.run,"reason":reason,"stats":sample,"holders":holders});
    if !cfg.watchdog.observe_locks.is_empty() {
        let mut locks = BTreeMap::new();
        for path in &cfg.watchdog.observe_locks {
            let held = match File::open(path) {
                Ok(f) => match flock(&f, FlockOperation::NonBlockingLockShared) {
                    Ok(()) => Some(false),
                    Err(rustix::io::Errno::WOULDBLOCK) => Some(true),
                    Err(_) => None,
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
                Err(_) => None,
            };
            locks.insert(path, held);
        }
        data["observed_locks"] = json!(locks);
    }
    event(data)
}
