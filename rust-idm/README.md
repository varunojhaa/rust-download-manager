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

- **Downloads list** with live progress, speed, ETA, search, sorting, pause / resume / retry / restart / remove / delete file, copy link, open file and open folder.
- **Automatic retry**: failed downloads are retried a configurable number of times after a configurable delay.
- **Categories**: files are sorted automatically into Music, Video, Documents, Compressed, Programs and Other folders inside the save folder. Categories and their extensions are editable in Settings.
- **Queues & scheduling**: any number of queues, each with an optional local `HH:MM` start and stop time plus active weekdays. Downloads only run inside their queue window; a running download pauses when its window closes.
- **HLS streaming**: `.m3u8` links are resolved (best variant of a master playlist) and every segment is joined into one `.ts` file.
- **Themes**: dark, light and pure black, with a custom accent colour.
- **Clipboard capture**: copy a download link anywhere and rdm offers to grab it (file types configurable).
- **Notifications**: desktop notification when a download finishes or fails, plus in-app toasts.
- **When everything finishes**: do nothing, close rdm, or shut down the computer.
- **Network options**: proxy (HTTP/SOCKS5), custom user agent, referer, cookies, extra headers and DNS-over-HTTPS — globally or per download, plus a per-download speed limit.
- **Link import**: "Import links…" scans a text file for links and opens a select/deselect window before adding them to the queue.
- **Browser integration**: a local endpoint accepts links from the browser extensions in `extension/`.
  - `GET  http://127.0.0.1:15080/ping`
  - `POST http://127.0.0.1:15080/add` with `{"url": "…", "filename": "…", "referer": "…"}` or `{"urls": ["…"]}`
  - Chrome/Edge: `chrome://extensions` → Developer mode → Load unpacked → `extension/`.
  - Firefox: `about:debugging` → This Firefox → Load Temporary Add-on → pick `extension/manifest-firefox.json`.
  - Both capture browser downloads and add "Download with rdm" plus "Download all links on this page" right-click items.

The CLI takes the same network flags: `--proxy`, `--user-agent`, `--referer`, `--cookie`, `--doh`, `--header "Name: value"`.

The download list and settings are stored in `~/.config/rdm/state.json`; partial downloads resume through the same `.rdm.json` sidecars used by the CLI.

Linux build dependencies for the GUI: `libxkbcommon`, `libX11`, `libXcursor`, `libXrandr`, `libXi`, `libGL`, `wayland`.

## Build an application installer

Linux and macOS use [`cargo-bundle`](https://github.com/burtonageo/cargobundle):

```bash
cargo install cargo-bundle
```

Then build the desktop app package for your current platform:

```bash
cd rust-idm
cargo bundle --release --bin rdm-gui
```

Windows uses [`cargo-wix`](https://github.com/volks73/cargo-wix) and the WiX Toolset
instead of `cargo-bundle`'s experimental MSI writer:

```powershell
cargo install cargo-wix --locked
cargo wix --nocapture
```

Outputs:

- **Linux:** `target/release/bundle/deb/*.deb` and `target/release/bundle/appimage/*.AppImage`
- **Windows:** `target/wix/*.msi`
- **macOS:** `target/release/bundle/osx/rdm.app` (drag to `/Applications`)

If a platform target is not supported by `cargo-bundle`, you can still ship the raw binary from `target/release/rdm-gui`.

## Signing the released files (optional)

The GitHub workflow signs each platform's files when the matching repository secrets
exist (Settings → Secrets and variables → Actions). Leave them empty and the build
still runs, just unsigned.

**Windows (Authenticode)**

| Secret | Value |
| --- | --- |
| `WINDOWS_CERT_BASE64` | your code-signing `.pfx`, base64 encoded |
| `WINDOWS_CERT_PASSWORD` | password for that `.pfx` |
| `WINDOWS_TIMESTAMP_URL` | optional, defaults to DigiCert's timestamp server |

**macOS (Developer ID + notarization)**

| Secret | Value |
| --- | --- |
| `MACOS_CERT_P12_BASE64` | Developer ID Application certificate `.p12`, base64 encoded |
| `MACOS_CERT_PASSWORD` | password for that `.p12` |
| `MACOS_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID email (only needed for notarization) |
| `APPLE_TEAM_ID` | your 10-character team ID |
| `APPLE_APP_PASSWORD` | app-specific password for that Apple ID |

**Linux (GPG detached signatures)**

| Secret | Value |
| --- | --- |
| `LINUX_GPG_PRIVATE_KEY` | exported private key (ASCII-armored or base64) |
| `LINUX_GPG_PASSPHRASE` | passphrase for that key |
| `LINUX_GPG_KEY_ID` | optional key ID when the keyring holds more than one |

Every release also ships `SHA256SUMS.txt`, and Linux builds add a `.asc` signature per
file plus the public key as `rdm-signing-key.asc`.

Base64-encode a certificate with `base64 -w0 cert.pfx` (Linux) or
`base64 -i cert.p12 | tr -d '\n'` (macOS).
