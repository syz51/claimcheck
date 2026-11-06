//! `claimcheck`: checks that a research document's quotations, numbers and
//! citations are supported by the sources it cites.
//!
//! The binary is a thin wrapper around [`run`]; the modules are public so the
//! rules can be tested and reused.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub mod check;
pub mod config;
pub mod doc;
pub mod finding;
pub mod matcher;
pub mod normalize;
pub mod numbers;
pub mod registry;
pub mod source;

use check::{Project, Stats};
use finding::Finding;

/// A usage, configuration or I/O failure (exit code 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn is_markdown(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "md" | "markdown"))
}

fn walk(dir: &Path, exclude: &[PathBuf], out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::new(format!("cannot read directory {}: {e}", dir.display())))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::new(format!("cannot read directory {}: {e}", dir.display())))?;
        paths.push(entry.path());
    }
    paths.sort();
    for p in paths {
        let hidden = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'));
        if hidden || exclude.iter().any(|x| p.starts_with(x)) {
            continue;
        }
        if p.is_dir() {
            walk(&p, exclude, out)?;
        } else if is_markdown(&p) {
            out.push(p);
        }
    }
    Ok(())
}

/// Expands input paths: files are taken as given, directories are walked for
/// Markdown files (skipping hidden entries and `exclude`).
pub fn collect_inputs(inputs: &[PathBuf], exclude: &[PathBuf]) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    for p in inputs {
        if p.is_dir() {
            walk(p, exclude, &mut out)?;
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            return Err(Error::new(format!(
                "no such file or directory: {}",
                p.display()
            )));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    Ok(out)
}

/// Checks `files` in parallel. Findings come back in input order.
pub fn run(project: &Project, files: &[PathBuf]) -> Result<(Vec<Finding>, Stats), Error> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Vec<Finding>, Stats)>> = Mutex::new(Vec::new());
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(files.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let display = project.cfg.display_path(path);
                    match std::fs::read_to_string(path) {
                        Ok(src) => {
                            let (f, st) = check::check_document(project, &display, src, i);
                            results.lock().expect("no worker panicked").push((i, f, st));
                        }
                        Err(e) => {
                            failure
                                .lock()
                                .expect("no worker panicked")
                                .get_or_insert(Error::new(format!("cannot read {display}: {e}")));
                            next.store(files.len(), Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().expect("no worker panicked") {
        return Err(e);
    }
    let mut results = results.into_inner().expect("no worker panicked");
    results.sort_by_key(|(i, ..)| *i);
    let mut findings = Vec::new();
    let mut stats = Stats::default();
    for (_, f, st) in results {
        findings.extend(f);
        stats.add(&st);
    }
    findings.extend(project.text_findings(files.len()));
    Ok((findings, stats))
}
