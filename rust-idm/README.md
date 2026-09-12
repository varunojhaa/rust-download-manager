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
