# rdm — a Rust download manager (IDM alternative)

Fast, resumable, multi-connection downloader with a queue, scheduling and live progress bars.

## Build

```bash
cd rust-idm
cargo build --release
./target/release/rdm --help
```

## Use

```bash
# 8 parallel connections, save as ubuntu.iso
rdm get https://releases.ubuntu.com/24.04/ubuntu-24.04.iso -o ubuntu.iso -c 8

# several files, 3 at a time, capped at 2 MB/s total
rdm get URL1 URL2 URL3 -o ~/Downloads -j 3 --limit 2M

# queue file, start tonight at 02:30 UTC
rdm queue downloads.txt -d ~/Downloads -j 2 -c 16 --at 02:30

# inspect saved progress of an interrupted download
rdm status ubuntu.iso
```

Queue file format (`#` comments allowed):

```
https://example.com/big.iso
https://example.com/other.zip   renamed.zip
```

## How pause/resume works

Each download splits into byte ranges, one connection per range. Progress is written
to `<file>.rdm.json` roughly twice a second and on exit. Press `Ctrl-C` to pause —
rerun the exact same command to continue from where each connection stopped. The
resume file is discarded once the download completes, and ignored if the remote
file's size or ETag changed.

Servers without HTTP range support fall back to a single stream automatically.

## Desktop app (GUI)

An AB Download Manager–style desktop window, built with egui:

```bash
cargo run --release --bin rdm-gui
```

Features:

- **Downloads list** with live progress, speed, pause / resume / retry / remove / delete file, and "open folder" when finished.
- **Categories**: files are sorted automatically into Music, Video, Documents, Compressed, Programs and Other folders inside the save folder. Categories and their extensions are editable in Settings.
- **Queues & scheduling**: any number of queues, each with an optional local `HH:MM` start and stop time. Downloads only run inside their queue window; a running download pauses when its window closes.
- **Settings**: save folder, simultaneous downloads, connections per download, global speed limit.
- **Link import**: "Import links…" scans a text file for links and opens a select/deselect window before adding them to the queue.
- **Browser integration**: a local endpoint accepts links from the browser extension in `extension/`.
  - `GET  http://127.0.0.1:15080/ping`
  - `POST http://127.0.0.1:15080/add` with `{"url": "…", "filename": "…"}` or `{"urls": ["…"]}`
  - Load `extension/` in Chrome via `chrome://extensions` → Developer mode → Load unpacked. It captures browser downloads and adds a "Download with rdm" right-click item.

The download list and settings are stored in `~/.config/rdm/state.json`; partial downloads resume through the same `.rdm.json` sidecars used by the CLI.

Linux build dependencies for the GUI: `libxkbcommon`, `libX11`, `libXcursor`, `libXrandr`, `libXi`, `libGL`, `wayland`.

## Build an application installer

Install [`cargo-bundle`](https://github.com/burtonageo/cargobundle):

```bash
cargo install cargo-bundle
```

Then build the desktop app package for your current platform:

```bash
cd rust-idm
cargo bundle --release --bin rdm-gui
```

Outputs:

- **Linux:** `target/release/bundle/deb/*.deb` and `target/release/bundle/appimage/*.AppImage`
- **Windows:** `target/release/bundle/msi/*.msi`
- **macOS:** `target/release/bundle/osx/rdm.app` (drag to `/Applications`)

If a platform target is not supported by `cargo-bundle`, you can still ship the raw binary from `target/release/rdm-gui`.
