//! Findings (`rf.finding/1`) and the rule catalogue.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

macro_rules! rules {
    ($($variant:ident => $id:literal, $sev:ident, $summary:literal;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Rule { $($variant),* }

        impl Rule {
            pub const ALL: &[Rule] = &[$(Rule::$variant),*];

            pub fn id(self) -> &'static str {
                match self { $(Rule::$variant => $id),* }
            }

            pub fn default_severity(self) -> Severity {
                match self { $(Rule::$variant => Severity::$sev),* }
            }

            pub fn summary(self) -> &'static str {
                match self { $(Rule::$variant => $summary),* }
            }
        }
    };
}

rules! {
    CitationUnresolved => "citation-unresolved", Error,
        "A citation key is not in the reference registry.";
    CitationDuplicate => "citation-duplicate", Warn,
        "A citation uses a key whose registry entry is a duplicate of another key.";
    CitationLabelMismatch => "citation-label-mismatch", Error,
        "A citation's visible number differs from the key in its link target.";
    QuoteNotFound => "quote-not-found", Error,
        "Quoted text attached to a citation does not occur in the cited source text.";
    SourceTextMissing => "source-text-missing", Warn,
        "A check needs the cited source's text, but the registry lists none or it cannot be read.";
    SourceTextChanged => "source-text-changed", Warn,
        "A source text file's SHA-256 differs from the registry's hash; quotes and numbers found only there are unverified.";
    NumberNotInSource => "number-not-in-source", Warn,
        "A number in a citing sentence does not occur in any cited source text.";
    DepthInsufficient => "depth-insufficient", Error,
        "A claim's kind needs deeper inspection than the cited reference's recorded depth.";
    UncitedClaim => "uncited-claim", Warn,
        "A quantitative or comparative sentence has no citation, link or evidence label.";
    EvidenceLabel => "evidence-label", Warn,
        "A paragraph lacks a required evidence label, or uses one outside the vocabulary.";
    ReferenceListMismatch => "reference-list-mismatch", Error,
        "A document's own reference-list entry disagrees with the registry's title, year or first author.";
    StaleKeptQuote => "stale-kept-quote", Warn,
        "A [[claimcheck.kept_quote]] entry matches no unverified quotation in its checked file.";
}

impl Rule {
    pub fn from_id(id: &str) -> Option<Rule> {
        Rule::ALL.iter().copied().find(|r| r.id() == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub path: String,
    pub locator: String,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: Rule,
    pub severity: Severity,
    pub subject: String,
    pub message: String,
    pub evidence: Vec<Evidence>,
    pub data: Map<String, Value>,
    /// Ordering key: input order, then line.
    pub order: (usize, usize),
}

impl Finding {
    pub fn to_json(&self, at: &str) -> Value {
        json!({
            "schema": "rf.finding/1",
            "tool": "claimcheck",
            "rule": self.rule.id(),
            "severity": self.severity,
            "subject": self.subject,
            "message": self.message,
            "evidence": self.evidence,
            "data": self.data,
            "at": at,
        })
    }
}

/// Current UTC time as RFC 3339 with second precision.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339(secs)
}

/// Formats seconds since the Unix epoch (civil-from-days, proleptic Gregorian).
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_ids_are_unique_and_round_trip() {
        for r in Rule::ALL {
            assert_eq!(Rule::from_id(r.id()), Some(*r));
        }
        let mut ids: Vec<_> = Rule::ALL.iter().map(|r| r.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Rule::ALL.len());
    }

    #[test]
    fn severity_display_honours_width() {
        assert_eq!(format!("[{:<5}]", Severity::Info), "[info ]");
        assert_eq!(format!("[{:>5}]", Severity::Warn), "[ warn]");
    }

    #[test]
    fn formats_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_020_400), "2026-11-07T03:00:00Z");
    }

    #[test]
    fn json_shape() {
        let f = Finding {
            rule: Rule::QuoteNotFound,
            severity: Severity::Error,
            subject: "a.md:3".into(),
            message: "m".into(),
            evidence: vec![Evidence {
                path: "t.txt".into(),
                locator: "line 2".into(),
            }],
            data: Map::new(),
            order: (0, 3),
        };
        let v = f.to_json("2026-01-01T00:00:00Z");
        assert_eq!(v["schema"], "rf.finding/1");
        assert_eq!(v["rule"], "quote-not-found");
        assert_eq!(v["severity"], "error");
        assert_eq!(v["evidence"][0]["locator"], "line 2");
    }
}
