use serde::Serialize;
#[cfg(target_os = "linux")]
use std::{env, fs, path::PathBuf};
#[derive(Clone, Debug, Serialize, Default)]
pub struct Stats {
    pub available_mib: Option<f64>,
    pub psi_full_avg10: Option<f64>,
    pub psi_full_avg60: Option<f64>,
    pub load1: Option<f64>,
    pub cores: usize,
}
#[cfg(target_os = "linux")]
pub fn setting(key: &str, default: &str) -> String {
    if env::var("SLOTR_TEST").as_deref() == Ok("1") {
        env::var(format!("SLOTR_{key}")).unwrap_or_else(|_| default.into())
    } else {
        default.into()
    }
}
pub fn read() -> Stats {
    #[allow(unused_mut)]
    let mut out = Stats {
        cores: std::thread::available_parallelism().map_or(1, usize::from),
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    {
        let root = PathBuf::from(setting("PROC_ROOT", "/proc"));
        if let Ok(text) = fs::read_to_string(root.join("meminfo")) {
            out.available_mib = text
                .lines()
                .find_map(|l| {
                    l.strip_prefix("MemAvailable:")
                        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
                })
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| v / 1024.0);
        }
        if let Ok(text) = fs::read_to_string(root.join("pressure/memory"))
            && let Some(line) = text.lines().find(|l| l.starts_with("full "))
        {
            let value = |key: &str| {
                line.split_whitespace()
                    .find_map(|v| v.strip_prefix(key)?.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v >= 0.0)
            };
            out.psi_full_avg10 = value("avg10=");
            out.psi_full_avg60 = value("avg60=");
        }
        out.load1 = fs::read_to_string(root.join("loadavg"))
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0);
    }
    out
}
