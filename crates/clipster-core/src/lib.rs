//! Shared building blocks for `clipsterd`, the `clipster` CLI and the
//! `clipster-ui` picker.

pub mod clipboard;
pub mod client;
pub mod config;
pub mod ipc;
pub mod paths;
pub mod store;

pub use client::request;
pub use config::Config;
pub use ipc::{Item, Request, Response, Status, Summary};
pub use store::Store;

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
///
/// Millisecond resolution is not cosmetic: recency ordering is the product's
/// core promise, and at second resolution two entries touched within the same
/// second tie, so re-copying an old entry visibly fails to move it to the top.
/// Clock skew before 1970 is not worth carrying through the type system, so
/// it saturates at 0.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Collapse an entry to a single line for list views.
///
/// Clipboard content is routinely multi-line (code, logs), and every consumer
/// of a list — the human formatter, rofi, dmenu — needs one row per item.
pub fn preview(content: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars.min(content.len()) + 1);
    let mut chars = 0;
    let mut pending_space = false;

    for c in content.chars() {
        if c.is_whitespace() {
            // Leading whitespace is dropped entirely; interior runs collapse.
            pending_space = !out.is_empty();
            continue;
        }
        // Account for the pending space before committing to it, so
        // truncating at a word gap does not leave a dangling " …".
        let extra = usize::from(pending_space);
        if chars + extra + 1 > max_chars {
            out.push('…');
            return out;
        }
        if pending_space {
            out.push(' ');
            chars += 1;
            pending_space = false;
        }
        out.push(c);
        chars += 1;
    }
    out
}

/// Content hash used for deduplication. Stable across rustc versions, which
/// `DefaultHasher` is explicitly not — and these values are persisted.
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(s, "{byte:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_collapses_whitespace_to_one_line() {
        assert_eq!(preview("  fn main() {\n    todo!()\n}", 100), "fn main() { todo!() }");
    }

    #[test]
    fn preview_truncates_on_char_boundaries() {
        // Naive byte slicing would panic here.
        assert_eq!(preview("héllo wörld", 5), "héllo…");
    }

    #[test]
    fn preview_leaves_short_content_untouched() {
        assert_eq!(preview("short", 100), "short");
    }

    #[test]
    fn preview_does_not_ellipsize_an_exact_fit() {
        assert_eq!(preview("abcde", 5), "abcde");
        assert_eq!(preview("abcdef", 5), "abcde…");
    }

    #[test]
    fn preview_never_emits_a_space_before_the_ellipsis() {
        assert!(!preview("aaaa bbbb", 5).contains(" …"));
    }

    #[test]
    fn hash_is_stable_and_distinguishing() {
        assert_eq!(content_hash("a"), content_hash("a"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }
}
