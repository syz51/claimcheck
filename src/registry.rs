//! Reader for the `rf.reference/1` registry (JSON Lines).

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// How far the archive inspected a source, shallowest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Depth {
    Metadata,
    Abstract,
    Method,
    Full,
    Artifact,
    Reproduced,
}

impl Depth {
    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Metadata => "metadata",
            Depth::Abstract => "abstract",
            Depth::Method => "method",
            Depth::Full => "full",
            Depth::Artifact => "artifact",
            Depth::Reproduced => "reproduced",
        }
    }
}

impl fmt::Display for Depth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRef {
    /// Path relative to the project root, as written in the registry.
    pub path: String,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub key: String,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub depth: Option<Depth>,
    pub texts: Vec<TextRef>,
    pub duplicate_of: Option<String>,
    /// 1-based line in the registry file.
    pub line: usize,
}

#[derive(Debug)]
pub struct Registry {
    pub refs: Vec<Reference>,
    by_key: HashMap<String, usize>,
}

#[derive(Deserialize)]
struct RawText {
    path: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct RawRef {
    key: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    authors: Option<Vec<String>>,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    depth: Option<Depth>,
    #[serde(default)]
    texts: Option<Vec<RawText>>,
    #[serde(default)]
    duplicate_of: Option<String>,
}

const SCHEMA: &str = "rf.reference";
const MAJOR: &str = "1";

fn check_schema(value: &serde_json::Value) -> Result<(), String> {
    let Some(schema) = value.get("schema").and_then(|s| s.as_str()) else {
        return Err("missing string field `schema`".into());
    };
    let (name, version) = schema
        .split_once('/')
        .ok_or_else(|| format!("schema `{schema}` has no version"))?;
    let major = version.split('.').next().unwrap_or_default();
    if name != SCHEMA || major != MAJOR {
        return Err(format!(
            "unsupported schema `{schema}` (expected {SCHEMA}/{MAJOR})"
        ));
    }
    Ok(())
}

impl Registry {
    pub fn parse(text: &str, display: &str) -> Result<Registry, Error> {
        let mut refs = Vec::new();
        let mut by_key = HashMap::new();
        for (idx, line) in text.lines().enumerate() {
            let lineno = idx + 1;
            if line.trim().is_empty() {
                continue;
            }
            let at = |msg: String| Error::new(format!("{display}:{lineno}: {msg}"));
            let value: serde_json::Value =
                serde_json::from_str(line).map_err(|e| at(format!("invalid JSON: {e}")))?;
            check_schema(&value).map_err(at)?;
            let raw: RawRef =
                serde_json::from_value(value).map_err(|e| at(format!("invalid record: {e}")))?;
            if raw.key.is_empty() {
                return Err(at("empty `key`".into()));
            }
            if let Some(first) = by_key.insert(raw.key.clone(), refs.len()) {
                let first: &Reference = &refs[first];
                return Err(at(format!(
                    "duplicate key `{}` (first on line {})",
                    raw.key, first.line
                )));
            }
            refs.push(Reference {
                key: raw.key,
                title: raw.title,
                authors: raw.authors.unwrap_or_default(),
                year: raw.year,
                depth: raw.depth,
                texts: raw
                    .texts
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| TextRef {
                        path: t.path,
                        sha256: t.sha256,
                    })
                    .collect(),
                duplicate_of: raw.duplicate_of,
                line: lineno,
            });
        }
        Ok(Registry { refs, by_key })
    }

    pub fn load(path: &Path, display: &str) -> Result<Registry, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read registry {display}: {e}")))?;
        Registry::parse(&text, display)
    }

    pub fn index(&self, key: &str) -> Option<usize> {
        self.by_key.get(key).copied()
    }

    pub fn get(&self, key: &str) -> Option<&Reference> {
        self.index(key).map(|i| &self.refs[i])
    }

    /// Follows `duplicate_of` to the surviving key. Returns `None` when the
    /// chain is broken (points at an unknown key) or cycles.
    pub fn survivor(&self, key: &str) -> Option<&str> {
        let mut current = self.get(key)?;
        for _ in 0..self.refs.len() {
            match current.duplicate_of.as_deref() {
                None => return Some(&current.key),
                Some(next) => current = self.get(next)?,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"schema":"rf.reference/1","key":"7","type":"paper","title":"A Study","authors":["Ada Lovelace"],"year":2025,"depth":"method","texts":[{"path":"t/7.txt","sha256":null,"origin":null}],"duplicate_of":null,"extra":1}"#;

    #[test]
    fn parses_and_ignores_unknown_fields() {
        let reg = Registry::parse(LINE, "r.jsonl").unwrap();
        let r = reg.get("7").unwrap();
        assert_eq!(r.depth, Some(Depth::Method));
        assert_eq!(r.texts[0].path, "t/7.txt");
        assert_eq!(r.line, 1);
    }

    #[test]
    fn rejects_unknown_major_version() {
        let line = LINE.replace("rf.reference/1", "rf.reference/2");
        let err = Registry::parse(&line, "r.jsonl").unwrap_err();
        assert!(
            err.to_string().contains("r.jsonl:1: unsupported schema"),
            "{err}"
        );
        assert!(Registry::parse(&LINE.replace("rf.reference/1", "rf.reference/1.3"), "r").is_ok());
    }

    #[test]
    fn rejects_duplicate_keys_with_line() {
        let text = format!("{LINE}\n\n{LINE}\n");
        let err = Registry::parse(&text, "r.jsonl").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("r.jsonl:3: duplicate key `7` (first on line 1)"),
            "{err}"
        );
    }

    #[test]
    fn follows_duplicate_chains() {
        let text = [
            r#"{"schema":"rf.reference/1","key":"1","duplicate_of":"2"}"#,
            r#"{"schema":"rf.reference/1","key":"2","duplicate_of":"3"}"#,
            r#"{"schema":"rf.reference/1","key":"3","duplicate_of":null}"#,
            r#"{"schema":"rf.reference/1","key":"4","duplicate_of":"5"}"#,
            r#"{"schema":"rf.reference/1","key":"5","duplicate_of":"4"}"#,
        ]
        .join("\n");
        let reg = Registry::parse(&text, "r").unwrap();
        assert_eq!(reg.survivor("1"), Some("3"));
        assert_eq!(reg.survivor("4"), None);
    }

    #[test]
    fn depth_is_ordered() {
        assert!(Depth::Abstract < Depth::Method);
        assert!(Depth::Full < Depth::Reproduced);
    }
}
