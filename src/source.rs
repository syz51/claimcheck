//! Source texts, loaded and indexed at most once per run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use sha2::{Digest, Sha256};

use crate::normalize::{self, NChar};
use crate::numbers::{self, NumTok};

const GRAM: usize = 3;

/// An extracted source text with the indexes the checks need.
pub struct IndexedText {
    pub text: String,
    line_starts: Vec<usize>,
    /// Normalized characters (case preserved) for number and word lookups.
    norm: Vec<NChar>,
    /// Skeleton characters and the original byte offset of each.
    pub skel: Vec<char>,
    skel_off: Vec<u32>,
    skel_str: String,
    /// Byte position in `skel_str` → character index in `skel`.
    skel_byte_to_char: Vec<u32>,
    grams: HashMap<[char; GRAM], Vec<u32>>,
    /// Numbers sorted by position in `norm`.
    numbers: Vec<NumTok>,
    number_index: HashMap<String, Vec<u32>>,
    /// Lowercase words → positions in `norm`.
    words: HashMap<String, Vec<u32>>,
}

impl IndexedText {
    pub fn new(text: String) -> IndexedText {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let norm = normalize::normalize_str(&text);
        let (skel, skel_off) = normalize::skeleton(&norm);
        let mut skel_str = String::with_capacity(skel.len());
        let mut skel_byte_to_char = Vec::with_capacity(skel.len() * 2);
        for (i, c) in skel.iter().enumerate() {
            skel_str.push(*c);
            skel_byte_to_char.resize(skel_str.len(), u32::try_from(i).unwrap_or(u32::MAX));
        }
        skel_byte_to_char.push(u32::try_from(skel.len()).unwrap_or(u32::MAX));
        let mut grams: HashMap<[char; GRAM], Vec<u32>> = HashMap::new();
        for (i, w) in skel.windows(GRAM).enumerate() {
            let key = [w[0], w[1], w[2]];
            grams.entry(key).or_default().push(i as u32);
        }
        let chars: Vec<char> = norm.iter().map(|n| n.c).collect();
        let numbers = numbers::scan(&chars);
        let mut number_index: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, t) in numbers.iter().enumerate() {
            for v in t.values() {
                number_index
                    .entry(v.to_string())
                    .or_default()
                    .push(i as u32);
            }
        }
        let mut words: HashMap<String, Vec<u32>> = HashMap::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_alphabetic() && !normalize::is_cjk(chars[i]) {
                let start = i;
                while i < chars.len() && chars[i].is_alphabetic() && !normalize::is_cjk(chars[i]) {
                    i += 1;
                }
                let w: String = chars[start..i]
                    .iter()
                    .flat_map(|c| c.to_lowercase())
                    .collect();
                if w.chars().count() >= 3 {
                    words.entry(w).or_default().push(start as u32);
                }
            } else {
                i += 1;
            }
        }
        IndexedText {
            text,
            line_starts,
            norm,
            skel,
            skel_off,
            skel_str,
            skel_byte_to_char,
            grams,
            numbers,
            number_index,
            words,
        }
    }

    /// 1-based line of a byte offset in the original text.
    pub fn line_of(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|s| *s <= byte)
    }

    /// Exact occurrence of a skeleton string at or after skeleton char `from`.
    pub fn find_exact(&self, needle: &str, from: usize) -> Option<usize> {
        let from_byte = self
            .skel_byte_to_char
            .partition_point(|c| (*c as usize) < from);
        let from_byte = from_byte.min(self.skel_str.len());
        let pos = self.skel_str.get(from_byte..)?.find(needle)?;
        Some(self.skel_byte_to_char[from_byte + pos] as usize)
    }

    pub fn grams(&self, g: &[char; GRAM]) -> &[u32] {
        self.grams.get(g).map_or(&[], Vec::as_slice)
    }

    /// Original byte range covered by skeleton chars `start..end`.
    pub fn skel_span(&self, start: usize, end: usize) -> (usize, usize) {
        if self.skel_off.is_empty() {
            return (0, 0);
        }
        let s = self.skel_off[start.min(self.skel_off.len() - 1)] as usize;
        let last =
            self.skel_off[end.saturating_sub(1).clamp(start, self.skel_off.len() - 1)] as usize;
        let e = last + self.text[last..].chars().next().map_or(0, char::len_utf8);
        (s, e.max(s))
    }

    /// Original text around a byte range, widened to `max` characters.
    pub fn excerpt(&self, start: usize, end: usize, max: usize) -> String {
        let floor = |mut i: usize| {
            while !self.text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let span = self.text[start..end].chars().count();
        let pad = max.saturating_sub(span) / 2;
        let mut s = start;
        for _ in 0..pad {
            if s == 0 {
                break;
            }
            s = floor(s - 1);
        }
        let shown: String = self.text[s..].chars().take(max + 1).collect();
        normalize::excerpt(&shown, max)
    }

    pub fn has_number(&self, value: &str) -> bool {
        self.number_index.contains_key(value)
    }

    /// Source numbers near occurrences of `context` words, best first:
    /// `(value, line)` pairs ranked by how many distinct context words occur
    /// within `window` normalized characters, then by closeness to `target`.
    pub fn nearby_numbers(
        &self,
        context: &[String],
        target: f64,
        window: usize,
        limit: usize,
    ) -> Vec<(String, usize)> {
        let mut score: HashMap<usize, u32> = HashMap::new();
        for word in context {
            let Some(positions) = self.words.get(word) else {
                continue;
            };
            let mut seen_here: Vec<usize> = Vec::new();
            for &p in positions.iter().take(500) {
                let p = p as usize;
                let lo = self.numbers.partition_point(|t| t.start + window < p);
                for (i, t) in self.numbers[lo..].iter().enumerate() {
                    if t.start > p + window {
                        break;
                    }
                    seen_here.push(lo + i);
                }
            }
            seen_here.sort_unstable();
            seen_here.dedup();
            for i in seen_here {
                *score.entry(i).or_default() += 1;
            }
        }
        let mut ranked: Vec<(u32, f64, usize)> = score
            .into_iter()
            .filter_map(|(i, s)| {
                let v = self.numbers[i].as_f64()?;
                let rel = (v - target).abs() / target.abs().max(1e-9);
                Some((s, rel, i))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut out: Vec<(String, usize)> = Vec::new();
        for (_, _, i) in ranked {
            let t = &self.numbers[i];
            if out.iter().any(|(v, _)| *v == t.value) {
                continue;
            }
            let off = self.norm[t.start].off as usize;
            out.push((t.value.clone(), self.line_of(off)));
            if out.len() == limit {
                break;
            }
        }
        out
    }
}

/// Outcome of loading one registered text file.
pub struct Loaded {
    pub text: Result<Arc<IndexedText>, String>,
    /// Recorded and actual SHA-256 when they differ.
    pub hash_mismatch: Option<(String, String)>,
}

pub struct TextEntry {
    pub abs: PathBuf,
    /// Path as shown in findings (relative to the project root when inside it).
    pub display: String,
    pub sha256: Option<String>,
    /// Keys of the references listing this file.
    pub keys: Vec<String>,
    cell: OnceLock<Loaded>,
}

impl TextEntry {
    pub fn new(abs: PathBuf, display: String, sha256: Option<String>) -> TextEntry {
        TextEntry {
            abs,
            display,
            sha256,
            keys: Vec::new(),
            cell: OnceLock::new(),
        }
    }

    /// Loads and indexes the file on first use; later calls share the result.
    pub fn get(&self) -> &Loaded {
        self.cell.get_or_init(|| match std::fs::read(&self.abs) {
            Err(e) => Loaded {
                text: Err(e.to_string()),
                hash_mismatch: None,
            },
            Ok(bytes) => {
                let actual = hex(&Sha256::digest(&bytes));
                let hash_mismatch = self
                    .sha256
                    .as_ref()
                    .filter(|want| !want.eq_ignore_ascii_case(&actual))
                    .map(|want| (want.clone(), actual));
                let text = match String::from_utf8(bytes) {
                    Ok(t) => Ok(Arc::new(IndexedText::new(t))),
                    Err(e) => Err(format!("not UTF-8: {e}")),
                };
                Loaded {
                    text,
                    hash_mismatch,
                }
            }
        })
    }

    /// The load result if the file was needed during this run.
    pub fn loaded(&self) -> Option<&Loaded> {
        self.cell.get()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_search_maps_back_to_lines() {
        let t = IndexedText::new("first line\nthe retriev-\nal step works\n".into());
        let needle = normalize::skeleton_str("retrieval step");
        let at = t.find_exact(&needle, 0).unwrap();
        let (s, _) = t.skel_span(at, at + needle.chars().count());
        assert_eq!(t.line_of(s), 2);
        assert!(t.find_exact(&needle, at + 1).is_none());
    }

    #[test]
    fn numbers_are_indexed_canonically() {
        let t = IndexedText::new("Accuracy reaches 12.30 % on 1,234 queries (Table 2).".into());
        assert!(t.has_number("12.3"));
        assert!(t.has_number("1234"));
        assert!(!t.has_number("12.4"));
    }

    #[test]
    fn nearby_numbers_rank_by_context() {
        let t = IndexedText::new("Recall is 71.2 on FinQA.\nUnrelated 70.6 value.\n".into());
        let near = t.nearby_numbers(&["recall".into(), "finqa".into()], 70.5, 15, 2);
        assert_eq!(near[0], ("71.2".to_string(), 1));
    }
}
