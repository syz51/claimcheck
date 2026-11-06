//! `research.toml` discovery and the `[claimcheck]` table.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::Error;
use crate::finding::{Rule, Severity};
use crate::registry::Depth;

/// Default citation patterns: `[N](references.md#reference-N)`, `[N]` and `[@key]`.
pub const DEFAULT_CITATION_PATTERNS: &[&str] = &[
    r"\[(?P<label>\d+)\]\([^)\s]*#reference-(?P<key>\d+)\)",
    r"\[(?P<keys>[1-9]\d*(?:\s*[,;\x{2013}-]\s*[1-9]\d*)*)(?:\s*;\s*[^\[\]\n\d\s][^\[\]\n]*)?\]",
    r"\[(?P<keys>[^\[\]]*@[^\[\]]+)\]",
];

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotesConfig {
    /// Quotations shorter than this many words (and `min_cjk_chars` CJK
    /// characters) are terms, not quotations, and are not checked.
    pub min_words: usize,
    pub min_cjk_chars: usize,
    /// Best similarity below this is an error; between it and an exact match, a warning.
    pub error_below: f64,
    pub excerpt_chars: usize,
}

impl Default for QuotesConfig {
    fn default() -> Self {
        QuotesConfig {
            min_words: 3,
            min_cjk_chars: 6,
            error_below: 0.8,
            excerpt_chars: 200,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NumbersConfig {
    /// Plain integers below this are not checked (percentages, decimals and
    /// numbers with thousands separators always are).
    pub min_integer: u64,
    pub exclude_years: bool,
    pub year_min: u64,
    pub year_max: u64,
    /// Normalized characters around a context word searched for nearby numbers.
    pub context_window: usize,
    pub nearest: usize,
}

impl Default for NumbersConfig {
    fn default() -> Self {
        NumbersConfig {
            min_integer: 100,
            exclude_years: true,
            year_min: 1900,
            year_max: 2100,
            context_window: 150,
            nearest: 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaimsToml {
    /// Claim kind → words or phrases (case-insensitive, whole words).
    pub kinds: BTreeMap<String, Vec<String>>,
    /// Claim kind → minimum reference depth. When absent, the defaults
    /// (`result` and `method` need `method`) apply to the kinds that exist.
    pub required_depth: Option<BTreeMap<String, Depth>>,
    /// Kind suggested by a checked number in the sentence. Numbers alone
    /// give info, never an error.
    pub number_kind: Option<String>,
    /// A keyword counts toward an error only within this many characters of
    /// the citation, in the same clause; farther ones give info.
    pub window_chars: usize,
}

impl Default for ClaimsToml {
    fn default() -> Self {
        let mut kinds = BTreeMap::new();
        kinds.insert(
            "result".to_string(),
            words(&[
                "outperform",
                "outperforms",
                "outperformed",
                "achieves",
                "achieved",
                "improves",
                "improvement",
                "accuracy",
                "state of the art",
                "state-of-the-art",
                "sota",
                "surpasses",
                "surpassed",
                "beats",
                "better than",
                "worse than",
                "results show",
                "we find",
                "finds that",
                "found that",
                "shows that",
                "show that",
                "demonstrates",
                "demonstrated",
                "gains",
                "reduces",
                "increases",
                "significantly",
            ]),
        );
        kinds.insert(
            "method".to_string(),
            words(&[
                "proposes",
                "proposed",
                "introduces",
                "introduced",
                "algorithm",
                "pipeline",
                "trains",
                "trained",
                "fine-tunes",
                "fine-tuned",
                "implements",
                "implemented",
                "employs",
                "uses",
                "applies",
                "loss function",
                "ablation",
            ]),
        );
        ClaimsToml {
            kinds,
            required_depth: None,
            number_kind: Some("result".to_string()),
            window_chars: 80,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UncitedToml {
    pub comparative: Vec<String>,
    /// A link that is not a citation (to a local file or URL) anywhere in the
    /// paragraph counts as evidence.
    pub links_are_evidence: bool,
}

impl Default for UncitedToml {
    fn default() -> Self {
        UncitedToml {
            comparative: words(&[
                "outperform",
                "outperforms",
                "outperformed",
                "outperforming",
                "state of the art",
                "state-of-the-art",
                "sota",
                "first to",
                "surpasses",
                "surpassed",
                "beats",
                "better than",
                "worse than",
                "superior to",
                "inferior to",
                "more accurate than",
                "best-performing",
            ]),
            links_are_evidence: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelRequire {
    /// Only check labels that are present.
    None,
    /// Paragraphs that cite or make a quantitative or comparative claim need a label.
    Claims,
    /// Every prose paragraph needs a label.
    All,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabelsConfig {
    pub vocabulary: Vec<String>,
    pub require: LabelRequire,
    /// Label → minimum depth of every reference the paragraph cites.
    pub min_depth: BTreeMap<String, Depth>,
}

impl Default for LabelsConfig {
    fn default() -> Self {
        LabelsConfig {
            vocabulary: Vec::new(),
            require: LabelRequire::None,
            min_depth: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReferenceListConfig {
    /// Headings (case-insensitive) that start a document's own reference list.
    pub headings: Vec<String>,
    /// Minimum similarity of the registry title to the entry text.
    pub title_similarity: f64,
}

impl Default for ReferenceListConfig {
    fn default() -> Self {
        ReferenceListConfig {
            headings: words(&[
                "references",
                "reference list",
                "bibliography",
                "works cited",
                "literature cited",
                "sources",
                "参考文献",
                "引用文献",
            ]),
            title_similarity: 0.9,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CitationsToml {
    patterns: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ClaimcheckToml {
    registry: Option<PathBuf>,
    paths: Vec<PathBuf>,
    exclude: Vec<PathBuf>,
    citations: CitationsToml,
    quotes: QuotesConfig,
    numbers: NumbersConfig,
    claims: ClaimsToml,
    uncited: UncitedToml,
    labels: LabelsConfig,
    reference_list: ReferenceListConfig,
    severity: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct PapersToml {
    registry: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct ResearchToml {
    #[serde(default)]
    papers: PapersToml,
    #[serde(default)]
    claimcheck: ClaimcheckToml,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeverityOverride {
    Off,
    Set(Severity),
}

/// A compiled citation pattern.
#[derive(Debug)]
pub struct CitationPattern {
    pub regex: Regex,
}

#[derive(Debug)]
pub struct Claims {
    pub kinds: Vec<(String, Regex)>,
    pub required_depth: BTreeMap<String, Depth>,
    pub number_kind: Option<String>,
    pub window_chars: usize,
}

#[derive(Debug)]
pub struct Uncited {
    pub comparative: Option<Regex>,
    pub links_are_evidence: bool,
}

/// Fully resolved configuration.
#[derive(Debug)]
pub struct Config {
    /// Project root: the directory of `research.toml`, or the working directory.
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub registry: PathBuf,
    pub paths: Vec<PathBuf>,
    pub exclude: Vec<PathBuf>,
    pub citations: Vec<CitationPattern>,
    pub quotes: QuotesConfig,
    pub numbers: NumbersConfig,
    pub claims: Claims,
    pub uncited: Uncited,
    pub labels: LabelsConfig,
    pub reference_list: ReferenceListConfig,
    pub severity: HashMap<Rule, SeverityOverride>,
}

/// Words that make a following claim hypothetical or normative ("must
/// outperform", "whether it improves") rather than asserted.
const MODALS: &[&str] = &[
    "must", "should", "could", "would", "might", "may", "can", "cannot", "whether", "if", "unless",
];

/// The words of `text` with their byte offsets.
fn words_at(text: &str) -> impl DoubleEndedIterator<Item = (usize, &str)> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(move |w| (w.as_ptr().addr() - text.as_ptr().addr(), w))
}

/// True when the word `w` at byte `at` of the sentence `text` is one of
/// [`MODALS`]. A capitalised "May" is the month, unless it opens the sentence
/// and no number follows it ("May we…", but not "May 2025 …").
fn is_modal(text: &str, at: usize, w: &str) -> bool {
    if !MODALS.contains(&w.to_lowercase().as_str()) {
        return false;
    }
    if w != "May" {
        return true;
    }
    let first = !text[..at].contains(char::is_alphanumeric);
    let number_follows = text[at + w.len()..]
        .trim_start()
        .starts_with(|c: char| c.is_ascii_digit());
    first && !number_follows
}

/// True when the sentence `text` contains one of [`MODALS`] as a word.
pub fn has_modal(text: &str) -> bool {
    words_at(text).any(|(at, w)| is_modal(text, at, w))
}

/// Matches of `re` in `text` that are not within three words after a modal
/// nor hyphenated to another word.
pub fn asserted_matches<'t>(re: &Regex, text: &'t str) -> Vec<regex::Match<'t>> {
    let hyphen = |c: char| matches!(c, '-' | '\u{2010}' | '\u{2011}');
    re.find_iter(text)
        .filter(|m| {
            // A keyword hyphenated to another word is part of a compound
            // modifier ("outcome-trained agents"), not a verb.
            let joined_before = m.as_str().starts_with(hyphen)
                && text[..m.start()]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
            let joined_after = m.as_str().ends_with(hyphen)
                && text[m.end()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric);
            !joined_before && !joined_after
        })
        .filter(|m| {
            !words_at(&text[..m.start()])
                .rev()
                .take(3)
                .any(|(at, w)| is_modal(text, at, w))
        })
        .collect()
}

/// Compiles a case-insensitive whole-word alternation of literal phrases.
pub fn phrase_regex(phrases: &[String]) -> Result<Option<Regex>, regex::Error> {
    let alts: Vec<String> = phrases
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            regex::escape(&p.to_lowercase())
                .replace(r"\ ", r"\s+")
                .replace(' ', r"\s+")
        })
        .collect();
    if alts.is_empty() {
        return Ok(None);
    }
    Regex::new(&format!(
        r"(?i)(?:^|[^\p{{L}}\p{{N}}])(?:{})(?:$|[^\p{{L}}\p{{N}}])",
        alts.join("|")
    ))
    .map(Some)
}

/// Finds `research.toml` by walking up from `start`.
pub fn discover(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join("research.toml"))
        .find(|p| p.is_file())
}

impl Config {
    /// Loads configuration. `explicit` is `--config`; otherwise `research.toml`
    /// is searched upward from `cwd`.
    pub fn load(cwd: &Path, explicit: Option<&Path>) -> Result<Config, Error> {
        let config_path = match explicit {
            Some(p) => {
                let p = cwd.join(p);
                if !p.is_file() {
                    return Err(Error::new(format!(
                        "config file not found: {}",
                        p.display()
                    )));
                }
                Some(p)
            }
            None => discover(cwd),
        };
        let (root, parsed) = match &config_path {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
                let parsed: ResearchToml = toml::from_str(&text).map_err(|e| {
                    Error::new(format!("{}: {}", path.display(), e.to_string().trim_end()))
                })?;
                (
                    path.parent()
                        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf),
                    parsed,
                )
            }
            None => (cwd.to_path_buf(), ResearchToml::default()),
        };
        Config::from_parsed(root, config_path, parsed)
    }

    /// Configuration from TOML text, rooted at `root` (for tests and tools).
    pub fn from_toml(root: &Path, text: &str) -> Result<Config, Error> {
        let parsed: ResearchToml = toml::from_str(text).map_err(|e| Error::new(e.to_string()))?;
        Config::from_parsed(root.to_path_buf(), None, parsed)
    }

    fn from_parsed(
        root: PathBuf,
        config_path: Option<PathBuf>,
        parsed: ResearchToml,
    ) -> Result<Config, Error> {
        let cc = parsed.claimcheck;
        let where_ = config_path
            .as_ref()
            .map_or_else(|| "config".to_string(), |p| p.display().to_string());
        let registry = root.join(
            cc.registry
                .or(parsed.papers.registry)
                .unwrap_or_else(|| PathBuf::from("references.jsonl")),
        );
        let pattern_src: Vec<String> = cc.citations.patterns.unwrap_or_else(|| {
            DEFAULT_CITATION_PATTERNS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });
        let mut citations = Vec::new();
        for (i, p) in pattern_src.iter().enumerate() {
            let regex = Regex::new(p).map_err(|e| {
                Error::new(format!(
                    "{where_}: [claimcheck.citations] pattern {}: {e}",
                    i + 1
                ))
            })?;
            let names: Vec<&str> = regex.capture_names().flatten().collect();
            if !names.contains(&"key") && !names.contains(&"keys") {
                return Err(Error::new(format!(
                    "{where_}: [claimcheck.citations] pattern {} needs a named group `key` or `keys`",
                    i + 1
                )));
            }
            citations.push(CitationPattern { regex });
        }
        let mut kinds = Vec::new();
        for (kind, list) in &cc.claims.kinds {
            let re = phrase_regex(list).map_err(|e| {
                Error::new(format!("{where_}: [claimcheck.claims.kinds] {kind}: {e}"))
            })?;
            if let Some(re) = re {
                kinds.push((kind.clone(), re));
            }
        }
        let known = |kind: &str| {
            cc.claims.kinds.contains_key(kind) || cc.claims.number_kind.as_deref() == Some(kind)
        };
        let required_depth = match cc.claims.required_depth {
            Some(map) => {
                if let Some(kind) = map.keys().find(|k| !known(k)) {
                    return Err(Error::new(format!(
                        "{where_}: [claimcheck.claims.required_depth] unknown claim kind `{kind}`"
                    )));
                }
                map
            }
            None => ["result", "method"]
                .into_iter()
                .filter(|k| known(k))
                .map(|k| (k.to_string(), Depth::Method))
                .collect(),
        };
        let comparative = phrase_regex(&cc.uncited.comparative)
            .map_err(|e| Error::new(format!("{where_}: [claimcheck.uncited] comparative: {e}")))?;
        for label in cc.labels.min_depth.keys() {
            if !cc.labels.vocabulary.contains(label) {
                return Err(Error::new(format!(
                    "{where_}: [claimcheck.labels.min_depth] `{label}` is not in the vocabulary"
                )));
            }
        }
        let mut severity = HashMap::new();
        for (id, value) in &cc.severity {
            let rule = Rule::from_id(id).ok_or_else(|| {
                Error::new(format!(
                    "{where_}: [claimcheck.severity] unknown rule `{id}`"
                ))
            })?;
            let value = match value.as_str() {
                "off" => SeverityOverride::Off,
                "info" => SeverityOverride::Set(Severity::Info),
                "warn" => SeverityOverride::Set(Severity::Warn),
                "error" => SeverityOverride::Set(Severity::Error),
                other => {
                    return Err(Error::new(format!(
                        "{where_}: [claimcheck.severity] {id} = `{other}`: expected error, warn, info or off"
                    )));
                }
            };
            severity.insert(rule, value);
        }
        if !(0.0..=1.0).contains(&cc.quotes.error_below)
            || !(0.0..=1.0).contains(&cc.reference_list.title_similarity)
        {
            return Err(Error::new(format!(
                "{where_}: similarity thresholds must be between 0 and 1"
            )));
        }
        Ok(Config {
            paths: cc.paths.iter().map(|p| root.join(p)).collect(),
            exclude: cc.exclude.iter().map(|p| root.join(p)).collect(),
            root,
            config_path,
            registry,
            citations,
            quotes: cc.quotes,
            numbers: cc.numbers,
            claims: Claims {
                kinds,
                required_depth,
                number_kind: cc.claims.number_kind,
                window_chars: cc.claims.window_chars,
            },
            uncited: Uncited {
                comparative,
                links_are_evidence: cc.uncited.links_are_evidence,
            },
            labels: cc.labels,
            reference_list: cc.reference_list,
            severity,
        })
    }

    /// Path shown in findings: relative to the project root when inside it.
    pub fn display_path(&self, path: &Path) -> String {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return rel.to_string_lossy().into_owned();
        }
        if let (Ok(root), Ok(p)) = (self.root.canonicalize(), path.canonicalize())
            && let Ok(rel) = p.strip_prefix(&root)
        {
            return rel.to_string_lossy().into_owned();
        }
        path.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalised_may_is_a_month_unless_it_opens_a_question_like_sentence() {
        assert!(has_modal("Results may vary."));
        assert!(has_modal("May we assume 500 runs?"));
        assert!(!has_modal("May 2025 accuracy rose to 87.5%."));
        assert!(!has_modal("In May accuracy rose to 87.5%."));
        let re = phrase_regex(&["outperforms".into()]).unwrap().unwrap();
        assert_eq!(
            asserted_matches(&re, "In May it outperforms BM25.").len(),
            1
        );
        assert!(asserted_matches(&re, "It may outperform or outperforms BM25.").is_empty());
    }

    #[test]
    fn asserted_matches_skip_modals_and_compounds() {
        let re = phrase_regex(&["trained".into(), "fine-tuned".into()])
            .unwrap()
            .unwrap();
        assert_eq!(
            asserted_matches(&re, "The model was trained on news.").len(),
            1
        );
        assert!(asserted_matches(&re, "Outcome-trained agents exist.").is_empty());
        assert!(asserted_matches(&re, "It could be trained later.").is_empty());
        assert_eq!(asserted_matches(&re, "A fine-tuned model.").len(), 1);
    }

    #[test]
    fn defaults_without_file() {
        let c = Config::from_toml(Path::new("/p"), "").unwrap();
        assert_eq!(c.registry, Path::new("/p/references.jsonl"));
        assert_eq!(c.citations.len(), 3);
        assert_eq!(c.quotes.min_words, 3);
    }

    #[test]
    fn registry_precedence() {
        let c = Config::from_toml(
            Path::new("/p"),
            "[papers]\nregistry = \"a.jsonl\"\nother = 1\n",
        )
        .unwrap();
        assert_eq!(c.registry, Path::new("/p/a.jsonl"));
        let c = Config::from_toml(
            Path::new("/p"),
            "[papers]\nregistry = \"a.jsonl\"\n[claimcheck]\nregistry = \"b.jsonl\"\n",
        )
        .unwrap();
        assert_eq!(c.registry, Path::new("/p/b.jsonl"));
    }

    #[test]
    fn rejects_unknown_keys_rules_and_patterns() {
        assert!(Config::from_toml(Path::new("/p"), "[claimcheck]\nregistr = \"x\"\n").is_err());
        let e = Config::from_toml(Path::new("/p"), "[claimcheck.severity]\nnope = \"off\"\n")
            .unwrap_err();
        assert!(e.to_string().contains("unknown rule `nope`"));
        let e = Config::from_toml(
            Path::new("/p"),
            "[claimcheck.citations]\npatterns = ['\\[(\\d+)\\]']\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("named group"));
    }

    #[test]
    fn phrases_match_whole_words() {
        let re = phrase_regex(&words(&["state of the art", "uses"]))
            .unwrap()
            .unwrap();
        assert!(re.is_match("is State of  the art."));
        assert!(re.is_match("X uses Y"));
        assert!(!re.is_match("X reuses Y"));
    }
}
