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
//!
//! A document number that misses every exact key may still match a nearby
//! source number approximately: see [`NumTok::near_match`].

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
    /// Followed by `%`, `‰` or `pp`.
    pub percent: bool,
    /// Followed by `‰`: per mille, not per cent.
    pub permille: bool,
    pub decimal: bool,
    /// Fractional digits as written, trailing zeros included (`8.20` has 2).
    pub places: usize,
    pub separated: bool,
    /// Directly preceded by a letter (an identifier such as `F1` or `H133`).
    pub glued_left: bool,
    /// Directly followed by a letter that is not a recognized unit.
    pub glued_right: bool,
    /// More than one decimal point (`1.2.3`), or a dotted identifier.
    pub dotted: bool,
    /// Preceded by a minus sign that is not a hyphen or range dash.
    pub negative: bool,
    /// Preceded by a dash right after a percent sign, as in `52%-90%`: a
    /// range, but in extracted source text also table cells run together
    /// (`-18.94%-43.50%`), so a source indexes such a number under both signs.
    pub percent_dash: bool,
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
    /// negative number, or one after a [`NumTok::percent_dash`], also the
    /// signed values.
    pub fn index_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.values().map(str::to_string).collect();
        if self.negative || self.percent_dash {
            keys.extend(self.values().map(|v| format!("-{v}")).collect::<Vec<_>>());
        }
        keys
    }

    /// Keys a document number looks up in a source index.
    pub fn lookup_keys(&self) -> Vec<String> {
        let sign = if self.negative { "-" } else { "" };
        self.values().map(|v| format!("{sign}{v}")).collect()
    }

    /// Significant digits as written: every digit from the first non-zero
    /// one, so `8` has 1, `0.082` has 2 and `8.20` has 3.
    fn significant_digits(&self) -> usize {
        let (int, frac) = self.value.split_once('.').unwrap_or((&self.value, ""));
        let digits = int.len() + frac.len();
        let leading = int
            .bytes()
            .chain(frac.bytes())
            .take_while(|b| *b == b'0')
            .count();
        digits - leading + self.places.saturating_sub(frac.len())
    }

    /// How this document number matches source number `s` when their values
    /// differ, if it does. A negative document number needs a negative `s`
    /// (or one after a [`NumTok::percent_dash`]), as for exact matches.
    ///
    /// - Rounded: `s` has more fractional digits than this number has as
    ///   written, and rounding `s` half-up or half-even to those digits
    ///   gives this value (`8.016` as `8.02`, `1030.90` as `1,031`).
    ///   Truncation does not match (`1218.66` as `1218`).
    /// - Percent: this number is a percentage (`%` or `pp`, not `‰`), `0 < |s| < 1` and `s` has no
    ///   percent sign, and `s` × 100 equals this value
    ///   (`0.082` as `8.2%`), or rounds to it as above.
    ///
    /// Rounding needs `rounding` and at least 2 significant digits in this
    /// number, so `8` never matches `7.6`.
    pub fn near_match(&self, s: &NumTok, rounding: bool) -> Option<NearMatch> {
        if self.negative && !(s.negative || s.percent_dash) {
            return None;
        }
        let rounds = |value: &str| {
            rounding
                && self.significant_digits() >= 2
                && rounded(value, self.places).is_some_and(|r| r.contains(&self.value))
        };
        if rounds(&s.value) {
            return Some(NearMatch::Rounded);
        }
        if !self.percent || self.permille || s.percent {
            return None;
        }
        let frac = s.value.strip_prefix("0.")?;
        let pct = shifted("0", frac, 2);
        if pct == self.value {
            Some(NearMatch::Percent)
        } else if rounds(&pct) {
            Some(NearMatch::RoundedPercent)
        } else {
            None
        }
    }
}

/// How a document number matched a source number with another value, from
/// the closest kind of match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NearMatch {
    /// A fraction written as a percentage.
    Percent,
    Rounded,
    /// A fraction written as a rounded percentage.
    RoundedPercent,
}

impl NearMatch {
    pub fn as_str(self) -> &'static str {
        match self {
            NearMatch::Rounded => "rounded",
            NearMatch::Percent => "percent",
            NearMatch::RoundedPercent => "rounded-percent",
        }
    }
}

/// `value`, a canonical unsigned decimal, rounded to `places` fractional
/// digits half-up and half-even, as canonical values. `None` unless `value`
/// has more than `places` fractional digits.
fn rounded(value: &str, places: usize) -> Option<[String; 2]> {
    let (int, frac) = value.split_once('.')?;
    if frac.len() <= places {
        return None;
    }
    let (keep, rest) = frac.split_at(places);
    let digits: Vec<u8> = int.bytes().chain(keep.bytes()).collect();
    let first = rest.as_bytes()[0];
    let half = first == b'5' && rest[1..].bytes().all(|b| b == b'0');
    let odd = digits.last().is_some_and(|d| (d - b'0') % 2 == 1);
    let round = |up: bool| {
        let mut d = digits.clone();
        if up {
            let mut i = d.len();
            loop {
                if i == 0 {
                    d.insert(0, b'1');
                    break;
                }
                i -= 1;
                if d[i] == b'9' {
                    d[i] = b'0';
                } else {
                    d[i] += 1;
                    break;
                }
            }
        }
        let (int, frac) = d.split_at(d.len() - places);
        // Digits are ASCII, so both halves are valid UTF-8.
        canonical(
            std::str::from_utf8(int).unwrap_or_default(),
            std::str::from_utf8(frac).unwrap_or_default(),
        )
    };
    Some([
        round(first >= b'5'),
        round(first > b'5' || (first == b'5' && (!half || odd))),
    ])
}

/// Numbers written with a space, or a comma and a space, between thousands
/// groups: `1, 590, 436` and `1 454`, which [`scan`] reads as separate
/// numbers. Each is returned as the index of its first token in `toks` and
/// its joined canonical value. A run starts with one to three digits and
/// continues with groups of exactly three; a group with a fraction or glued
/// to a letter ends it, so `1 454.5` and `12 345abc` are not joined.
pub fn split_groups(chars: &[char], toks: &[NumTok]) -> Vec<(usize, String)> {
    let gap = |a: &NumTok, b: &NumTok| matches!(&chars[a.end..b.start], [' '] | [',', ' ']);
    let plain = |t: &NumTok| {
        !t.decimal && !t.separated && !t.dotted && !t.glued_right && t.scaled.is_none()
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let starts = plain(t)
            && !t.percent
            && !t.glued_left
            && t.end - t.start <= 3
            && !(i > 0 && gap(&toks[i - 1], t));
        let mut j = i;
        while starts
            && j + 1 < toks.len()
            && !toks[j].percent
            && gap(&toks[j], &toks[j + 1])
            && plain(&toks[j + 1])
            && toks[j + 1].end - toks[j + 1].start == 3
        {
            j += 1;
        }
        if j > i {
            let digits: String = toks[i..=j]
                .iter()
                .flat_map(|t| &chars[t.start..t.end])
                .collect();
            out.push((i, canonical(&digits, "")));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
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
        // "-2.1" or "(-2.1)", not "10-20", "52%-90%", "BGE-3" or "x-2".
        let dash = start > 1 && chars[start - 1] == '-';
        let percent_dash = dash && matches!(chars[start - 2], '%' | '‰');
        let negative = start > 0
            && chars[start - 1] == '-'
            && (start == 1 || !(chars[start - 2].is_alphanumeric() || percent_dash));
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
        let permille = at(after_space) == Some('‰');
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
            permille,
            decimal,
            places: frac.len(),
            separated,
            glued_left,
            glued_right,
            dotted,
            negative,
            percent_dash,
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

    #[test]
    fn dash_after_a_percent_is_a_range() {
        for s in ["52%–90%", "48%-73%", "2‰-5‰", "4 pp-6 pp"] {
            let norm: Vec<char> = crate::normalize::normalize_str(s)
                .iter()
                .map(|n| n.c)
                .collect();
            let toks = scan(&norm);
            assert_eq!(toks.len(), 2, "{s}");
            assert!(toks.iter().all(|t| !t.negative), "{s}: {toks:?}");
        }
        let toks = scan(&"52%-90%".chars().collect::<Vec<_>>());
        assert!(toks[1].percent_dash && !toks[0].percent_dash);
        // A source indexes it under both signs: cells may have run together.
        assert_eq!(toks[1].index_keys(), ["90", "-90"]);
        assert!(one("fell to -2.1%").negative);
        assert!(one("(-0.5%)").negative);
        assert!(!one("(-0.5%)").percent_dash);
    }

    fn near(doc: &str, src: &str, rounding: bool) -> Option<NearMatch> {
        one(doc).near_match(&one(src), rounding)
    }

    #[test]
    fn rounded_values_match() {
        assert_eq!(near("8.02", "8.016", true), Some(NearMatch::Rounded));
        assert_eq!(near("1,031", "1030.90", true), Some(NearMatch::Rounded));
        assert_eq!(near("41.9", "41.8713", true), Some(NearMatch::Rounded));
        assert_eq!(near("-14,451", "-14451.4", true), Some(NearMatch::Rounded));
        assert_eq!(near("8.20", "8.196", true), Some(NearMatch::Rounded));
        assert_eq!(near("10.0", "9.96", true), Some(NearMatch::Rounded));
        // Half-up and half-even both count.
        assert_eq!(near("8.03", "8.025", true), Some(NearMatch::Rounded));
        assert_eq!(near("8.02", "8.025", true), Some(NearMatch::Rounded));
        assert_eq!(near("8.04", "8.035", true), Some(NearMatch::Rounded));
        assert_eq!(near("8.03", "8.035", true), None);
        // An unsigned number matches either sign, a negative one only a negative.
        assert_eq!(near("4.47", "-4.4749", true), Some(NearMatch::Rounded));
        assert_eq!(near("-4.47", "4.4749", true), None);
    }

    #[test]
    fn rounding_needs_more_digits_two_significant_digits_and_the_switch() {
        // Truncation is not rounding.
        assert_eq!(near("1,218%", "1,218.66%", true), None);
        assert_eq!(near("0.002995", "0.002995509", true), None);
        // Fewer than 2 significant digits.
        assert_eq!(near("8", "7.6", true), None);
        assert_eq!(near("0.07", "0.06849", true), None);
        assert_eq!(near("0.068", "0.06849", true), Some(NearMatch::Rounded));
        // The source must have more fractional digits than the document.
        assert_eq!(near("8.02", "8.0", true), None);
        assert_eq!(near("8.02", "8.016", false), None);
    }

    #[test]
    fn fractions_match_percentages() {
        assert_eq!(near("8.2%", "+0.082", true), Some(NearMatch::Percent));
        assert_eq!(near("15.8%", "0.158", false), Some(NearMatch::Percent));
        assert_eq!(near("5%", "0.05", true), Some(NearMatch::Percent));
        assert_eq!(
            near("15.8%", "0.1578", true),
            Some(NearMatch::RoundedPercent)
        );
        assert_eq!(near("15.8%", "0.1578", false), None);
        assert_eq!(near("-8.2%", "-0.082", true), Some(NearMatch::Percent));
        // Only a percentage, only a fraction below 1 and not one with a percent sign.
        assert_eq!(near("8.2", "0.082", true), None);
        assert_eq!(near("120%", "1.2", true), None);
        assert_eq!(near("8.2%", "0.082%", true), None);
        assert_eq!(near("8.2 pp", "0.082", true), Some(NearMatch::Percent));
        assert_eq!(near("8.2‰", "0.082", true), None);
        assert_eq!(near("8.2‰", "8.21", true), Some(NearMatch::Rounded));
        assert_eq!(near("0%", "0", true), None);
    }

    #[test]
    fn rounding_carries() {
        assert_eq!(rounded("9.996", 2).unwrap(), ["10", "10"]);
        assert_eq!(rounded("0.0049", 2).unwrap(), ["0", "0"]);
        assert_eq!(rounded("2.5", 0).unwrap(), ["3", "2"]);
        assert_eq!(rounded("8.2", 1), None);
    }

    fn groups(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        split_groups(&chars, &scan(&chars))
            .into_iter()
            .map(|(_, v)| v)
            .collect()
    }

    #[test]
    fn space_split_thousands_groups() {
        assert_eq!(groups("users 1, 590, 436 in total"), ["1590436"]);
        assert_eq!(
            groups("profit $1 454 and 26, 603, 038"),
            ["1454", "26603038"]
        );
        assert_eq!(groups("1 045"), ["1045"]);
        // A group of other than three digits, a fraction or a glued letter ends a run.
        assert!(groups("12 34 and 1 2345 and 1 454.5 and 12 345abc").is_empty());
        assert!(groups("1,234 567 and 1.5 234").is_empty());
        // A run starts at its first group.
        assert_eq!(groups("2020 454 123"), Vec::<String>::new());
        assert_eq!(groups("12 345 678"), ["12345678"]);
    }
}
