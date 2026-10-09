use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};
use toml::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub pools: BTreeMap<String, Pool>,
    pub admission: Admission,
    pub watchdog: Watchdog,
    pub lease: Lease,
    pub hooks: Hooks,
    pub priority: Priority,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Priority {
    pub campaigns: Vec<String>,
    pub file: String,
}
impl Priority {
    fn path(&self) -> PathBuf {
        if self.file == "~" {
            PathBuf::from(env::var_os("HOME").unwrap_or_default())
        } else if let Some(rest) = self.file.strip_prefix("~/") {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(rest)
        } else {
            PathBuf::from(&self.file)
        }
    }
    pub fn level(&self, campaign: &str, task: &str) -> u32 {
        let matches = |pattern: &str| {
            pattern
                .strip_suffix('*')
                .map_or(pattern == campaign, |prefix| campaign.starts_with(prefix))
        };
        let mut level = u32::from(self.campaigns.iter().any(|p| matches(p)));
        if !self.file.is_empty()
            && let Ok(bytes) = fs::read(self.path())
        {
            for line in bytes
                .split(|b| *b == b'\n')
                .filter_map(|line| std::str::from_utf8(line).ok())
            {
                let fields: Vec<_> = line
                    .split('#')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                if let [kind, name, value] = fields.as_slice()
                    && ((*kind == "campaign" && matches(name))
                        || (*kind == "task" && *name == task))
                    && let Ok(value) = value.parse::<u32>()
                {
                    level = level.max(value);
                }
            }
        }
        level
    }
    pub fn status(&self) -> serde_json::Value {
        if self.file.is_empty() {
            serde_json::json!({"state":"off"})
        } else {
            let path = self.path();
            if fs::read(&path).is_ok() {
                let age = fs::metadata(path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .map_or(0, |age| age.as_secs());
                serde_json::json!({"state":"ok", "age_seconds":age})
            } else {
                serde_json::json!({"state":"missing"})
            }
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pool {
    pub slots: u32,
    pub memory_gated: bool,
    pub evictable: bool,
    pub default_cost_mib: u64,
    pub max_lease: String,
    pub campaign_cap: u32,
    pub ports: Option<Ports>,
    pub legacy_lock: Option<Legacy>,
    #[serde(default)]
    pub kinds: BTreeMap<String, Kind>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Ports {
    pub base: u32,
    pub stride: u32,
    pub probe: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Legacy {
    pub path: String,
    pub mode: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Kind {
    pub cost_mib: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Admission {
    pub reserve_mib: u64,
    pub psi_full_avg60_max: f64,
    pub load1_per_core_max: f64,
    pub recovery_healthy_seconds: f64,
    pub queue_poll_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Watchdog {
    pub observe_locks: Vec<String>,
    pub interval_ms: u64,
    pub on_pressure: String,
    pub stop_available_mib: u64,
    pub stop_available_samples: u32,
    pub emergency_available_mib: u64,
    pub stop_psi_full_avg10_min: f64,
    pub stop_psi_samples: u32,
    pub term_grace_seconds: f64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lease {
    pub on_expiry: String,
    pub grace_seconds: f64,
    pub waiter_min_wait_seconds: f64,
    pub idle_release_minutes: f64,
    pub idle_cpu_ms_per_min: f64,
    pub holder_probe: Vec<String>,
    pub holder_idle_minutes: f64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Hooks {
    pub on_admit: Vec<String>,
    pub on_warn: Vec<String>,
    pub on_stop: Vec<String>,
}

pub fn duration(s: &str) -> Result<f64> {
    let (n, multiplier) = match s.as_bytes().last() {
        Some(b's') => (&s[..s.len() - 1], 1.0),
        Some(b'm') => (&s[..s.len() - 1], 60.0),
        Some(b'h') => (&s[..s.len() - 1], 3600.0),
        Some(b'd') => (&s[..s.len() - 1], 86400.0),
        _ => (s, 1.0),
    };
    let seconds = n
        .parse::<f64>()
        .context("expected duration, e.g. 4h, 300s or 0")?
        * multiplier;
    ensure!(seconds.is_finite() && seconds >= 0.0, "invalid duration");
    Ok(seconds)
}
pub fn xdg(key: &str, fallback: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(fallback))
}
fn pool_template() -> Value {
    let mut v: Value =
        toml::from_str(include_str!("../config.example.toml")).expect("default config");
    v["pools"]
        .as_table_mut()
        .unwrap()
        .remove("default")
        .unwrap()
}
fn schema(path: &str) -> Result<Value> {
    let parts: Vec<_> = path.split('.').collect();
    match parts.as_slice() {
        ["pools", name] => {
            ensure!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "config {path}: invalid pool name"
            );
            Ok(pool_template())
        }
        ["pools", _, "ports"] => Ok(toml::from_str("base=31000\nstride=32\nprobe=7")?),
        ["pools", _, "legacy_lock"] => Ok(toml::from_str("path=\"\"\nmode=\"shared\"")?),
        ["pools", _, "kinds"] => Ok(Value::Table(Default::default())),
        ["pools", _, "kinds", _] => Ok(toml::from_str("cost_mib=7680")?),
        _ => bail!("config {path}: unknown key"),
    }
}
fn merge(
    dst: &mut Value,
    src: Value,
    path: &str,
    source: &str,
    sources: &mut BTreeMap<String, String>,
) -> Result<()> {
    if let Value::Table(values) = src {
        let table = dst
            .as_table_mut()
            .with_context(|| format!("config {path}: expected scalar"))?;
        for (key, value) in values {
            let p = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            if !table.contains_key(&key) {
                table.insert(key.clone(), schema(&p)?);
            }
            merge(table.get_mut(&key).unwrap(), value, &p, source, sources)?;
        }
    } else {
        let valid = match (&*dst, &src) {
            (Value::Integer(_), Value::Integer(v)) => *v >= 0,
            (Value::Float(_), Value::Integer(v)) => *v >= 0,
            (Value::Float(_), Value::Float(v)) => v.is_finite() && *v >= 0.0,
            (Value::Boolean(_), Value::Boolean(_)) | (Value::String(_), Value::String(_)) => true,
            (Value::Array(_), Value::Array(v)) => v.iter().all(Value::is_str),
            _ => false,
        };
        ensure!(valid, "config {path}: wrong type or invalid value");
        *dst = src;
        sources.insert(path.to_owned(), source.to_owned());
    }
    Ok(())
}
fn leaves(v: &Value, path: &str, out: &mut BTreeMap<String, String>) {
    if let Some(t) = v.as_table() {
        for (k, v) in t {
            leaves(
                v,
                &if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                },
                out,
            );
        }
    } else {
        out.entry(path.into()).or_insert_with(|| "default".into());
    }
}
pub fn load(file: Option<&Path>) -> Result<(Config, BTreeMap<String, String>)> {
    let mut value: Value = toml::from_str(include_str!("../config.example.toml"))?;
    let mut sources = BTreeMap::new();
    let path = file
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("SLOTR_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join("slotr/config.toml"));
    match fs::read_to_string(&path) {
        Ok(text) => {
            let custom: Value =
                toml::from_str(&text).with_context(|| format!("config {}", path.display()))?;
            if custom.get("pools").is_some() {
                value["pools"] = Value::Table(Default::default());
            }
            merge(&mut value, custom, "", "file", &mut sources)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && file.is_none() => {}
        Err(e) => return Err(e).with_context(|| format!("config {}", path.display())),
    }
    for (key, raw) in env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k.starts_with("SLOTR_") && k.contains("__"))
    {
        let parts: Vec<_> = key[6..].split("__").map(str::to_lowercase).collect();
        let path = parts.join(".");
        let mut cursor = &mut value;
        for i in 0..parts.len() {
            let parent = cursor
                .as_table_mut()
                .with_context(|| format!("config {path}: invalid path"))?;
            if !parent.contains_key(&parts[i]) {
                parent.insert(parts[i].clone(), schema(&parts[..=i].join("."))?);
            }
            cursor = parent.get_mut(&parts[i]).unwrap();
        }
        let parsed = match cursor {
            Value::String(_) => Value::String(raw),
            _ => toml::from_str::<Value>(&format!("value = {raw}"))
                .with_context(|| format!("config {path}: invalid environment value"))?["value"]
                .clone(),
        };
        merge(cursor, parsed, &path, "env", &mut sources)?;
    }
    leaves(&value, "", &mut sources);
    let cfg: Config = value.try_into().context("config")?;
    ensure!(
        !cfg.pools.is_empty(),
        "config pools: at least one pool required"
    );
    for (name, pool) in &cfg.pools {
        ensure!(
            pool.slots > 0,
            "config pools.{name}.slots: must be positive"
        );
        duration(&pool.max_lease).with_context(|| format!("config pools.{name}.max_lease"))?;
        if let Some(p) = &pool.ports {
            ensure!(
                p.base >= 1024
                    && p.stride > 0
                    && p.probe > 0
                    && p.probe <= p.stride
                    && u64::from(p.base) + u64::from(p.stride) * u64::from(pool.slots) <= 65536,
                "config pools.{name}.ports: invalid block range"
            );
        }
        if let Some(l) = &pool.legacy_lock {
            ensure!(
                !l.path.is_empty() && ["shared", "exclusive"].contains(&l.mode.as_str()),
                "config pools.{name}.legacy_lock: invalid path or mode"
            );
        }
    }
    ensure!(
        cfg.admission.queue_poll_ms > 0,
        "config admission.queue_poll_ms: must be positive"
    );
    ensure!(
        cfg.watchdog.term_grace_seconds > 0.0,
        "config watchdog.term_grace_seconds: must be positive"
    );
    ensure!(
        cfg.watchdog.interval_ms > 0,
        "config watchdog.interval_ms: must be positive"
    );
    ensure!(
        cfg.watchdog.stop_available_samples > 0,
        "config watchdog.stop_available_samples: must be positive"
    );
    ensure!(
        cfg.watchdog.stop_psi_samples > 0,
        "config watchdog.stop_psi_samples: must be positive"
    );
    for (key, policy) in [
        ("watchdog.on_pressure", &cfg.watchdog.on_pressure),
        ("lease.on_expiry", &cfg.lease.on_expiry),
    ] {
        ensure!(
            ["stop", "warn", "off"].contains(&policy.as_str()),
            "config {key}: expected stop, warn or off"
        );
    }
    Ok((cfg, sources))
}
