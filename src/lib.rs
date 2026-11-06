//! `claimcheck`: checks that a research document's quotations, numbers and
//! citations are supported by the sources it cites.
//!
//! The binary is a thin wrapper around [`run`]; the modules are public so the
//! rules can be tested and reused.

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
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

/// A path with symlinks, `.` and `..` resolved. A path that does not exist is
/// resolved through its longest existing ancestor.
fn resolve(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut lexical = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(lexical.components().next_back(), Some(Component::Normal(_))) =>
            {
                lexical.pop();
            }
            c => lexical.push(c),
        }
    }
    let mut rest = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, name| acc.join(name));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return lexical,
        }
    }
}

struct Walk {
    /// Resolved `exclude` entries.
    exclude: Vec<PathBuf>,
    /// Resolved directories already walked; stops symlink loops.
    visited: HashSet<PathBuf>,
    /// Resolved files already collected.
    seen: HashSet<PathBuf>,
    out: Vec<PathBuf>,
}

impl Walk {
    fn excluded(&self, p: &Path) -> bool {
        self.exclude.iter().any(|x| p.starts_with(x))
    }

    fn file(&mut self, p: PathBuf, resolved: PathBuf) {
        if self.seen.insert(resolved) {
            self.out.push(p);
        }
    }

    /// Walks `dir` (as given) whose resolved path is `real`.
    fn dir(&mut self, dir: &Path, real: PathBuf) -> Result<(), Error> {
        if !self.visited.insert(real.clone()) {
            return Ok(());
        }
        let entries = std::fs::read_dir(dir)
            .map_err(|e| Error::new(format!("cannot read directory {}: {e}", dir.display())))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|e| Error::new(format!("cannot read directory {}: {e}", dir.display())))?;
            names.push(entry.file_name());
        }
        names.sort();
        for name in names {
            if name.to_str().is_some_and(|n| n.starts_with('.')) {
                continue;
            }
            let p = dir.join(&name);
            // The entry's own location, before following a symlink.
            let at = real.join(&name);
            if self.excluded(&at) {
                continue;
            }
            // Follows symlinks; a loop or a dangling link is an error rather than
            // a silently skipped entry, which could hide documents.
            let meta = std::fs::metadata(&p)
                .map_err(|e| Error::new(format!("cannot read {}: {e}", p.display())))?;
            if !meta.is_dir() && !is_markdown(&p) {
                continue;
            }
            let resolved = p
                .canonicalize()
                .map_err(|e| Error::new(format!("cannot read {}: {e}", p.display())))?;
            if self.excluded(&resolved) {
                continue;
            }
            if meta.is_dir() {
                self.dir(&p, resolved)?;
            } else {
                self.file(p, resolved);
            }
        }
        Ok(())
    }
}

/// Expands input paths: files are taken as given, directories are walked for
/// Markdown files (skipping hidden entries and `exclude`, following symlinks
/// once). A file reached by several paths is checked once.
pub fn collect_inputs(inputs: &[PathBuf], exclude: &[PathBuf]) -> Result<Vec<PathBuf>, Error> {
    let mut walk = Walk {
        exclude: exclude.iter().map(|x| resolve(x)).collect(),
        visited: HashSet::new(),
        seen: HashSet::new(),
        out: Vec::new(),
    };
    for p in inputs {
        let meta = std::fs::metadata(p)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", p.display())))?;
        let resolved = p
            .canonicalize()
            .map_err(|e| Error::new(format!("cannot read {}: {e}", p.display())))?;
        if meta.is_dir() {
            walk.dir(p, resolved)?;
        } else {
            walk.file(p.clone(), resolved);
        }
    }
    Ok(walk.out)
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
