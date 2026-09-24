//! X11 clipboard capture via the XFixes selection-notify extension.
//!
//! The daemon owns a 1x1 never-mapped window purely as a target for selection
//! transfers and property events. XFixes tells us *that* the CLIPBOARD owner
//! changed; retrieving the content is then an ordinary ICCCM selection
//! transfer, including the INCR chunking protocol for large payloads.

use crate::Shared;
use anyhow::{bail, Context, Result};
use clipster_core::store::Inserted;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_DEPTH_FROM_PARENT;

/// 32-bit words fetched per `GetProperty` round trip (256 KiB).
const CHUNK_LONGS: u32 = 1 << 16;
/// How long to wait for a selection owner to respond before giving up.
///
/// A bounded wait matters: a wedged or slow-to-respond clipboard owner must
/// not be able to hang the capture loop, which would silently stop recording
/// everything else.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(2);

struct Atoms {
    clipboard: Atom,
    utf8_string: Atom,
    incr: Atom,
    dest: Atom,
    net_active_window: Atom,
}

pub struct Capture {
    conn: RustConnection,
    window: Window,
    root: Window,
    atoms: Atoms,
    /// Events pulled off the wire while waiting for a specific reply. They are
    /// replayed by the main loop so a clipboard change that lands mid-transfer
    /// is not dropped.
    pending: VecDeque<Event>,
}

fn intern(conn: &RustConnection, name: &[u8]) -> Result<Atom> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

impl Capture {
    pub fn connect() -> Result<Self> {
        let (conn, screen_num) =
            x11rb::connect(None).context("connecting to the X server (is DISPLAY set?)")?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let root_visual = screen.root_visual;

        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            root_visual,
            // PROPERTY_CHANGE is what makes INCR transfers observable.
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        // Intentionally never mapped: it exists to hold properties, not pixels.

        let atoms = Atoms {
            clipboard: intern(&conn, b"CLIPBOARD")?,
            utf8_string: intern(&conn, b"UTF8_STRING")?,
            incr: intern(&conn, b"INCR")?,
            dest: intern(&conn, b"CLIPSTER_RECV")?,
            net_active_window: intern(&conn, b"_NET_ACTIVE_WINDOW")?,
        };

        // XFixes requires version negotiation before any of its requests work.
        conn.xfixes_query_version(5, 0)?
            .reply()
            .context("XFixes extension unavailable; clipster needs it to observe the clipboard")?;
        conn.xfixes_select_selection_input(
            window,
            atoms.clipboard,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER
                | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        conn.flush()?;

        Ok(Self { conn, window, root, atoms, pending: VecDeque::new() })
    }

    pub fn run(&mut self, shared: &Arc<Shared>) -> Result<()> {
        // Capture what is already on the clipboard, so a daemon restart
        // mid-session does not leave a hole in the history.
        self.capture(shared);

        loop {
            let event = match self.pending.pop_front() {
                Some(event) => event,
                // Blocking wait; this is what holds idle CPU at zero.
                None => self.conn.wait_for_event().context("X11 connection lost")?,
            };
            match event {
                Event::XfixesSelectionNotify(e) if e.selection == self.atoms.clipboard => {
                    self.capture(shared)
                }
                Event::Error(e) => log::debug!("X11 error: {e:?}"),
                _ => {}
            }
        }
    }

    fn capture(&mut self, shared: &Arc<Shared>) {
        if let Err(e) = self.try_capture(shared) {
            // A failed transfer is not fatal; the next copy gets another shot.
            log::warn!("clipboard capture failed: {e:#}");
        }
    }

    fn try_capture(&mut self, shared: &Arc<Shared>) -> Result<()> {
        // Snapshot the config and drop the lock before touching the store, so
        // the two locks are never held simultaneously.
        let config = {
            let mut watcher = shared
                .config
                .lock()
                .map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
            watcher.current().clone()
        };

        if let Some(class) = self.source_class() {
            if config.is_denied(&class) {
                log::info!("skipping clipboard owned by denylisted app: {class}");
                return Ok(());
            }
        }

        let Some(text) = self.fetch_text()? else {
            // No UTF8_STRING form: an image, a file list, or an empty
            // clipboard. Images and file URIs are v0.3 scope.
            return Ok(());
        };

        if config.ignore_whitespace_only && text.trim().is_empty() {
            return Ok(());
        }
        if text.len() > config.max_item_bytes {
            log::info!(
                "skipping {} byte entry (max_item_bytes = {})",
                text.len(),
                config.max_item_bytes
            );
            return Ok(());
        }

        let store = shared
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
        match store.insert(&text)? {
            Inserted::New(id) => {
                log::info!("captured #{id} ({} bytes)", text.len());
                shared.captured.fetch_add(1, Ordering::Relaxed);
            }
            Inserted::Bumped(id) => log::debug!("bumped #{id} to most recent"),
        }
        let evicted = store.evict(config.history_size)?;
        if evicted > 0 {
            log::debug!("evicted {evicted} entries over history_size");
        }
        Ok(())
    }

    /// Ask the current owner for the clipboard as UTF-8 text.
    ///
    /// `Ok(None)` means "nothing text-shaped to store" — a refusal or a
    /// timeout — as distinct from `Err`, which means the transfer broke.
    fn fetch_text(&mut self) -> Result<Option<String>> {
        let (window, dest) = (self.window, self.atoms.dest);

        self.conn.delete_property(window, dest)?;
        self.conn.convert_selection(
            window,
            self.atoms.clipboard,
            self.atoms.utf8_string,
            dest,
            x11rb::CURRENT_TIME,
        )?;
        self.conn.flush()?;

        let notify = self.await_event(TRANSFER_TIMEOUT, move |ev| {
            matches!(ev, Event::SelectionNotify(e) if e.requestor == window)
        })?;

        let Some(Event::SelectionNotify(notify)) = notify else {
            log::debug!("no SelectionNotify within {TRANSFER_TIMEOUT:?}");
            return Ok(None);
        };
        if notify.property == x11rb::NONE {
            // Owner cannot provide UTF8_STRING.
            return Ok(None);
        }

        let (kind, bytes) = self.read_property()?;
        if kind == self.atoms.incr {
            // Deleting the property is the signal that we are ready for the
            // first chunk.
            self.conn.delete_property(window, dest)?;
            self.conn.flush()?;
            return self.read_incr().map(Some);
        }
        self.conn.delete_property(window, dest)?;
        self.conn.flush()?;

        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Read our destination property in full, paging over `bytes_after`.
    /// Returns the property type alongside the bytes so the caller can detect
    /// an INCR handshake.
    fn read_property(&self) -> Result<(Atom, Vec<u8>)> {
        let mut buf = Vec::new();
        let mut offset = 0u32;
        // Always assigned on the first iteration, before any `break`.
        let mut kind;

        loop {
            let reply = self
                .conn
                .get_property(false, self.window, self.atoms.dest, AtomEnum::ANY, offset, CHUNK_LONGS)?
                .reply()?;
            kind = reply.type_;
            if kind == x11rb::NONE || reply.value.is_empty() {
                break;
            }
            offset += (reply.value.len() / 4) as u32;
            buf.extend_from_slice(&reply.value);
            if reply.bytes_after == 0 {
                break;
            }
        }
        Ok((kind, buf))
    }

    /// Drive an INCR transfer to completion.
    ///
    /// The owner appends each chunk to our property; we read it, delete it to
    /// acknowledge, and repeat until a zero-length chunk arrives.
    fn read_incr(&mut self) -> Result<String> {
        let (window, dest) = (self.window, self.atoms.dest);
        let mut buf: Vec<u8> = Vec::new();

        loop {
            let arrived = self.await_event(TRANSFER_TIMEOUT, move |ev| {
                matches!(ev, Event::PropertyNotify(e)
                    if e.window == window && e.atom == dest && e.state == Property::NEW_VALUE)
            })?;
            if arrived.is_none() {
                bail!("INCR transfer stalled after {} bytes", buf.len());
            }

            let (_, chunk) = self.read_property()?;
            self.conn.delete_property(window, dest)?;
            self.conn.flush()?;

            if chunk.is_empty() {
                break;
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Wait for an event matching `pred`, queueing everything else for the
    /// main loop. Returns `None` on timeout.
    fn await_event<F>(&mut self, timeout: Duration, pred: F) -> Result<Option<Event>>
    where
        F: Fn(&Event) -> bool,
    {
        if let Some(pos) = self.pending.iter().position(&pred) {
            return Ok(self.pending.remove(pos));
        }

        let deadline = Instant::now() + timeout;
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                if pred(&event) {
                    return Ok(Some(event));
                }
                self.pending.push_back(event);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            // Polling rather than blocking keeps the timeout enforceable. This
            // only runs during an in-flight transfer, so idle CPU is untouched.
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// WM_CLASS of the application the clipboard content came from, used for
    /// denylist matching.
    ///
    /// Two strategies, because neither alone is sufficient:
    ///
    /// 1. The selection owner, which is exact when it works. It frequently
    ///    does not: many clients own the selection from an unmapped window
    ///    with no WM_CLASS anywhere up its ancestry (`xclip` does exactly
    ///    this), leaving nothing to match.
    /// 2. The focused window, via `_NET_ACTIVE_WINDOW`. A heuristic — it
    ///    assumes whoever has focus is who copied — but it resolves to a real
    ///    toplevel with a real class.
    ///
    /// Returning `None` means "unattributable", and an unattributable copy is
    /// stored. That is the permissive direction, so the log line below is the
    /// only way a user can tell why their denylist entry is not firing.
    fn source_class(&self) -> Option<String> {
        let class = self
            .selection_owner()
            .and_then(|owner| self.class_of(owner))
            .or_else(|| self.active_window_class());

        match &class {
            Some(class) => log::debug!("clipboard source class: {class}"),
            None => log::debug!("clipboard source class: unattributable"),
        }
        class
    }

    fn selection_owner(&self) -> Option<Window> {
        let owner = self
            .conn
            .get_selection_owner(self.atoms.clipboard)
            .ok()?
            .reply()
            .ok()?
            .owner;
        (owner != x11rb::NONE).then_some(owner)
    }

    fn active_window_class(&self) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, self.root, self.atoms.net_active_window, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let active = reply.value32()?.next()?;
        (active != x11rb::NONE).then_some(active).and_then(|w| self.class_of(w))
    }

    /// WM_CLASS of `window`, or of its nearest ancestor that has one.
    fn class_of(&self, window: Window) -> Option<String> {
        let mut window = window;
        for _ in 0..16 {
            if let Some(class) = self.wm_class(window) {
                return Some(class);
            }
            let tree = self.conn.query_tree(window).ok()?.reply().ok()?;
            if tree.parent == x11rb::NONE || tree.parent == tree.root {
                return None;
            }
            window = tree.parent;
        }
        None
    }

    fn wm_class(&self, window: Window) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        // WM_CLASS is "instance\0class\0"; keep both halves so a denylist
        // entry can match whichever the user knows.
        let raw = String::from_utf8_lossy(&reply.value);
        let parts: Vec<&str> = raw.split('\0').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("."))
        }
    }
}
