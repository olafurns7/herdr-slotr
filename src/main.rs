#[cfg(target_os = "linux")]
mod admission;
mod config;
#[cfg(target_os = "linux")]
mod manager;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod state;
mod stats;
mod timestamp;
#[derive(Clone, Copy)]
#[repr(i32)]
enum ExitCode {
    Success = 0,
    Invalid = 2,
    Stopped = 75,
}
impl ExitCode {
    fn value(self) -> i32 {
        self as i32
    }
}
#[cfg(target_os = "linux")]
mod supervisor;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Run(Run),
    Status {
        #[arg(long)]
        json: bool,
    },
    Stop {
        run: String,
    },
    /// Reset a holder's idle clocks without renewing its lease.
    Touch {
        run: String,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(name = "_supervise", hide = true)]
    Supervise {
        run: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
}
#[derive(clap::Args)]
struct Run {
    #[arg(long, default_value = "default")]
    pool: String,
    #[arg(long)]
    campaign: String,
    #[arg(long)]
    purpose: String,
    #[arg(long, conflicts_with = "cost")]
    kind: Option<String>,
    #[arg(long)]
    cost: Option<u64>,
    #[arg(long)]
    lease: Option<String>,
    #[arg(long, default_value = "")]
    task: String,
    #[arg(long, default_value = "")]
    pane: String,
    #[arg(last = true, required = true)]
    cmd: Vec<String>,
}
#[derive(Subcommand)]
enum ConfigCommand {
    Show,
    Check { file: Option<PathBuf> },
}
fn execute(cli: Cli) -> Result<i32> {
    if let Command::Config { command } = cli.command {
        match command {
            ConfigCommand::Show => {
                let (cfg, sources) = config::load(None)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"config":cfg,"sources":sources})
                    )?
                );
            }
            ConfigCommand::Check { file } => {
                config::load(file.as_deref())?;
                println!("slotr: config OK");
            }
        }
        return Ok(ExitCode::Success.value());
    }
    let (cfg, _) = config::load(None)?;
    #[cfg(target_os = "linux")]
    {
        match cli.command {
            Command::Run(args) => runtime::run(args, &cfg),
            Command::Supervise { run, cmd } => supervisor::supervise(&run, &cmd, &cfg),
            Command::Stop { run } => runtime::stop(&run),
            Command::Touch { run } => {
                supervisor::touch(&run)?;
                Ok(ExitCode::Success.value())
            }
            Command::Status { json } => {
                runtime::status(&cfg, json)?;
                Ok(ExitCode::Success.value())
            }
            Command::Config { .. } => unreachable!(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        match cli.command {
            Command::Status { .. } => {
                println!(
                    "{}",
                    serde_json::json!({"stats":stats::read(),"pools":cfg.pools,"events_path":config::xdg("XDG_STATE_HOME",".local/state").join("slotr/events.jsonl")})
                );
                Ok(ExitCode::Success.value())
            }
            _ => anyhow::bail!("run, stop and touch require Linux and a systemd user bus"),
        }
    }
}
fn main() {
    let code = match execute(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("slotr: {e:#}");
            ExitCode::Invalid.value()
        }
    };
    std::process::exit(code);
}
