//! Text normalization shared by the document and source sides.
//!
//! Two forms are produced:
//!
//! - [`normalize`] keeps every visible character but applies Unicode NFKC
//!   (full-width forms, ligatures, superscripts), unifies quote and dash
//!   variants, and drops soft hyphens and zero-width characters. Each output
//!   character remembers the byte offset of the input character it came from.
//! - [`skeleton`] reduces normalized text to lowercase letters and digits
//!   (plus a few meaning-bearing symbols such as `%` and decimal points). It
//!   removes whitespace, line-break hyphenation and punctuation, so PDF
//!   extraction artifacts such as `retriev-\nal`, `ﬁ`, missing or doubled
//!   spaces and curly quotes do not prevent an exact match.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// A normalized character with the byte offset of the original character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NChar {
    pub c: char,
    pub off: u32,
}

fn unify(c: char) -> Option<char> {
    match c {
        // Soft hyphen, zero-width characters and byte-order marks.
        '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => None,
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{0060}'
        | '\u{00B4}' => Some('\''),
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{00AB}'
        | '\u{00BB}' => Some('"'),
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}' => Some('-'),
        c if c != '\n' && c.is_whitespace() => Some(' '),
        c => Some(c),
    }
}

/// Normalizes `(char, byte offset)` pairs. Combining marks are normalized
/// together with their base character so that NFKC can compose them.
pub fn normalize(chars: &[(char, u32)]) -> Vec<NChar> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    let mut cluster = String::new();
    while i < chars.len() {
        let off = chars[i].1;
        cluster.clear();
        cluster.push(chars[i].0);
        i += 1;
        while i < chars.len() && is_combining_mark(chars[i].0) {
            cluster.push(chars[i].0);
            i += 1;
        }
        if cluster.is_ascii() {
            for c in cluster.chars() {
                if let Some(c) = unify(c) {
                    out.push(NChar { c, off });
                }
            }
        } else {
            for c in cluster.nfkc() {
                if let Some(c) = unify(c) {
                    out.push(NChar { c, off });
                }
            }
        }
    }
    out
}

/// Normalizes a whole string, recording byte offsets into it.
pub fn normalize_str(text: &str) -> Vec<NChar> {
    let pairs: Vec<(char, u32)> = text
        .char_indices()
        .map(|(i, c)| (c, u32::try_from(i).unwrap_or(u32::MAX)))
        .collect();
    normalize(&pairs)
}

/// Symbols that change the meaning of a quotation and survive in the skeleton.
fn is_kept_symbol(c: char) -> bool {
    matches!(
        c,
        '%' | '<' | '>' | '=' | '+' | '±' | '×' | '≤' | '≥' | '≈' | '$' | '€' | '£' | '¥'
    )
}

/// Letters, digits and kept symbols of normalized text, lowercased, with the
/// original byte offset of each output character.
///
/// A decimal point between digits is kept so that `1.5` and `15` differ; a
/// comma is dropped only when it separates thousands. Whitespace between two
/// digits becomes a single space so that `1 5` and `15` differ.
pub fn skeleton(chars: &[NChar]) -> (Vec<char>, Vec<u32>) {
    let mut out = Vec::with_capacity(chars.len());
    let mut offs = Vec::with_capacity(chars.len());
    let next_non_space = |from: usize| {
        chars[from..]
            .iter()
            .map(|n| n.c)
            .find(|c| *c != ' ' && *c != '\n')
    };
    for (i, n) in chars.iter().enumerate() {
        let c = n.c;
        let prev_digit = out.last().is_some_and(|p: &char| p.is_ascii_digit());
        let next = chars.get(i + 1).map(|n| n.c);
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                out.push(l);
                offs.push(n.off);
            }
        } else if is_kept_symbol(c) {
            out.push(c);
            offs.push(n.off);
        } else if c == '.' && prev_digit && next.is_some_and(|d| d.is_ascii_digit()) {
            out.push('.');
            offs.push(n.off);
        } else if c == ',' && prev_digit && next.is_some_and(|d| d.is_ascii_digit()) {
            let digits = chars[i + 1..]
                .iter()
                .take_while(|n| n.c.is_ascii_digit())
                .count();
            if digits != 3 {
                out.push(',');
                offs.push(n.off);
            }
        } else if (c == ' ' || c == '\n')
            && prev_digit
            && next_non_space(i).is_some_and(|d| d.is_ascii_digit())
            && out.last() != Some(&' ')
        {
            out.push(' ');
            offs.push(n.off);
        }
    }
    (out, offs)
}

/// Skeleton of a plain string, without offsets.
pub fn skeleton_str(text: &str) -> String {
    skeleton(&normalize_str(text)).0.into_iter().collect()
}

/// True for characters of scripts written without spaces between words.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xF900..=0xFAFF // compatibility ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0x20000..=0x2FA1F)
}

/// Collapses whitespace and truncates to `max` characters for display.
pub fn excerpt(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        if count >= max {
            out.push('…');
            return out;
        }
        out.push(c);
        count += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_ligatures_and_full_width() {
        assert_eq!(
            skeleton_str("ﬁnancial ＲＡＧ １２．５％"),
            "financialrag12.5%"
        );
    }

    #[test]
    fn hyphenation_across_line_breaks() {
        assert_eq!(
            skeleton_str("evidence retriev-\nal is hard"),
            skeleton_str("evidence retrieval is hard")
        );
        assert_eq!(
            skeleton_str("state-of-the-\nart"),
            skeleton_str("state-of-the-art")
        );
    }

    #[test]
    fn quotes_dashes_and_spacing_variants() {
        assert_eq!(
            skeleton_str("“Don’t  stop” — now"),
            skeleton_str("\"Don't stop\" - now")
        );
        assert_eq!(skeleton_str("soft\u{00AD}hyphen"), "softhyphen");
    }

    #[test]
    fn digits_keep_their_structure() {
        assert_ne!(skeleton_str("1.5 points"), skeleton_str("15 points"));
        assert_ne!(skeleton_str("1 5"), skeleton_str("15"));
        assert_eq!(skeleton_str("1,234 rows"), skeleton_str("1234 rows"));
        assert_ne!(skeleton_str("1,2"), skeleton_str("12"));
    }

    #[test]
    fn combining_marks_compose() {
        assert_eq!(skeleton_str("Cafe\u{0301}"), skeleton_str("Café"));
    }

    #[test]
    fn offsets_point_into_the_original() {
        let text = "ab ﬁ";
        let n = normalize_str(text);
        let (sk, offs) = skeleton(&n);
        assert_eq!(sk, vec!['a', 'b', 'f', 'i']);
        assert_eq!(offs, vec![0, 1, 3, 3]);
    }

    #[test]
    fn excerpt_truncates() {
        assert_eq!(excerpt("a  b\n c", 10), "a b c");
        assert_eq!(excerpt("abcdef", 3), "abc…");
    }
}
