//! `clipster` — thin CLI client. All state lives in the daemon; this binary
//! only marshals a request onto the socket and formats what comes back.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use clipster_core::ipc::{Item, Request, Response, Summary};
use clipster_core::paths;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
            let tool = to_clipboard(&item.content)?;
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

/// Send one request, read one response.
fn request(request: Request) -> Result<Response> {
    let path = paths::socket_path();
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot reach clipsterd at {}\n       start it with: systemctl --user start clipsterd",
            path.display()
        )
    })?;

    let mut writer = stream.try_clone()?;
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response)? == 0 {
        bail!("clipsterd closed the connection without responding");
    }
    Ok(serde_json::from_str(&response)?)
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

/// Hand content to an external clipboard tool.
///
/// Taking ownership of the X selection directly would mean this process has
/// to stay alive to serve it, which a one-shot CLI cannot do. Delegating is
/// the honest MVP answer; the v0.2 picker will have the daemon own it.
///
/// X11 tools come first *even on a Wayland session*, because this build
/// captures through X11. Writing to the Wayland clipboard while watching the
/// X one means `clipster copy` lands somewhere the daemon cannot see, and the
/// entry does not move to the top of the history as the user expects. The
/// ordering becomes backend-aware in v0.3, when capture is too.
const CLIPBOARD_TOOLS: &[(&str, &[&str])] = &[
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("wl-copy", &[]),
];

fn to_clipboard(content: &str) -> Result<&'static str> {
    let mut last_failure = None;

    for (tool, args) in CLIPBOARD_TOOLS {
        let spawned = Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            // Not installed; try the next one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("spawning {tool}")),
        };

        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(content.as_bytes())
            .with_context(|| format!("writing to {tool}"))?;

        // These tools fork and keep a child alive to serve the selection, so
        // the parent exiting is expected. What is *not* expected is exiting
        // non-zero — that means the content went nowhere, and reporting
        // success there is worse than failing, because the user walks away
        // believing the clipboard was set.
        match settled_failure(&mut child) {
            None => return Ok(tool),
            Some(status) => last_failure = Some(format!("{tool} exited with {status}")),
        }
    }

    match last_failure {
        Some(reason) => bail!("no clipboard tool succeeded ({reason})"),
        None => bail!("no clipboard tool found; install one of xclip, xsel or wl-clipboard"),
    }
}

/// Give a just-spawned tool a moment to fail, and report the status if it
/// does. Still running after the grace period counts as success: that is the
/// normal case, where it has forked to serve the selection.
fn settled_failure(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    const GRACE: Duration = Duration::from_millis(150);
    const STEP: Duration = Duration::from_millis(10);

    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return None,
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(STEP),
            Err(_) => return None,
        }
    }
    None
}
