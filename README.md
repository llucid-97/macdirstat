# MacDirStat

A disk usage visualizer for macOS, inspired by [WinDirStat](https://windirstat.net/) and [WizTree](https://diskanalyzer.com/). I loved the functionality of these tools but was never really satisfied with the alternatives available on macOS, so I built my own.

## Features

- **Treemap visualization** with cushion shading (matching WinDirStat's look)
- **Directory tree** with collapsible nodes, keyboard navigation, and size annotations
- **File type breakdown** showing extension statistics with color coding
- **Fast scanning** using macOS-native `getattrlistbulk` syscall with parallel tree building via rayon
- **Delete files/folders** directly from the UI — ⌘Delete for instant delete, Delete for native macOS confirmation dialog
- **Open any folder** via the File menu, native folder picker on startup, or pass a path on the command line

## Screenshot

<img src="https://github.com/MichaelStromberg/macdirstat/blob/main/screenshot.png?raw=true" alt="MacDirStat screenshot" width="750" />

## Building

Requires Rust (2024 edition). macOS only — uses platform-specific APIs for fast directory scanning.

```sh
cargo build --release
```

## Usage

```sh
# Launch with folder picker
cargo run --release

# Scan a specific directory
cargo run --release -- /path/to/scan
```

## Read-only CLI

Build and install the CLI without any GUI dependencies:

```sh
cargo build --release --no-default-features --features cli --bin macdirstat-cli
mkdir -p ~/.local/bin
install -m 755 target/release/macdirstat-cli ~/.local/bin/macdirstat-cli
macdirstat-cli --top 30 "$HOME" > scan.json
```

This keeps the Cargo-installed GUI command intact. `macdirstat-cli` writes
JSON to stdout and progress to stderr. The JSON contains totals, largest files,
largest directories (excluding the scan root), and up to 100 read errors.
Exit status is 0 for a complete scan, 2 for a partial scan, or 1 for invalid input
or a fatal error. All sizes are exact byte counts, ranked by **allocated bytes**
(`st_blocks * 512`), with logical sizes included for comparison.

The CLI uses a serial, streaming filesystem walk rather than the GUI's full-tree
bulk scanner. It retains only the top results, up to 256 open directory frames,
and an inode index for files with multiple hard links. Memory for that index grows
with the number of hard-linked files, not every file in the tree. Each hard-linked
file is counted once, under whichever path is encountered first. Child symlinks
are measured but not followed. An explicitly supplied root symlink is resolved.
Non-UTF-8 paths include `path_bytes_hex` for lossless use by callers.

Other filesystems, including mounted network shares, are skipped and counted in
`other_filesystems_skipped`. Scan the Data volume or a home directory explicitly
on macOS; scanning `/` alone may miss the separately mounted Data volume.
Unreadable paths, disappearing files, and the depth limit produce a partial scan;
its totals are lower bounds. Full Disk Access restrictions still apply.

Allocated bytes are **not a promise of space recovered by deletion**: APFS clones,
snapshots, compression, and hard links outside the scan can change that amount.
The CLI has no delete operation and does not create a scan database.

## How it works

MacDirStat scans directories using the macOS `getattrlistbulk` syscall, which retrieves multiple directory entries with their attributes in a single kernel call — avoiding per-file overhead. Directory traversal is parallelized across cores using rayon, with `openat()` for efficient relative path resolution.

The treemap uses squarified layout from the `treemap` crate with cushion-shaded rendering, producing the familiar WinDirStat look where each file is a colored rectangle sized proportionally to its disk usage.

## License

[GPL-3.0](LICENSE)
