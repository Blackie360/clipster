//! `clipster-ui` — the picker window.
//!
//! Bind it to a hotkey and it behaves like a menu: it opens centred and
//! focused, you filter and pick, it closes. Like the CLI it holds no state
//! of its own; everything comes from `clipsterd` over the same socket.

mod app;
mod filter;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use clipster_core::client::request;
use clipster_core::ipc::{Request, Response};
use eframe::egui;

const WINDOW_SIZE: [f32; 2] = [640.0, 420.0];

#[derive(Parser, Debug)]
#[command(name = "clipster-ui", version, about = "Picker window for clipster")]
struct Args {
    /// Maximum number of entries to load
    #[arg(short = 'n', long, default_value_t = 200)]
    limit: usize,

    /// Load every entry
    #[arg(short, long, conflicts_with = "limit")]
    all: bool,

    /// Only show pinned entries
    #[arg(short, long)]
    pinned: bool,

    /// Stay open when the window loses focus, instead of closing like a menu
    #[arg(long)]
    stay_open: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("clipster-ui: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let limit = if args.all { None } else { Some(args.limit) };

    // Fetch before opening the window, so an unreachable daemon is an error
    // on the terminal that launched us rather than an empty window with a
    // message in it that nobody bound a hotkey to read.
    let items = match request(Request::List { limit, pinned_only: args.pinned })? {
        Response::Items(items) => items,
        Response::Error { message } => bail!("{message}"),
        other => bail!("unexpected response from daemon: {other:?}"),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("clipster")
            // WM_CLASS / app_id, so window rules can float and centre it.
            .with_app_id("clipster")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([320.0, 200.0])
            // A picker is not a window you arrange: no title bar to drag, no
            // resize grip, and it stays above whatever you summoned it over.
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top(),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "clipster",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::Picker::new(cc, items, limit, args.pinned, !args.stay_open)))
        }),
    )
    .map_err(|e| anyhow!("could not open a window: {e}"))
}
