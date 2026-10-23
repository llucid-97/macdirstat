use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::env;
use std::fs::{self, Metadata, ReadDir};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const HELP: &str = "Usage: macdirstat-cli [--top N] [--] DIRECTORY
Read-only disk scan. JSON goes to stdout; progress goes to stderr.
Default: 30 largest files and 30 largest directories, ranked by allocated bytes.
Stays on the root filesystem; does not follow child symlinks.
Exit codes: 0 complete, 1 invalid input/output failure, 2 partial scan.
";
const MAX_DEPTH: usize = 256;
const ERROR_SAMPLES: usize = 100;

type Ranked = (u64, PathBuf, u64, &'static str);

#[derive(Clone, Copy, Default)]
struct Totals {
    allocated: u64,
    logical: u64,
    files: u64,
    directories: u64,
}

impl Totals {
    fn add(&mut self, other: Self) {
        self.allocated += other.allocated;
        self.logical += other.logical;
        self.files += other.files;
        self.directories += other.directories;
    }
}

struct Frame {
    path: PathBuf,
    entries: ReadDir,
    totals: Totals,
}

#[derive(Default)]
struct Scan {
    files: BinaryHeap<Reverse<Ranked>>,
    directories: BinaryHeap<Reverse<Ranked>>,
    hardlinks: HashSet<(u64, u64)>,
    duplicate_links: u64,
    skipped_filesystems: u64,
    error_count: u64,
    errors: Vec<Value>,
    visited: u64,
}

fn path_json(path: &Path) -> Value {
    let mut value = json!({"path": path.to_string_lossy()});
    // Preserve exact names for callers even when a filename is not valid UTF-8.
    if path.to_str().is_none() {
        let hex: String = path
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        value["path_bytes_hex"] = json!(hex);
    }
    value
}

fn totals_json(totals: Totals) -> Value {
    json!({"allocated_bytes": totals.allocated, "logical_bytes": totals.logical,
        "files": totals.files, "directories": totals.directories})
}

fn retain(heap: &mut BinaryHeap<Reverse<Ranked>>, entry: Ranked, limit: usize) {
    heap.push(Reverse(entry));
    if heap.len() > limit {
        heap.pop();
    }
}

fn ranked_json(heap: BinaryHeap<Reverse<Ranked>>) -> Vec<Value> {
    heap.into_sorted_vec()
        .into_iter()
        .map(|Reverse((allocated, path, logical, kind))| {
            let mut value = path_json(&path);
            value["allocated_bytes"] = json!(allocated);
            value["logical_bytes"] = json!(logical);
            value["kind"] = json!(kind);
            value
        })
        .collect()
}

impl Scan {
    fn error(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.error_count += 1;
        if self.errors.len() < ERROR_SAMPLES {
            let mut value = path_json(path);
            value["error"] = json!(error.to_string());
            self.errors.push(value);
        }
    }

    fn measure(&mut self, meta: &Metadata) -> Totals {
        if !meta.is_dir() && meta.nlink() > 1 && !self.hardlinks.insert((meta.dev(), meta.ino())) {
            self.duplicate_links += 1;
            return Totals::default();
        }
        let directory = meta.is_dir();
        Totals {
            allocated: meta.blocks() * 512,
            logical: if directory { 0 } else { meta.len() },
            files: u64::from(!directory),
            directories: u64::from(directory),
        }
    }
}

fn scan(root: PathBuf, limit: usize) -> io::Result<(Value, bool)> {
    let started = Instant::now();
    let meta = fs::symlink_metadata(&root)?;
    if !meta.is_dir() {
        return Err(io::Error::other("scan root must be a directory"));
    }
    let device = meta.dev();
    let mut state = Scan::default();
    let mut stack = vec![Frame {
        path: root.clone(),
        entries: fs::read_dir(&root)?,
        totals: state.measure(&meta),
    }];
    let mut progress = Instant::now();
    let totals = loop {
        let frame = stack
            .last_mut()
            .expect("root frame exists until scan completes");
        match frame.entries.next() {
            Some(Ok(entry)) => {
                let path = entry.path();
                state.visited += 1;
                if progress.elapsed() >= Duration::from_secs(2) {
                    eprintln!(
                        "Scanned {} entries in {:.0}s; {} errors",
                        state.visited,
                        started.elapsed().as_secs_f64(),
                        state.error_count
                    );
                    progress = Instant::now();
                }
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        state.error(&path, error);
                        continue;
                    }
                };
                if meta.dev() != device {
                    state.skipped_filesystems += 1;
                    continue;
                }
                let totals = state.measure(&meta);
                if meta.is_dir() {
                    if stack.len() >= MAX_DEPTH {
                        state.error(&path, "directory depth limit reached");
                    } else {
                        match fs::read_dir(&path) {
                            Ok(entries) => {
                                stack.push(Frame {
                                    path,
                                    entries,
                                    totals,
                                });
                                continue;
                            }
                            Err(error) => state.error(&path, error),
                        }
                    }
                } else if totals.files != 0 {
                    let kind = if meta.file_type().is_symlink() {
                        "symlink"
                    } else {
                        "file"
                    };
                    retain(
                        &mut state.files,
                        (totals.allocated, path.clone(), totals.logical, kind),
                        limit,
                    );
                }
                stack.last_mut().unwrap().totals.add(totals);
            }
            Some(Err(error)) => state.error(&frame.path, error),
            None => {
                let frame = stack.pop().unwrap();
                if let Some(parent) = stack.last_mut() {
                    parent.totals.add(frame.totals);
                    retain(
                        &mut state.directories,
                        (
                            frame.totals.allocated,
                            frame.path,
                            frame.totals.logical,
                            "directory",
                        ),
                        limit,
                    );
                } else {
                    break frame.totals;
                }
            }
        }
    };
    let partial = state.error_count != 0;
    let mut output = path_json(&root);
    output["schema_version"] = json!(1);
    output["complete"] = json!(!partial);
    output["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    output["totals"] = totals_json(totals);
    output["largest_files"] = json!(ranked_json(state.files));
    output["largest_directories"] = json!(ranked_json(state.directories));
    output["hardlink_duplicates_skipped"] = json!(state.duplicate_links);
    output["other_filesystems_skipped"] = json!(state.skipped_filesystems);
    output["error_count"] = json!(state.error_count);
    output["errors"] = json!(state.errors);
    Ok((output, partial))
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let mut root = None;
    let mut limit = 30;
    let mut positional = false;
    while let Some(arg) = args.next() {
        if !positional && (arg == "--help" || arg == "-h") {
            print!("{HELP}");
            return Ok(false);
        } else if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg == "--top" {
            limit = args
                .next()
                .ok_or("--top requires a number")?
                .to_str()
                .ok_or("invalid --top value")?
                .parse::<usize>()?;
            if !(1..=10_000).contains(&limit) {
                return Err("--top must be between 1 and 10000".into());
            }
        } else if !positional && arg.as_bytes().starts_with(b"-") {
            return Err(format!("unknown option: {}", arg.to_string_lossy()).into());
        } else if root.replace(PathBuf::from(arg)).is_some() {
            return Err("supply exactly one directory".into());
        }
    }
    let root = root.ok_or(HELP)?.canonicalize()?;
    let (output, partial) = scan(root, limit)?;
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    serde_json::to_writer_pretty(&mut stdout, &output)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(partial)
}

fn main() {
    let code = match run() {
        Ok(false) => 0,
        Ok(true) => 2,
        Err(error) => {
            eprintln!("macdirstat-cli: {error}");
            1
        }
    };
    std::process::exit(code);
}
