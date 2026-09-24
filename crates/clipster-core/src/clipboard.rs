//! Handing content back to the system clipboard.
//!
//! Taking ownership of the X selection directly would mean the calling
//! process has to stay alive to serve it. Neither client can promise that:
//! the CLI is one-shot, and the picker exits the moment you press Enter.
//! Delegating to an external tool is the honest answer for both; having the
//! daemon own the selection is still the right long-term fix.
//!
//! X11 tools come first *even on a Wayland session*, because this build
//! captures through X11. Writing to the Wayland clipboard while watching the
//! X one means the copy lands somewhere the daemon cannot see, and the entry
//! does not move to the top of the history as the user expects. The ordering
//! becomes backend-aware in v0.3, when capture is too.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CLIPBOARD_TOOLS: &[(&str, &[&str])] = &[
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("wl-copy", &[]),
];

/// Put `content` on the clipboard, returning the tool that took it.
pub fn copy(content: &str) -> Result<&'static str> {
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
