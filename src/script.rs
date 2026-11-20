//! Traditional and simplified Chinese characters.
//!
//! A quotation in simplified characters of a text in traditional characters,
//! or the reverse, is not verbatim, but it is no wrong text either. The table
//! is OpenCC's `TSCharacters.txt` (see `data/opencc/README.md`): each
//! traditional character with its simplified forms.

use std::collections::HashMap;
use std::sync::LazyLock;

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

/// True when `a` and `b` are distinct characters and one is a simplified
/// form of the other. Two traditional characters that share a simplified
/// form (`發` and `髮`) are not such a pair, and neither are variant forms
/// of one script (`爲` and `為`).
pub fn differ_in_script(a: char, b: char) -> bool {
    let simplifies = |t: char, s: char| SIMPLIFIED.get(&t).is_some_and(|f| f.contains(&s));
    a != b && (simplifies(a, b) || simplifies(b, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs() {
        assert!(SIMPLIFIED.len() > 4000);
        assert!(differ_in_script('學', '学'));
        assert!(differ_in_script('学', '學'));
        // A second simplified form of the table.
        assert!(differ_in_script('乾', '干'));
        assert!(!differ_in_script('學', '學'));
        assert!(!differ_in_script('發', '髮'));
        assert!(!differ_in_script('爲', '為'));
        assert!(!differ_in_script('悦', '說'));
        assert!(!differ_in_script('a', 'b'));
    }
}
