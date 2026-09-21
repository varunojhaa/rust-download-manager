# Build rdm installers on GitHub

Add a GitHub Actions workflow so every push and every tagged release builds ready-to-run
desktop apps for Windows, macOS and Linux, and attaches them as downloadable files.

## What you get

- Push code to GitHub, open the Actions tab, and download the built app from the run.
- Push a version tag (e.g. `v0.1.0`) and a GitHub Release is created with all files attached.
- Files produced:
  - Windows: `rdm-gui.exe` (plus `.msi` installer when the packaging step succeeds)
  - macOS: `rdm.app` zipped (Apple Silicon and Intel)
  - Linux: `rdm-gui` binary, `.deb` and `.AppImage`

## Files to add

- `.github/workflows/build.yml` — main build matrix
- `.github/workflows/release.yml` — tag-triggered release that reuses the build job

(If you prefer one file, both can be merged; plan keeps them split for clarity.)

## Technical details

Workflow `build.yml`:

- Triggers: `push` to `main`, `pull_request`, `workflow_dispatch`, and `push` on `v*` tags.
- Matrix: `ubuntu-latest`, `windows-latest`, `macos-14` (arm64), `macos-13` (x86_64).
- Steps per job:
  1. `actions/checkout@v4`
  2. `dtolnay/rust-toolchain@stable`
  3. `Swatinem/rust-cache@v2` with `workspaces: rust-idm`
  4. Linux only: `apt-get install` for `libxkbcommon-dev libx11-dev libxcursor-dev
     libxrandr-dev libxi-dev libgl1-mesa-dev libwayland-dev pkg-config libssl-dev
     squashfs-tools` (squashfs-tools makes the AppImage target work)
  5. `cargo build --release --bins` in `rust-idm`
  6. `cargo test --release` (Linux job only, to keep runtime down)
  7. `cargo install cargo-bundle` then `cargo bundle --release --bin rdm-gui`,
     run with `continue-on-error: true` so a packaging failure never loses the raw binary
  8. Collect `target/release/rdm-gui[.exe]`, `target/release/rdm` CLI, and everything under
     `target/release/bundle/` into a `dist/` folder
  9. `actions/upload-artifact@v4` named `rdm-${{ matrix.os }}`

Workflow `release.yml`:

- Trigger: `push` on tags matching `v*`.
- Calls the build workflow via `workflow_call`, downloads all artifacts, then
  `softprops/action-gh-release@v2` attaches them to the release.
- Needs `permissions: contents: write`.

Notes:

- macOS `.app` is unsigned; first launch needs right-click → Open. Signing would need
  your Apple Developer certificate stored as repository secrets — not included here.
- Windows `.msi` comes from `cargo-bundle`; if it proves flaky on the runner, the fallback
  is the plain `rdm-gui.exe` artifact, which always builds.
