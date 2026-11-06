//! Number tokens and their canonical forms.
//!
//! The scanner runs on normalized characters (see [`crate::normalize`]), so
//! full-width digits are already ASCII and dash variants are `-`. A token's
//! canonical value drops thousands separators, leading zeros and trailing
//! fractional zeros: `1,234`, `1234` and `1234.0` all become `1234`; `.5` and
//! `0.50` become `0.5`. Percent signs and spacing do not change the value, so
//! `12.3%`, `12.3 %` and a table cell `12.3` match. A minus sign is kept
//! apart: a negative number in a document needs a negative number in the
//! source, while an unsigned one ("fell by 2.1") also matches `-2.1`.

/// A number found in normalized text. Positions are indices into the scanned
/// character slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumTok {
    pub start: usize,
    pub end: usize,
    /// Canonical value.
    pub value: String,
    /// Value with a `k`, `M` or `B` multiplier applied, when one follows.
    pub scaled: Option<String>,
    pub percent: bool,
    pub decimal: bool,
    pub separated: bool,
    /// Directly preceded by a letter (an identifier such as `F1` or `H133`).
    pub glued_left: bool,
    /// Directly followed by a letter that is not a recognized unit.
    pub glued_right: bool,
    /// More than one decimal point (`1.2.3`), or a dotted identifier.
    pub dotted: bool,
    /// Preceded by a minus sign that is not a hyphen or range dash.
    pub negative: bool,
}

impl NumTok {
    /// The canonical values a matching source number may have.
    pub fn values(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.value.as_str()).chain(self.scaled.as_deref())
    }

    /// Integer value, when the token has no fraction or separator.
    pub fn as_plain_integer(&self) -> Option<u64> {
        if self.decimal || self.separated {
            return None;
        }
        self.value.parse().ok()
    }

    pub fn as_f64(&self) -> Option<f64> {
        let v: f64 = self.value.parse().ok()?;
        Some(if self.negative { -v } else { v })
    }

    /// Canonical value with its sign, for display.
    pub fn signed(&self) -> String {
        if self.negative {
            format!("-{}", self.value)
        } else {
            self.value.clone()
        }
    }

    /// Keys under which a source number is indexed: its values, and for a
    /// negative number also the signed values.
    pub fn index_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.values().map(str::to_string).collect();
        if self.negative {
            keys.extend(self.values().map(|v| format!("-{v}")).collect::<Vec<_>>());
        }
        keys
    }

    /// Keys a document number looks up in a source index.
    pub fn lookup_keys(&self) -> Vec<String> {
        let sign = if self.negative { "-" } else { "" };
        self.values().map(|v| format!("{sign}{v}")).collect()
    }
}

fn canonical(int: &str, frac: &str) -> String {
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    }
}

/// Multiplies a decimal number given as integer and fraction digits by 10^shift.
fn shifted(int: &str, frac: &str, shift: usize) -> String {
    let mut digits = String::with_capacity(int.len() + shift);
    digits.push_str(int);
    let mut frac = frac.to_string();
    while frac.len() < shift {
        frac.push('0');
    }
    digits.push_str(&frac[..shift]);
    canonical(&digits, &frac[shift..])
}

fn is_letter(c: char) -> bool {
    c.is_alphabetic()
}

/// Scans every number token in `chars`.
pub fn scan(chars: &[char]) -> Vec<NumTok> {
    let mut out = Vec::new();
    let n = chars.len();
    let at = |i: usize| chars.get(i).copied();
    let digit = |i: usize| at(i).is_some_and(|c| c.is_ascii_digit());
    let mut i = 0;
    while i < n {
        let starts_here = digit(i) || (at(i) == Some('.') && digit(i + 1));
        if !starts_here {
            i += 1;
            continue;
        }
        let start = i;
        let mut int = String::new();
        let mut separated = false;
        while digit(i) {
            int.push(chars[i]);
            i += 1;
        }
        // Thousands groups: ",ddd" not followed by another digit, after at most three digits.
        let group = |i: usize| at(i) == Some(',') && (1..=3).all(|k| digit(i + k)) && !digit(i + 4);
        if int.len() <= 3 && group(i) {
            separated = true;
            while group(i) {
                int.extend(&chars[i + 1..i + 4]);
                i += 4;
            }
        }
        let mut frac = String::new();
        let mut decimal = false;
        if at(i) == Some('.') && digit(i + 1) {
            decimal = true;
            i += 1;
            while digit(i) {
                frac.push(chars[i]);
                i += 1;
            }
        }
        let mut dotted = false;
        // A further ".d" makes a version or section number such as 1.2.3.
        while at(i) == Some('.') && digit(i + 1) {
            dotted = true;
            i += 1;
            while digit(i) {
                i += 1;
            }
        }
        let end = i;
        // "-2.1" or "(-2.1)", not "10-20", "BGE-3" or "x-2".
        let negative = start > 0
            && chars[start - 1] == '-'
            && (start == 1 || !chars[start - 2].is_alphanumeric());
        let glued_left = start > 0 && {
            let p = chars[start - 1];
            is_letter(p)
                || p == '_'
                || ((p == '-' || p == '/' || p == '.') && start > 1 && is_letter(chars[start - 2]))
        };
        // Suffixes: optional space, then a unit.
        let mut percent = false;
        let mut scaled = None;
        let mut glued_right = false;
        let after_space = if at(end) == Some(' ') { end + 1 } else { end };
        let pp = at(after_space) == Some('p')
            && at(after_space + 1) == Some('p')
            && !at(after_space + 2).is_some_and(is_letter);
        if matches!(at(after_space), Some('%' | '‰')) || pp {
            percent = true;
        } else if let Some(c) = at(end).filter(|c| is_letter(*c) || *c == '_') {
            let free = |k: usize| !at(k).is_some_and(|c| is_letter(c) || c == '_');
            let shift = match c {
                'k' | 'K' if free(end + 1) => Some(3),
                'M' if free(end + 1) => Some(6),
                'B' if free(end + 1) => Some(9),
                'b' if at(end + 1) == Some('n') && free(end + 2) => Some(9),
                'x' | '×' if free(end + 1) => Some(0),
                _ => None,
            };
            match shift {
                Some(0) => {}
                Some(s) => scaled = Some(shifted(&int, &frac, s)),
                None => glued_right = true,
            }
        }
        out.push(NumTok {
            start,
            end,
            value: canonical(&int, &frac),
            scaled,
            percent,
            decimal,
            separated,
            glued_left,
            glued_right,
            dotted,
            negative,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        scan(&chars).into_iter().map(|t| t.value).collect()
    }

    fn one(s: &str) -> NumTok {
        let chars: Vec<char> = s.chars().collect();
        let mut toks = scan(&chars);
        assert_eq!(toks.len(), 1, "{s}: {toks:?}");
        toks.remove(0)
    }

    #[test]
    fn canonical_forms() {
        assert_eq!(
            values("1,234 and 1234 and 1234.0"),
            ["1234", "1234", "1234"]
        );
        assert_eq!(values(".5 0.50 00.5"), ["0.5", "0.5", "0.5"]);
        assert_eq!(values("12,345,678"), ["12345678"]);
        assert_eq!(values("1,2"), ["1", "2"]);
    }

    #[test]
    fn percent_with_and_without_space() {
        let a = one("12.3%");
        let b = one("12.3 %");
        assert!(a.percent && b.percent);
        assert_eq!(a.value, b.value);
        assert!(one("4.5 pp gain").percent);
    }

    #[test]
    fn identifiers_are_glued() {
        assert!(one("F1").glued_left);
        assert!(one("H133").glued_left);
        assert!(one("BGE-M3").glued_left);
        assert!(one("12abc").glued_right);
        assert!(!one("(12)").glued_left);
        assert!(one("v2").glued_left);
    }

    #[test]
    fn dotted_versions() {
        assert!(one("1.2.3").dotted);
        assert!(!one("1.2").dotted);
    }

    #[test]
    fn multipliers() {
        assert_eq!(one("1.2k").scaled.as_deref(), Some("1200"));
        assert_eq!(one("3M docs").scaled.as_deref(), Some("3000000"));
        assert_eq!(one("2.5 bn").scaled, None);
        assert_eq!(one("2.5bn").scaled.as_deref(), Some("2500000000"));
        assert!(!one("3x faster").glued_right);
    }

    #[test]
    fn minus_signs() {
        assert!(one("fell to -2.1").negative);
        assert!(one("(-0.5)").negative);
        assert_eq!(one("(-0.5)").lookup_keys(), ["-0.5"]);
        assert_eq!(one("-2.5k").index_keys(), ["2.5", "2500", "-2.5", "-2500"]);
        let chars: Vec<char> = "10-20 BGE-3".chars().collect();
        assert!(scan(&chars).iter().all(|t| !t.negative));
    }

    #[test]
    fn ranges_split_into_endpoints() {
        assert_eq!(values("10-20%"), ["10", "20"]);
        assert_eq!(values("865/1192"), ["865", "1192"]);
    }
}
