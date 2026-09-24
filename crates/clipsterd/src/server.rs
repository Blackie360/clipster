//! Unix-socket IPC server: one request line in, one response line out.

use crate::Shared;
use anyhow::{bail, Context, Result};
use clipster_core::ipc::{Request, Response, Status};
use clipster_core::paths;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Bind the IPC socket, refusing to start if another daemon already holds it.
pub fn bind() -> Result<UnixListener> {
    let path = paths::socket_path();
    let dir = path.parent().expect("socket path always has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    // A socket file left behind by a killed daemon would block bind() forever.
    // Distinguish "stale" from "live" by trying to connect: only a socket that
    // refuses connections is safe to unlink.
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => bail!(
                "another clipsterd is already listening on {}",
                path.display()
            ),
            Err(_) => {
                log::warn!("removing stale socket at {}", path.display());
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing stale socket {}", path.display()))?;
            }
        }
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding socket at {}", path.display()))?;
    // Clipboard history is private to the user; keep the socket owner-only.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting permissions on {}", path.display()))?;

    log::info!("listening on {}", path.display());
    Ok(listener)
}

pub fn serve(listener: UnixListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = Arc::clone(&shared);
                // A thread per connection: connections are short-lived
                // request/response pairs, so a pool would be more machinery
                // than the load justifies.
                std::thread::spawn(move || {
                    if let Err(e) = handle(stream, &shared) {
                        log::debug!("client connection ended: {e:#}");
                    }
                });
            }
            Err(e) => log::warn!("accept failed: {e}"),
        }
    }
}

fn handle(stream: UnixStream, shared: &Arc<Shared>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(()); // client hung up without sending anything
    }

    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => dispatch(request, shared),
        Err(e) => Response::Error { message: format!("malformed request: {e}") },
    };

    let mut out = stream;
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    match out.write_all(&encoded) {
        // The client may have walked away mid-response; not our problem.
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

fn dispatch(request: Request, shared: &Arc<Shared>) -> Response {
    match run(request, shared) {
        Ok(response) => response,
        Err(e) => Response::Error { message: format!("{e:#}") },
    }
}

fn run(request: Request, shared: &Arc<Shared>) -> Result<Response> {
    let store = shared
        .store
        .lock()
        .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;

    Ok(match request {
        Request::List { limit, pinned_only } => Response::Items(store.list(limit, pinned_only)?),

        Request::Get { id } => match store.get(id)? {
            Some(item) => Response::Item(item),
            None => Response::NotFound { id },
        },

        Request::Latest => match store.latest()? {
            Some(item) => Response::Item(item),
            None => Response::NotFound { id: 0 },
        },

        Request::SetPinned { id, pinned } => Response::Affected(store.set_pinned(id, pinned)?),

        Request::SetLabel { id, label } => {
            Response::Affected(store.set_label(id, label.as_deref())?)
        }

        Request::Remove { id } => Response::Affected(store.remove(id)?),

        Request::Clear { keep_pinned } => Response::Affected(store.clear(keep_pinned)?),

        Request::Status => {
            let (total, pinned) = store.counts()?;
            let history_size = {
                let mut watcher = shared
                    .config
                    .lock()
                    .map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
                watcher.current().history_size
            };
            Response::Status(Status {
                version: env!("CARGO_PKG_VERSION").to_string(),
                backend: shared.backend.clone(),
                total,
                pinned,
                history_size,
                captured_since_start: shared.captured.load(Ordering::Relaxed),
                db_path: shared.db_path.display().to_string(),
            })
        }
    })
}

/// Best-effort socket cleanup on shutdown.
pub fn unlink(path: &Path) {
    let _ = std::fs::remove_file(path);
}
