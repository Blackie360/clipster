# clipster

Fast, keyboard-driven clipboard history for Linux. A background daemon
captures what you copy; a thin CLI gets it back.

**Status: MVP (v0.1).** X11 text capture, CLI, a picker window, and a
rofi/dmenu picker script.
See [Scope](#what-this-version-does-and-does-not-do) before installing — the
limits are real and worth reading first.

## What this version does and does not do

| | |
|---|---|
| ✅ | X11 text capture, including large payloads via INCR |
| ✅ | SQLite history, deduplicated, with recency ordering |
| ✅ | Pinning, named snippets, per-app denylist |
| ✅ | CLI: `list` `get` `copy` `pin` `unpin` `label` `rm` `clear` `status` |
| ✅ | `clipster-ui` picker window, with fuzzy search |
| ✅ | rofi / fuzzel / dmenu picker script |
| ❌ | Native Wayland capture — X11 only, see [below](#wayland) |
| ❌ | Images and file URIs — text only (v0.3) |
| ❌ | Global hotkey — bind the picker in your WM, clipster grabs no keys |
| ❌ | Paste-on-select — the picker copies, you paste (v0.2) |
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

### Debian / Ubuntu

```sh
gh release download v0.1.0 --pattern '*_amd64.deb'   # or *_arm64.deb
sudo dpkg -i clipster_0.1.0_amd64.deb
```

### Any distro (prebuilt static binary)

The release tarballs are statically linked against musl, so they carry no
libc dependency and run on any x86_64 or aarch64 Linux.

```sh
gh release download v0.1.0 --pattern '*x86_64-unknown-linux-musl.tar.gz'
tar xzf clipster-0.1.0-x86_64-unknown-linux-musl.tar.gz
cd clipster-0.1.0-x86_64-unknown-linux-musl
install -Dm755 clipster clipsterd     -t ~/.local/bin/
install -Dm755 clipster-rofi.sh        ~/.local/bin/clipster-rofi
install -Dm644 clipsterd.service       ~/.config/systemd/user/clipsterd.service
install -Dm644 config.example.toml     ~/.config/clipster/config.toml
```

While the repository is private, downloads need an authenticated `gh`. Once
it is public, plain `curl -LO` on the release URL works too.

### From source

Requires a Rust toolchain and a C compiler (SQLite is built from source).
The picker additionally needs OpenGL and the usual X11/Wayland client
libraries at runtime; it is dlopened, so there is nothing extra to install
at build time on a normal desktop.

```sh
cargo build --release
install -Dm755 target/release/clipsterd  ~/.local/bin/clipsterd
install -Dm755 target/release/clipster   ~/.local/bin/clipster
install -Dm755 target/release/clipster-ui ~/.local/bin/clipster-ui
install -Dm755 contrib/clipster-rofi.sh  ~/.local/bin/clipster-rofi
install -Dm644 contrib/clipsterd.service ~/.config/systemd/user/clipsterd.service
install -Dm644 contrib/config.example.toml ~/.config/clipster/config.toml
```

To build your own `.deb` from a source checkout:

```sh
cargo build --release
packaging/build-deb.sh            # writes dist/clipster_<version>_<arch>.deb
```

### Enable the daemon

clipster runs as a systemd **user** service — enable it as yourself, not as
root. (The `.deb` installs the unit system-wide but cannot enable it for you.)

```sh
systemctl --user daemon-reload
systemctl --user enable --now clipsterd
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

### Picker window

`clipster-ui` is a single-screen picker: a filter box, the history, a hint
bar. It opens centred and focused, and closes as soon as you pick something
or click away — like a menu, not a window you manage.

```sh
clipster-ui                    # 200 most recent, pinned first
clipster-ui -n 50              # fewer
clipster-ui --all              # everything
clipster-ui --pinned           # snippets only
clipster-ui --stay-open        # survive losing focus, for debugging
```

| Key | |
|---|---|
| type | fuzzy filter over previews and labels |
| `↑` `↓`, `ctrl+k` `ctrl+j`, `ctrl+n` | move |
| `enter`, click | copy and close |
| `ctrl+p` | pin / unpin |
| `ctrl+d` | delete |
| `ctrl+u` | clear the filter |
| `esc` | close |

It copies to the clipboard; it does not paste for you. Synthesising a paste
into whichever window had focus needs `xdotool`-style key injection, which
is a different can of worms — for now, `enter` then `ctrl+v`.

Bind it to a key:

```
# i3 / sway
bindsym $mod+v exec --no-startup-id ~/.local/bin/clipster-ui

# Hyprland
bind = SUPER, V, exec, ~/.local/bin/clipster-ui
```

The window asks to be undecorated, centred and above other windows. Every
one of those is a request a compositor may refuse — always-on-top in
particular is unavailable on Wayland, where a window cannot raise itself. If
yours places it badly, a rule keyed on the `clipster` app id / `WM_CLASS`
will fix it.

Launching costs a window and a GPU context, so budget roughly 100ms rather
than the <50ms the CLI path hits. If that matters more than the window does,
the rofi script below stays supported.

### Picker hotkey via rofi

`clipster-rofi` does the same job through rofi (or fuzzel, or dmenu), with no
GUI toolkit involved. Bind it exactly the same way.

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
├──────────────┤                             │           │
│  clipster-ui │ ◄─────────────────────────► │           │
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
- **`clipster-ui`** — egui, and equally stateless: it holds a query, a
  selection, and the last list the daemon gave it. Every pin, delete and copy
  is the same IPC call the CLI makes, through the same
  [`client`](crates/clipster-core/src/client.rs) and
  [`clipboard`](crates/clipster-core/src/clipboard.rs) code in core. It does
  not poll, so entries copied while it is open appear the next time you open
  it.
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
cargo test          # 32 tests, storage + config + preview + filter logic
cargo build         # debug
cargo build --release
cargo build -p clipster --release   # CLI and daemon only, no GUI toolkit
```

`clipster-ui` pins `eframe` to 0.31. egui renames things between minor
versions (`Margin`, `Frame::none`, `rounding`), so treat a bump as a small
porting job rather than a number change.

The X11 layer has no automated coverage — it needs a live X server. It is
exercised by hand against `xclip`; an Xvfb-based integration test is the
obvious next addition. The picker is in the same position: its fuzzy matching
and age formatting are unit-tested, the widgets are not.

## Roadmap

| Phase | Scope |
|---|---|
| **v0.1** | **X11 text capture, CLI, egui picker, rofi integration** ← you are here |
| v0.2 | Paste-on-select, picker theming, richer preview pane |
| v0.3 | Wayland via `wlr-data-control`, images, file URIs |
| v0.4 | Pinning UX polish, richer snippet management |
| v1.0 | Encryption at rest, auto-clear, packaging (AUR, deb, Nix) |

## License

MIT
