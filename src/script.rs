//! Traditional and simplified Chinese characters.
//!
//! A quotation in simplified characters of a text in traditional characters,
//! or the reverse, is not verbatim, but it is no wrong text either. The table
//! is OpenCC's `TSCharacters.txt` (see `data/opencc/README.md`): each
//! traditional character with its simplified forms.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::normalize;

static SIMPLIFIED: LazyLock<HashMap<char, Vec<char>>> = LazyLock::new(|| {
    include_str!("../data/opencc/TSCharacters.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (key, values) = l.split_once('\t')?;
            let mut key = key.chars();
            let (Some(k), None) = (key.next(), key.next()) else {
                return None;
            };
            let forms: Vec<char> = values
                .split(' ')
                .filter_map(|v| {
                    let mut v = v.chars();
                    match (v.next(), v.next()) {
                        (Some(c), None) if c != k => Some(c),
                        _ => None,
                    }
                })
                .collect();
            Some((k, forms))
        })
        .collect()
});

/// The script of a character of a quotation, against the source character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Simplified,
    Traditional,
}

/// The script of `quote` when it and `source` are distinct characters and one
/// is a simplified form of the other; `None` otherwise. Two traditional
/// characters that share a simplified form (`發` and `髮`) are not such a
/// pair, and neither are variant forms of one script (`爲` and `為`).
pub fn script_of(quote: char, source: char) -> Option<Script> {
    // Most pairs of a search are not two CJK characters: no table lookup.
    if quote == source || !normalize::is_cjk(quote) || !normalize::is_cjk(source) {
        return None;
    }
    let simplifies = |t: char, s: char| SIMPLIFIED.get(&t).is_some_and(|f| f.contains(&s));
    if simplifies(source, quote) {
        Some(Script::Simplified)
    } else if simplifies(quote, source) {
        Some(Script::Traditional)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs() {
        assert!(SIMPLIFIED.len() > 4000);
        // Every character of the table is one `is_cjk` knows.
        for (k, forms) in SIMPLIFIED.iter() {
            assert!(normalize::is_cjk(*k), "{k}");
            assert!(forms.iter().all(|f| normalize::is_cjk(*f)), "{k}");
        }
        assert_eq!(script_of('学', '學'), Some(Script::Simplified));
        assert_eq!(script_of('學', '学'), Some(Script::Traditional));
        // A second simplified form of the table.
        assert_eq!(script_of('干', '乾'), Some(Script::Simplified));
        assert_eq!(script_of('學', '學'), None);
        assert_eq!(script_of('發', '髮'), None);
        assert_eq!(script_of('爲', '為'), None);
        assert_eq!(script_of('悦', '說'), None);
        assert_eq!(script_of('a', 'b'), None);
    }
}
