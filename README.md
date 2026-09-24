# clipster

Fast, keyboard-driven clipboard history for Linux. A background daemon
captures what you copy; a thin CLI gets it back.

**Status: MVP (v0.1).** X11 text capture, CLI, and a rofi/dmenu picker script.
See [Scope](#what-this-version-does-and-does-not-do) before installing — the
limits are real and worth reading first.

## What this version does and does not do

| | |
|---|---|
| ✅ | X11 text capture, including large payloads via INCR |
| ✅ | SQLite history, deduplicated, with recency ordering |
| ✅ | Pinning, named snippets, per-app denylist |
| ✅ | CLI: `list` `get` `copy` `pin` `unpin` `label` `rm` `clear` `status` |
| ✅ | rofi / fuzzel / dmenu picker script |
| ❌ | Native Wayland capture — X11 only, see [below](#wayland) |
| ❌ | Images and file URIs — text only (v0.3) |
| ❌ | Native GUI picker, global hotkey, fuzzy search (v0.2) |
| ❌ | Encryption at rest (v1.0) — **history is a plaintext SQLite file** |

### Wayland

This release captures through **XWayland only**. Under a Wayland session it
will see copies from X11 apps, and will silently miss copies from native
Wayland clients. The daemon warns about this at startup rather than appearing
to work. Native support via `wlr-data-control` is v0.3.

### Privacy

Captured content is stored **unencrypted** at
`~/.local/share/clipster/history.db`. The denylist in
[`contrib/config.example.toml`](contrib/config.example.toml) ships non-empty
and covers the common password managers, because a clipboard manager that
records your vault on first run cannot un-record it afterwards. Check that
your own tools are covered before running the daemon:

```sh
xprop WM_CLASS      # click a window to read its class
```

`clipster clear` deletes rows and `VACUUM`s the file, but that is not a
guarantee against forensic recovery on the underlying disk.

The denylist is a best-effort filter, not a security boundary — see
[the X11 notes](#notes-on-the-x11-path) for why attribution can fail. If
something must never be captured, do not copy it while the daemon is running.

## Install

Requires a Rust toolchain and a C compiler (SQLite is built from source).

```sh
cargo build --release
install -Dm755 target/release/clipsterd  ~/.local/bin/clipsterd
install -Dm755 target/release/clipster   ~/.local/bin/clipster
install -Dm755 contrib/clipster-rofi.sh  ~/.local/bin/clipster-rofi
install -Dm644 contrib/clipsterd.service ~/.config/systemd/user/clipsterd.service
install -Dm644 contrib/config.example.toml ~/.config/clipster/config.toml

systemctl --user daemon-reload
systemctl --user enable --now clipsterd
```

Check it came up:

```sh
clipster status
journalctl --user -u clipsterd -f
```

To run it in the foreground instead, just `clipsterd -v`.

## Usage

```sh
clipster list                  # newest first, pinned entries on top
clipster list -n 10            # limit
clipster list --pinned         # only pinned
clipster list --format plain   # id<TAB>preview, for scripts
clipster list --format json    # full records

clipster get 42                # full content to stdout, no trailing newline
clipster get                   # most recent entry
clipster copy 42               # put it back on the system clipboard

clipster pin 42
clipster label 42 "commit template"   # implies pin
clipster unpin 42
clipster rm 42

clipster clear                 # unpinned only, prompts
clipster clear --all -y        # everything, no prompt
```

Because `get` emits no trailing newline, it composes cleanly:

```sh
clipster get 42 | wc -c
ssh host "$(clipster get 42)"
```

### Picker hotkey

Bind `clipster-rofi` to a key. It lists history through rofi (or fuzzel, or
dmenu) and copies your selection back.

```
# i3 / sway
bindsym $mod+v exec --no-startup-id ~/.local/bin/clipster-rofi

# Hyprland
bind = SUPER, V, exec, ~/.local/bin/clipster-rofi
```

A native picker with built-in fuzzy matching replaces this in v0.2.

## Configuration

`~/.config/clipster/config.toml`, hot-reloaded on change — see
[`contrib/config.example.toml`](contrib/config.example.toml) for the full set
of keys with defaults. An unrecognised key is an error, logged by the daemon,
rather than a setting that silently does nothing.

## Architecture

```
┌──────────────┐   XFixes selection-notify   ┌───────────┐
│  X server    │ ──────────────────────────► │           │
└──────────────┘   ICCCM transfer + INCR     │ clipsterd │
                                             │           │
┌──────────────┐   JSON lines / Unix socket  │           │
│ clipster CLI │ ◄─────────────────────────► │           │
└──────────────┘                             └─────┬─────┘
                                                   │
                                          ┌────────▼────────┐
                                          │ SQLite (WAL)    │
                                          │ history.db      │
                                          └─────────────────┘
```

- **`clipsterd`** — two threads. One blocks on the X11 connection (which is
  what keeps idle CPU at zero); one accepts IPC connections. State is a single
  SQLite connection behind a mutex, which is ample at human clipboard rates.
- **`clipster`** — holds no state. Marshals a request, formats the response.
- **IPC** — newline-delimited JSON at
  `$XDG_RUNTIME_DIR/clipster/clipsterd.sock`, mode `0600`. Serialized JSON
  never contains a raw newline, so the framing survives multi-line content.
  Inspectable with `socat`; a binary protocol can come later if the wire ever
  shows up in a profile.
- **Storage** — SQLite in WAL mode. WAL is what backs the PRD's "no data loss
  on crash": a killed daemon recovers on next open.

### Notes on the X11 path

- The daemon owns a 1×1 never-mapped window purely to receive property events
  and selection transfers.
- Large payloads arrive via the INCR protocol, driven to completion in
  [`x11.rs`](crates/clipsterd/src/x11.rs).
- Every transfer is bounded by a 2s timeout. A wedged clipboard owner must not
  be able to hang the capture loop, because that would silently stop recording
  everything else.
- Denylist attribution is best-effort by nature. Selection owners are often
  unmapped windows with no `WM_CLASS` anywhere up their ancestry (`xclip` is
  one), so when the owner cannot be identified clipster falls back to the
  focused window via `_NET_ACTIVE_WINDOW`. That is a heuristic — it assumes
  whoever has focus is who copied. Run `clipsterd -vv` to see the class it
  resolved for each copy. An unattributable copy **is** stored, so do not
  treat the denylist as a hard guarantee.

## Development

```sh
cargo test          # 22 tests, storage + config + preview logic
cargo build         # debug
cargo build --release
```

The X11 layer has no automated coverage — it needs a live X server. It is
exercised by hand against `xclip`; an Xvfb-based integration test is the
obvious next addition.

## Roadmap

| Phase | Scope |
|---|---|
| **v0.1** | **X11 text capture, CLI, rofi integration** ← you are here |
| v0.2 | Native picker (egui/iced), global hotkey, fuzzy search |
| v0.3 | Wayland via `wlr-data-control`, images, file URIs |
| v0.4 | Pinning UX polish, richer snippet management |
| v1.0 | Encryption at rest, auto-clear, packaging (AUR, deb, Nix) |

## License

MIT
