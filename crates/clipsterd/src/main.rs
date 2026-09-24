//! `clipsterd` — the clipboard capture daemon.
//!
//! Two threads: an X11 event loop that blocks on the display connection, and
//! an IPC accept loop serving the `clipster` CLI over a Unix socket. State is
//! a single SQLite connection behind a mutex; at human clipboard rates there
//! is nothing to be gained from a connection pool.

mod server;
mod x11;

use anyhow::{Context, Result};
use clap::Parser;
use clipster_core::config::ConfigWatcher;
use clipster_core::{paths, Store};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// State shared between the capture loop and the IPC server.
pub struct Shared {
    pub store: Mutex<Store>,
    pub config: Mutex<ConfigWatcher>,
    pub captured: AtomicU64,
    pub db_path: PathBuf,
    pub backend: String,
}

#[derive(Parser, Debug)]
#[command(name = "clipsterd", version, about = "clipster clipboard capture daemon")]
struct Args {
    /// Config file to use instead of ~/.config/clipster/config.toml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// History database to use instead of ~/.local/share/clipster/history.db
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Increase log verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("clipsterd: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    let default_level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .format_timestamp_secs()
        .init();

    check_session();

    let db_path = args.db.unwrap_or_else(paths::db_path);
    let config_path = args.config.unwrap_or_else(paths::config_path);

    let store = Store::open(&db_path)?;
    let config = ConfigWatcher::new(config_path.clone());

    log::info!("clipsterd {} starting", env!("CARGO_PKG_VERSION"));
    log::info!("database: {}", db_path.display());
    log::info!("config:   {}", config_path.display());

    let shared = Arc::new(Shared {
        store: Mutex::new(store),
        config: Mutex::new(config),
        captured: AtomicU64::new(0),
        db_path,
        backend: "x11".to_string(),
    });

    // Bind before connecting to X: if another daemon is running, fail fast
    // rather than racing it for clipboard transfers.
    let listener = server::bind()?;
    let mut capture = x11::Capture::connect()?;

    {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("ipc".into())
            .spawn(move || server::serve(listener, shared))
            .context("spawning IPC server thread")?;
    }

    // Runs until the X connection drops, at which point exiting non-zero lets
    // the systemd unit restart us against the new server.
    let result = capture.run(&shared);
    server::unlink(&paths::socket_path());
    result
}

/// Warn about session types this MVP does not fully cover.
///
/// Silently capturing nothing is the worst possible failure mode for a
/// clipboard manager, so be loud about it at startup.
fn check_session() {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let has_display = std::env::var_os("DISPLAY").is_some();

    if session == "wayland" {
        if has_display {
            log::warn!(
                "Wayland session detected: this build captures via XWayland only. \
                 Copies from native Wayland clients may not be seen. \
                 Native support (wlr-data-control) is planned for v0.3."
            );
        } else {
            log::error!(
                "Wayland session with no DISPLAY: there is no X server to watch. \
                 Native Wayland support is planned for v0.3."
            );
        }
    }
}
