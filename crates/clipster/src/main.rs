//! `clipster` — thin CLI client. All state lives in the daemon; this binary
//! only marshals a request onto the socket and formats what comes back.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use clipster_core::client::request;
use clipster_core::ipc::{Item, Request, Response, Summary};
use std::io::Write;

#[derive(Parser, Debug)]
#[command(name = "clipster", version, about = "Fast clipboard history for Linux")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List history, newest first, pinned entries first
    List {
        /// Maximum number of entries to show
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
        /// Show every entry
        #[arg(short, long, conflicts_with = "limit")]
        all: bool,
        /// Only show pinned entries
        #[arg(short, long)]
        pinned: bool,
        #[arg(short, long, value_enum, default_value_t = Format::Human)]
        format: Format,
    },
    /// Print an entry's full content to stdout
    Get {
        /// Entry id, or omit for the most recent entry
        id: Option<i64>,
    },
    /// Put an entry back on the system clipboard
    Copy { id: Option<i64> },
    /// Pin an entry so it is never evicted
    Pin { id: i64 },
    /// Unpin an entry
    Unpin { id: i64 },
    /// Give a pinned entry a name (implies pin)
    Label {
        id: i64,
        /// The label text; omit with --remove to clear it
        text: Option<String>,
        /// Remove the existing label
        #[arg(long, conflicts_with = "text")]
        remove: bool,
    },
    /// Delete a single entry
    Rm { id: i64 },
    /// Delete history
    Clear {
        /// Also delete pinned entries
        #[arg(long)]
        all: bool,
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show daemon status
    Status,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Aligned, decorated, for reading
    Human,
    /// `id<TAB>preview`, for rofi/dmenu and shell pipelines
    Plain,
    /// One JSON array
    Json,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("clipster: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Cmd::List { limit, all, pinned, format } => {
            let limit = if all { None } else { Some(limit) };
            match request(Request::List { limit, pinned_only: pinned })? {
                Response::Items(items) => print_list(&items, format),
                other => unexpected(other)?,
            }
        }

        Cmd::Get { id } => {
            let item = fetch(id)?;
            // No trailing newline: the content is the content. Shell
            // substitution and pipes both depend on not inventing one.
            std::io::stdout().write_all(item.content.as_bytes())?;
        }

        Cmd::Copy { id } => {
            let item = fetch(id)?;
            let tool = clipster_core::clipboard::copy(&item.content)?;
            eprintln!("copied #{} via {}", item.id, tool);
        }

        Cmd::Pin { id } => affected(Request::SetPinned { id, pinned: true }, id, "pinned")?,
        Cmd::Unpin { id } => affected(Request::SetPinned { id, pinned: false }, id, "unpinned")?,

        Cmd::Label { id, text, remove } => {
            if text.is_none() && !remove {
                bail!("provide a label, or pass --remove to clear it");
            }
            let label = if remove { None } else { text };
            affected(Request::SetLabel { id, label }, id, "labelled")?;
        }

        Cmd::Rm { id } => affected(Request::Remove { id }, id, "removed")?,

        Cmd::Clear { all, yes } => {
            if !yes && !confirm(all)? {
                eprintln!("aborted");
                return Ok(());
            }
            match request(Request::Clear { keep_pinned: !all })? {
                Response::Affected(n) => eprintln!("cleared {n} entries"),
                other => unexpected(other)?,
            }
        }

        Cmd::Status => match request(Request::Status)? {
            Response::Status(s) => {
                println!("clipsterd    {} ({})", s.version, s.backend);
                println!("database     {}", s.db_path);
                println!("entries      {} ({} pinned)", s.total, s.pinned);
                println!("history_size {}", s.history_size);
                println!("captured     {} since daemon start", s.captured_since_start);
            }
            other => unexpected(other)?,
        },
    }
    Ok(())
}

/// `Get`/`Copy` share the "id or most recent" lookup.
fn fetch(id: Option<i64>) -> Result<Item> {
    let req = match id {
        Some(id) => Request::Get { id },
        None => Request::Latest,
    };
    match request(req)? {
        Response::Item(item) => Ok(item),
        Response::NotFound { .. } => match id {
            Some(id) => bail!("no entry with id {id}"),
            None => bail!("history is empty"),
        },
        other => unexpected(other).map(|_| unreachable!()),
    }
}

fn affected(req: Request, id: i64, verb: &str) -> Result<()> {
    match request(req)? {
        Response::Affected(0) => bail!("no entry with id {id}"),
        Response::Affected(_) => {
            eprintln!("{verb} #{id}");
            Ok(())
        }
        other => unexpected(other),
    }
}

fn unexpected(response: Response) -> Result<()> {
    match response {
        Response::Error { message } => bail!("{message}"),
        other => bail!("unexpected response from daemon: {other:?}"),
    }
}

fn confirm(all: bool) -> Result<bool> {
    let scope = if all { "ALL entries, including pinned" } else { "all unpinned entries" };
    eprint!("Delete {scope}? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn print_list(items: &[Summary], format: Format) {
    match format {
        Format::Json => {
            println!("{}", serde_json::to_string_pretty(items).unwrap_or_default());
        }
        Format::Plain => {
            for item in items {
                println!("{}\t{}", item.id, item.preview);
            }
        }
        Format::Human => {
            if items.is_empty() {
                eprintln!("history is empty");
                return;
            }
            let width = items.iter().map(|i| digits(i.id)).max().unwrap_or(1);
            let mut printed_separator = false;
            for (n, item) in items.iter().enumerate() {
                // Pinned entries sort first, so one transition marks the
                // boundary between the pinned section and the rest.
                if !item.pinned && n > 0 && items[n - 1].pinned && !printed_separator {
                    println!("{:-<width$}--+", "", width = width);
                    printed_separator = true;
                }
                let mark = if item.pinned { '*' } else { ' ' };
                match &item.label {
                    Some(label) => {
                        println!("{:>width$} {mark} [{label}] {}", item.id, item.preview)
                    }
                    None => println!("{:>width$} {mark} {}", item.id, item.preview),
                }
            }
        }
    }
}

fn digits(n: i64) -> usize {
    n.abs().to_string().len()
}
