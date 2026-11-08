//! Source texts, loaded and indexed at most once per run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use sha2::{Digest, Sha256};

use crate::normalize::{self, NChar};
use crate::numbers::{self, NearMatch, NumTok};

const GRAM: usize = 3;

/// An extracted source text with the indexes the checks need.
pub struct IndexedText {
    pub text: String,
    line_starts: Vec<usize>,
    /// Normalized characters (case preserved) for number and word lookups.
    norm: Vec<NChar>,
    /// Position of each `norm` character with every run of spaces counted as
    /// one character, so that layout padding does not stretch distances.
    squeezed: Vec<u32>,
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
    /// Space-split thousands groups (see [`numbers::split_groups`]) read as
    /// one number, by the index of their first group in `numbers`: the whole
    /// run, then its shorter prefixes of two or more groups.
    groups: HashMap<usize, Vec<NumTok>>,
    /// Lowercase words → positions in `norm`.
    words: HashMap<String, Vec<u32>>,
}

impl IndexedText {
    pub fn new(text: String) -> IndexedText {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let norm = normalize::normalize_str(&text);
        let mut squeezed = Vec::with_capacity(norm.len());
        let mut pos = 0u32;
        for (i, n) in norm.iter().enumerate() {
            if i > 0 && !(n.c == ' ' && norm[i - 1].c == ' ') {
                pos += 1;
            }
            squeezed.push(pos);
        }
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
            for v in t.index_keys() {
                number_index.entry(v).or_default().push(i as u32);
            }
        }
        // Joined values of space-split thousands groups, beside the groups'
        // own keys, which table cells need. Only the whole run is an exact
        // key; its prefixes are near-match candidates.
        let mut groups = HashMap::new();
        for (i, joined) in numbers::split_groups(&chars, &numbers) {
            let whole = &joined[0];
            if numbers[i].negative {
                number_index
                    .entry(format!("-{whole}"))
                    .or_default()
                    .push(i as u32);
            }
            number_index
                .entry(whole.clone())
                .or_default()
                .push(i as u32);
            let toks = joined
                .into_iter()
                .map(|value| NumTok {
                    value,
                    separated: true,
                    ..numbers[i].clone()
                })
                .collect();
            groups.insert(i, toks);
        }
        let mut words: HashMap<String, Vec<u32>> = HashMap::new();
        for (start, _, w) in normalize::words(&chars) {
            words.entry(w).or_default().push(start as u32);
        }
        IndexedText {
            text,
            line_starts,
            norm,
            squeezed,
            skel,
            skel_off,
            skel_str,
            skel_byte_to_char,
            grams,
            numbers,
            number_index,
            groups,
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

    /// Like [`IndexedText::find_exact`], but only occurrences that start and
    /// end at token boundaries: a quotation must not cut a word or a number of
    /// the source (`reaches 12` inside `reaches 12.3 %`).
    pub fn find_bounded(&self, needle: &str, from: usize) -> Option<usize> {
        let len = needle.chars().count();
        let mut from = from;
        while let Some(p) = self.find_exact(needle, from) {
            if self.bounded(p, p + len) {
                return Some(p);
            }
            from = p + 1;
        }
        None
    }

    /// Whether skeleton chars `start..end` begin and end at token boundaries.
    pub fn bounded(&self, start: usize, end: usize) -> bool {
        if start >= end || end > self.skel.len() {
            return false;
        }
        (start == 0 || !self.joined(start - 1, start))
            && (end == self.skel.len() || !self.joined(end - 1, end))
    }

    /// Whether adjacent skeleton chars `a` and `a + 1` belong to one token of
    /// the original text, so that a match may not begin or end between them.
    fn joined(&self, a: usize, b: usize) -> bool {
        let (x, y) = (self.skel[a], self.skel[b]);
        let (oa, ob) = (self.skel_off[a] as usize, self.skel_off[b] as usize);
        if oa == ob {
            return true; // one original character, such as a ligature
        }
        let after_a = oa + self.text[oa..].chars().next().map_or(0, char::len_utf8);
        let raw_gap = &self.text[after_a.min(ob)..ob];
        let numeric = |c: char| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '%');
        // A digit next to a digit, decimal point, separator, sign or percent
        // sign continues the same number: "12" of "12.3 %" or "2.1" of "-2.1".
        // Whitespace ends a number except before a percent sign, as in the
        // number scanner: "31, 2021" and table cells "68.56 66.31" are two.
        if (x.is_ascii_digit() && numeric(y)) || (y.is_ascii_digit() && numeric(x)) {
            return y == '%' || !raw_gap.contains(char::is_whitespace);
        }
        if !(x.is_alphanumeric() && y.is_alphanumeric())
            || normalize::is_cjk(x)
            || normalize::is_cjk(y)
        {
            return false;
        }
        // Two letters or digits: one word unless the original text separates them.
        let gap: String = raw_gap
            .chars()
            .filter(|c| {
                !unicode_normalization::char::is_combining_mark(*c)
                    && !matches!(
                        *c,
                        '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
                    )
            })
            .collect();
        let hyphenation = gap
            .strip_prefix(['-', '\u{2010}', '\u{2011}'])
            .is_some_and(|rest| rest.contains('\n') && rest.chars().all(char::is_whitespace));
        gap.is_empty() || hyphenation
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
    /// The window counts each run of spaces or tabs as one character, so a
    /// padded table row is as close to its label as the same row unpadded.
    pub fn nearby_numbers(
        &self,
        context: &[String],
        target: f64,
        window: usize,
        limit: usize,
    ) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for i in self.nearby(context, target, window) {
            let value = self.numbers[i].signed();
            if out.iter().any(|(v, _)| *v == value) {
                continue;
            }
            out.push((value, self.number_line(i)));
            if out.len() == limit {
                break;
            }
        }
        out
    }

    /// The [`IndexedText::nearby_numbers`] candidate that `n` matches
    /// approximately (see [`NumTok::near_match`]), as how it matched, the
    /// source value (in full for a [`NearMatch::Scaled`] match) and its line.
    /// A candidate that starts space-split thousands groups is also tried as
    /// the joined number and as each joined prefix of two or more groups. The
    /// closest kind of match wins, then the better-ranked candidate, then the
    /// longer run.
    pub fn near_number(
        &self,
        n: &NumTok,
        context: &[String],
        window: usize,
        rounding: bool,
    ) -> Option<(NearMatch, String, usize)> {
        let target = n.as_f64().unwrap_or(0.0);
        self.nearby(context, target, window)
            .into_iter()
            .flat_map(|i| {
                std::iter::once(&self.numbers[i])
                    .chain(self.groups.get(&i).into_iter().flatten())
                    .map(move |s| (i, s))
            })
            .filter_map(|(i, s)| Some((n.near_match(s, rounding)?, i, s)))
            .min_by_key(|(how, ..)| *how)
            .map(|(how, i, s)| {
                let value = match how {
                    NearMatch::Scaled(_) => s.signed_full(),
                    _ => s.signed(),
                };
                (how, value, self.number_line(i))
            })
    }

    fn number_line(&self, i: usize) -> usize {
        self.line_of(self.norm[self.numbers[i].start].off as usize)
    }

    /// Indexes into `numbers` of every number near a context word, ranked as
    /// [`IndexedText::nearby_numbers`] describes.
    fn nearby(&self, context: &[String], target: f64, window: usize) -> Vec<usize> {
        let mut score: HashMap<usize, u32> = HashMap::new();
        for word in context {
            let Some(positions) = self.words.get(word) else {
                continue;
            };
            let mut seen_here: Vec<usize> = Vec::new();
            for &p in positions.iter().take(500) {
                let p = self.squeezed[p as usize] as usize;
                let at = |t: &NumTok| self.squeezed[t.start] as usize;
                let lo = self.numbers.partition_point(|t| at(t) + window < p);
                for (i, t) in self.numbers[lo..].iter().enumerate() {
                    if at(t) > p + window {
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
        ranked.into_iter().map(|(_, _, i)| i).collect()
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

pub(crate) fn hex(bytes: &[u8]) -> String {
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
    fn bounded_search_does_not_cut_words_or_numbers() {
        let t = IndexedText::new(
            "Our method reaches 12.3 % accuracy on the catalog, a retriev-\nal gain of \u{2212}2.1 points.".into(),
        );
        let find = |q: &str| t.find_bounded(&normalize::skeleton_str(q), 0);
        assert!(find("our method reaches 12.3 %").is_some());
        assert!(find("our method reaches 12").is_none());
        // The percent sign belongs to the number.
        assert!(find("reaches 12.3").is_none());
        assert!(find("12.3% accuracy").is_some());
        assert!(find("he catalog").is_none());
        assert!(find("on the cat").is_none());
        assert!(find("retriev").is_none());
        assert!(find("retrieval gain").is_some());
        assert!(find("gain of 2.1 points").is_none());
        assert!(find("gain of -2.1 points").is_some());
        // Whitespace separates numbers, as in the number scanner.
        let t = IndexedText::new("Scores 68.56\n66.31 on March 31, 2021 and 1,234 runs.".into());
        let find = |q: &str| t.find_bounded(&normalize::skeleton_str(q), 0);
        assert!(find("66.31 on March").is_some());
        assert!(find("2021 and").is_some());
        assert!(find("234 runs").is_none());
        let t = IndexedText::new("本研究では金融文書の検索精度を改善する。".into());
        assert!(
            t.find_bounded(&normalize::skeleton_str("検索精度を改善"), 0)
                .is_some()
        );
    }

    #[test]
    fn numbers_are_indexed_canonically() {
        let t = IndexedText::new("Accuracy reaches 12.30 % on 1,234 queries (Table 2).".into());
        assert!(t.has_number("12.3"));
        assert!(t.has_number("1234"));
        assert!(!t.has_number("12.4"));
        let t = IndexedText::new("The gap is \u{2212}2.1 points.".into());
        assert!(t.has_number("-2.1") && t.has_number("2.1"));
        assert!(!IndexedText::new("The gap is 2.1 points.".into()).has_number("-2.1"));
    }

    #[test]
    fn nearby_numbers_rank_by_context() {
        let t = IndexedText::new("Recall is 71.2 on FinQA.\nUnrelated 70.6 value.\n".into());
        let near = t.nearby_numbers(&["recall".into(), "finqa".into()], 70.5, 15, 2);
        assert_eq!(near[0], ("71.2".to_string(), 1));
    }

    #[test]
    fn window_counts_space_runs_as_one_character() {
        // A padded table row: the cell is over 150 characters from its
        // label, but under 150 once each space run counts as one.
        let pad = " ".repeat(40);
        let row = format!("Premium{pad}89{pad}12{pad}7{pad}43.09");
        let t = IndexedText::new(format!("Premium by group\n{row}\n"));
        assert!(row.chars().count() > 150);
        let near = t.nearby_numbers(&["premium".into()], 43.1, 150, 10);
        assert!(near.iter().any(|(v, _)| v == "43.09"), "{near:?}");
        // Text that is truly far away stays out of the window.
        let filler = "word ".repeat(40);
        let t = IndexedText::new(format!("Premium by group\n{filler}\n43.09\n"));
        assert!(
            t.nearby_numbers(&["premium".into()], 43.1, 150, 10)
                .is_empty()
        );
        assert_eq!(
            t.nearby_numbers(&["premium".into()], 43.1, 300, 10).len(),
            1
        );
    }
}
