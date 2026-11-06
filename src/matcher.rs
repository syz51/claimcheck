//! Approximate location of a skeleton string inside a source text.
//!
//! Candidate regions come from character trigram votes along alignment
//! diagonals; each candidate is scored with a semi-global edit distance (the
//! whole query against any substring of the region). Working on skeleton
//! characters makes the comparison character-based, which also suits Chinese
//! and Japanese text where words are not separated by spaces.

use std::collections::HashMap;

use crate::source::IndexedText;

/// Postings longer than this carry no locating information.
const MAX_POSTINGS: usize = 20_000;
/// Regions aligned per query.
const CANDIDATES: usize = 6;
/// Below this many cells, align against the whole text instead of voting.
const FULL_SCAN_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Approx {
    /// `1 - edit_distance / query_length`, clamped at 0.
    pub similarity: f64,
    /// Skeleton character range of the best placement.
    pub start: usize,
    pub end: usize,
}

/// Semi-global alignment: the minimal edit distance between `q` and any
/// substring of `w`, with that substring's range. Ties prefer the shorter span.
pub fn align(q: &[char], w: &[char]) -> (usize, usize, usize) {
    let m = q.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut prev_start = vec![0usize; m + 1];
    let mut cur = vec![0usize; m + 1];
    let mut cur_start = vec![0usize; m + 1];
    let mut best = (m, 0, 0);
    for j in 1..=w.len() {
        cur[0] = 0;
        cur_start[0] = j;
        let wc = w[j - 1];
        for i in 1..=m {
            let mut d = prev[i - 1] + usize::from(q[i - 1] != wc);
            let mut s = prev_start[i - 1];
            if prev[i] + 1 < d {
                d = prev[i] + 1;
                s = prev_start[i];
            }
            if cur[i - 1] + 1 < d {
                d = cur[i - 1] + 1;
                s = cur_start[i - 1];
            }
            cur[i] = d;
            cur_start[i] = s;
        }
        let span = j - cur_start[m];
        if cur[m] < best.0 || (cur[m] == best.0 && span < best.2 - best.1) {
            best = (cur[m], cur_start[m], j);
        }
        std::mem::swap(&mut prev, &mut cur);
        std::mem::swap(&mut prev_start, &mut cur_start);
    }
    best
}

fn similarity(dist: usize, m: usize) -> f64 {
    if m == 0 {
        return 0.0;
    }
    (1.0 - dist as f64 / m as f64).max(0.0)
}

/// Best approximate placement of `q` in the text's skeleton, or `None` when
/// the text shares no trigram with the query.
pub fn best_approx(text: &IndexedText, q: &[char]) -> Option<Approx> {
    let m = q.len();
    let n = text.skel.len();
    if m == 0 || n == 0 {
        return None;
    }
    if m.saturating_mul(n) <= FULL_SCAN_CELLS || m < 3 {
        let (d, s, e) = align(q, &text.skel);
        return Some(Approx {
            similarity: similarity(d, m),
            start: s,
            end: e,
        });
    }
    let bucket = (m / 4).max(8) as i64;
    let mut votes: HashMap<i64, u32> = HashMap::new();
    for (i, w) in q.windows(3).enumerate() {
        let postings = text.grams(&[w[0], w[1], w[2]]);
        if postings.len() > MAX_POSTINGS {
            continue;
        }
        for &p in postings {
            *votes
                .entry((i64::from(p) - i as i64).div_euclid(bucket))
                .or_default() += 1;
        }
    }
    let mut ranked: Vec<(i64, u32)> = votes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut best: Option<Approx> = None;
    let mut done: Vec<i64> = Vec::new();
    for (d, _) in ranked {
        if done.len() == CANDIDATES {
            break;
        }
        if done.iter().any(|x| (x - d).abs() <= 1) {
            continue;
        }
        done.push(d);
        let pad = m as i64 / 2 + bucket;
        let lo = (d * bucket - pad).clamp(0, n as i64) as usize;
        let hi = (d * bucket + m as i64 + pad).clamp(0, n as i64) as usize;
        if lo >= hi {
            continue;
        }
        let (dist, s, e) = align(q, &text.skel[lo..hi]);
        let cand = Approx {
            similarity: similarity(dist, m),
            start: lo + s,
            end: lo + e,
        };
        if best.is_none_or(|b| cand.similarity > b.similarity) {
            best = Some(cand);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::skeleton_str;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn align_finds_substring() {
        let (d, s, e) = align(&chars("abc"), &chars("xxabcxx"));
        assert_eq!((d, s, e), (0, 2, 5));
        let (d, _, _) = align(&chars("abd"), &chars("xxabcxx"));
        assert_eq!(d, 1);
    }

    #[test]
    fn locates_a_misquote_in_a_long_text() {
        let filler: String = (0..3000)
            .map(|i| format!("token{} ", i * 7919 % 10007))
            .collect();
        let body = format!(
            "{filler} the retriever gathers complementary evidence before answering {filler}"
        );
        let text = IndexedText::new(body);
        let q = chars(&skeleton_str(
            "the retriever gathers supplementary evidence before answering",
        ));
        let hit = best_approx(&text, &q).unwrap();
        assert!(hit.similarity > 0.85 && hit.similarity < 1.0, "{hit:?}");
        let (s, e) = text.skel_span(hit.start, hit.end);
        assert!(text.text[s..e].contains("evidence before"));
    }

    #[test]
    fn cjk_is_character_based() {
        let text =
            IndexedText::new("本研究では金融文書の検索精度を改善する手法を提案する。".into());
        let q = chars(&skeleton_str("金融文書の検索性能を改善する"));
        let hit = best_approx(&text, &q).unwrap();
        assert!(hit.similarity > 0.8 && hit.similarity < 1.0, "{hit:?}");
    }
}
