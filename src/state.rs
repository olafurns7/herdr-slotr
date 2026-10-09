use anyhow::{Context, Result, ensure};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub enqueue_seq: u64,
    pub pool: String,
    pub campaign: String,
    pub purpose: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub pane: String,
    pub cwd: String,
    pub cost_mib: u64,
    pub lease_seconds: f64,
    #[serde(with = "crate::timestamp::required")]
    pub since: f64,
    #[serde(default)]
    pub seen_at: f64,
    // A consumed stop claim stays with this ticket, preventing eviction cascades.
    #[serde(default)]
    pub stop_claimed_by: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Holder {
    #[serde(flatten)]
    pub request: Request,
    pub run: String,
    pub admit_seq: u64,
    pub slot: u32,
    #[serde(default)]
    pub port_base: Option<u32>,
    #[serde(default)]
    pub port_end: Option<u32>,
    #[serde(with = "crate::timestamp::required")]
    pub admitted_at: f64,
    #[serde(default, with = "crate::timestamp::optional")]
    pub lease_expires_at: Option<f64>,
    #[serde(default)]
    pub started: bool,
    #[serde(default, with = "crate::timestamp::optional")]
    pub stopping_at: Option<f64>,
    #[serde(default, with = "crate::timestamp::optional")]
    pub warned_at: Option<f64>,
    #[serde(default)]
    pub warned_for: Option<u64>,
    #[serde(default)]
    pub warned_waiters: Vec<u64>,
    #[serde(default, with = "crate::timestamp::optional")]
    pub idle_since: Option<f64>,
    #[serde(default)]
    pub cpu_usage_usec: Option<u64>,
    #[serde(default, with = "crate::timestamp::optional")]
    pub cpu_sample_at: Option<f64>,
    #[serde(default, with = "crate::timestamp::optional")]
    pub holder_idle_since: Option<f64>,
    #[serde(default)]
    pub holder_idle_warned: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StopRecord {
    #[serde(with = "crate::timestamp::required")]
    pub at: f64,
    pub run: String,
    pub reason: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct State {
    pub schema_version: u32,
    pub enqueue_seq: u64,
    pub admit_seq: u64,
    pub holders: Vec<Holder>,
    pub queue: Vec<Request>,
    #[serde(default)]
    pub last_stop: Option<StopRecord>,
    #[serde(default, with = "crate::timestamp::optional")]
    pub healthy_since: Option<f64>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enqueue_seq: 0,
            admit_seq: 0,
            holders: vec![],
            queue: vec![],
            last_stop: None,
            healthy_since: None,
        }
    }
}
pub fn root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
        })
        .join("slotr")
}
pub fn read(root: &Path) -> Result<State> {
    let path = root.join("state.json");
    let state: State = match fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data)
            .with_context(|| format!("corrupt state: {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e).with_context(|| format!("read state: {}", path.display())),
    };
    ensure!(
        state.schema_version == 1
            && state
                .holders
                .iter()
                .all(|h| h.run == format!("slotr-{}-{}", h.request.pool, h.admit_seq)),
        "corrupt state: {}",
        path.display()
    );
    Ok(state)
}
pub fn writable(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?)
}
pub fn transaction<T>(f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    let root = root();
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let lock = writable(&root.join("state.lock"))?;
    flock(&lock, FlockOperation::LockExclusive)?;
    let mut state = read(&root)?;
    let out = f(&mut state)?;
    let pending = root.join("state.json.tmp");
    let mut file = writable(&pending)?;
    file.set_len(0)?;
    serde_json::to_writer(&mut file, &state)?;
    file.write_all(b"\n")?;
    fs::rename(&pending, root.join("state.json"))?;
    Ok(out)
}
pub fn ticket_live(seq: u64) -> bool {
    match File::open(root().join(format!("ticket-{seq}"))) {
        Ok(f) => !matches!(flock(&f, FlockOperation::NonBlockingLockShared), Ok(())),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}
pub fn clean_tickets(s: &State) -> Result<()> {
    for entry in fs::read_dir(root())? {
        let path = entry?.path();
        let seq = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("ticket-"))
            .and_then(|s| s.parse::<u64>().ok());
        if let Some(seq) = seq
            && !s.queue.iter().any(|q| q.enqueue_seq == seq)
            && !s.holders.iter().any(|h| h.request.enqueue_seq == seq)
            && !ticket_live(seq)
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
