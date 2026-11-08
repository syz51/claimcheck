//! The rules, applied to one parsed document at a time.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::config::{self, Config, LabelRequire, SeverityOverride, asserted_matches};
use crate::doc::{self, Block, BlockKind, Doc, MARK, Unit};
use crate::finding::{Evidence, Finding, Rule, Severity};
use crate::matcher::{self, Approx};
use crate::normalize::{self, NChar};
use crate::numbers::{self, NearMatch, NumTok};
use crate::registry::{Depth, Registry};
use crate::source::{IndexedText, TextEntry, hex};

/// Registry, configuration and the shared source-text cache for one run.
pub struct Project {
    pub cfg: Config,
    pub registry: Registry,
    pub texts: Vec<TextEntry>,
    /// Registry index → indexes into `texts`.
    ref_texts: Vec<Vec<usize>>,
    /// Resolved `source.path` → the survivor of the only entry that names
    /// the file; `None` when several entries name it or the chain is broken.
    notes: HashMap<std::path::PathBuf, Option<usize>>,
}

impl Project {
    pub fn new(cfg: Config, registry: Registry) -> Project {
        let mut texts: Vec<TextEntry> = Vec::new();
        let mut by_path: HashMap<std::path::PathBuf, usize> = HashMap::new();
        let mut ref_texts = Vec::with_capacity(registry.refs.len());
        for r in &registry.refs {
            let mut ids = Vec::new();
            for t in &r.texts {
                let abs = cfg.root.join(&t.path);
                let id = *by_path.entry(abs.clone()).or_insert_with(|| {
                    texts.push(TextEntry::new(abs, t.path.clone(), t.sha256.clone()));
                    texts.len() - 1
                });
                if !texts[id].keys.contains(&r.key) {
                    texts[id].keys.push(r.key.clone());
                }
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            ref_texts.push(ids);
        }
        let mut named: HashMap<std::path::PathBuf, Vec<usize>> = HashMap::new();
        for (i, r) in registry.refs.iter().enumerate() {
            if let Some(path) = &r.source_path {
                named
                    .entry(crate::resolve(&cfg.root.join(path)))
                    .or_default()
                    .push(i);
            }
        }
        let notes = named
            .into_iter()
            .map(|(path, entries)| {
                let survivor = match entries.as_slice() {
                    [one] => registry
                        .survivor(&registry.refs[*one].key)
                        .and_then(|s| registry.index(s)),
                    _ => None,
                };
                (path, survivor)
            })
            .collect();
        Project {
            cfg,
            registry,
            texts,
            ref_texts,
            notes,
        }
    }

    /// The reference that `file` (as shown in findings, relative to the root)
    /// cites implicitly: the survivor of the only registry entry whose
    /// `source.path` names it.
    pub fn note_reference(&self, file: &str) -> Option<usize> {
        let path = crate::resolve(&self.cfg.root.join(file));
        self.notes.get(&path).copied().flatten()
    }

    /// The reference whose texts stand for `r`: `r` itself, or, when `r`
    /// lists no texts and is a duplicate, the survivor of its chain.
    fn text_source(&self, r: usize) -> usize {
        let reference = &self.registry.refs[r];
        if !self.ref_texts[r].is_empty() || reference.duplicate_of.is_none() {
            return r;
        }
        self.registry
            .survivor(&reference.key)
            .and_then(|s| self.registry.index(s))
            .unwrap_or(r)
    }

    /// Readable texts of a reference, read from its survivor when it is a
    /// duplicate without texts of its own.
    fn texts_of(&self, r: usize) -> Vec<(&TextEntry, Arc<IndexedText>)> {
        self.ref_texts[self.text_source(r)]
            .iter()
            .filter_map(|&i| {
                let e = &self.texts[i];
                e.get().text.as_ref().ok().map(|t| (e, Arc::clone(t)))
            })
            .collect()
    }

    /// Findings about the registry's text files, for files this run needed.
    pub fn text_findings(&self, order: usize) -> Vec<Finding> {
        let mut out = Vec::new();
        for e in &self.texts {
            let Some(loaded) = e.loaded() else { continue };
            let key = e.keys.first().cloned().unwrap_or_default();
            let mut data = Map::new();
            data.insert("references".into(), json!(e.keys));
            let evidence = vec![Evidence {
                path: e.display.clone(),
                locator: "file".into(),
            }];
            if let Err(err) = &loaded.text {
                let msg = format!(
                    "text file {} listed for reference {key} cannot be read: {err}",
                    e.display
                );
                push(
                    &self.cfg,
                    &mut out,
                    Rule::SourceTextMissing,
                    Severity::Warn,
                    key.clone(),
                    msg,
                    evidence.clone(),
                    data.clone(),
                    (order, 0),
                );
            }
            if let Some((want, got)) = &loaded.hash_mismatch {
                data.insert("expected".into(), json!(want));
                data.insert("actual".into(), json!(got));
                let msg = format!(
                    "text file {} for reference {key} does not match the SHA-256 recorded in the registry",
                    e.display
                );
                push(
                    &self.cfg,
                    &mut out,
                    Rule::SourceTextChanged,
                    Severity::Warn,
                    key,
                    msg,
                    evidence,
                    data,
                    (order, 0),
                );
            }
        }
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn push(
    cfg: &Config,
    out: &mut Vec<Finding>,
    rule: Rule,
    severity: Severity,
    subject: String,
    message: String,
    evidence: Vec<Evidence>,
    data: Map<String, Value>,
    order: (usize, usize),
) {
    let severity = match cfg.severity.get(&rule) {
        Some(SeverityOverride::Off) => return,
        Some(SeverityOverride::Set(s)) => *s,
        _ => severity,
    };
    out.push(Finding {
        rule,
        severity,
        subject,
        message,
        evidence,
        data,
        order,
    });
}

/// Counts of what was checked, for `--summary`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: usize,
    pub citations: usize,
    pub citations_resolved: usize,
    pub quotes_checked: usize,
    pub quotes_supported: usize,
    pub numbers_checked: usize,
    pub numbers_found: usize,
    pub claims_classified: usize,
    /// Sentences (or non-prose blocks) with text that cite a reference, or
    /// that are part of a blockquote checked as a cited quotation.
    pub citing: Measure,
    /// Sentences with text and without a citation in paragraphs and list
    /// items outside blockquotes: those `uncited-claim` checks. Uncited table
    /// rows, and sentences without a citation of a blockquote not checked as
    /// a cited quotation, are in neither unit.
    pub uncited: Measure,
}

/// One `rf.measure/1` unit: how many sentences, and the distinct
/// [`normalize::sentence_key`] of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measure {
    pub count: usize,
    pub keys: BTreeSet<[u8; 8]>,
}

impl Measure {
    fn add(&mut self, mut o: Measure) {
        self.count += o.count;
        self.keys.append(&mut o.keys);
    }

    /// Keys sort as bytes, which is their order as hex strings.
    fn record(&self, unit: &str) -> Value {
        let keys: Vec<String> = self.keys.iter().map(|k| hex(k)).collect();
        json!({
            "schema": "rf.measure/1",
            "tool": "claimcheck",
            "unit": unit,
            "count": self.count,
            "keys": keys,
        })
    }
}

impl Stats {
    pub fn add(&mut self, o: Stats) {
        self.files += o.files;
        self.citations += o.citations;
        self.citations_resolved += o.citations_resolved;
        self.quotes_checked += o.quotes_checked;
        self.quotes_supported += o.quotes_supported;
        self.numbers_checked += o.numbers_checked;
        self.numbers_found += o.numbers_found;
        self.claims_classified += o.claims_classified;
        self.citing.add(o.citing);
        self.uncited.add(o.uncited);
    }

    /// The `rf.measure/1` records for `--measure`, one per unit, in output
    /// order. A checked sentence is in at most one of them.
    pub fn measures(&self) -> [Value; 2] {
        [
            self.citing.record("citing-sentence"),
            self.uncited.record("uncited-sentence"),
        ]
    }
}

struct Ctx<'a> {
    p: &'a Project,
    doc: &'a Doc,
    file: &'a str,
    order: usize,
    out: Vec<Finding>,
    stats: Stats,
    /// Citation index → resolved registry indexes.
    resolved: Vec<Vec<usize>>,
    /// The reference every uncited prose sentence cites implicitly: see
    /// [`Project::note_reference`].
    implied: Option<usize>,
}

fn quote_excerpt(s: &str, max: usize) -> String {
    normalize::excerpt(s, max)
}

/// A unit's text for its `rf.measure/1` key: without citation markers or the
/// whitespace before each, so "helps [1]." and "helps.[1]" both read
/// "helps.", and moving a citation within a sentence keeps its key.
fn key_text(block: &Block, u: &Unit) -> String {
    let mut t = String::new();
    for p in &block.chars[u.start..u.end] {
        if p.c == MARK {
            t.truncate(t.trim_end().len());
        } else {
            t.push(p.c);
        }
    }
    t
}

/// Joins the units that overlap one checked inline quotation. A stop inside
/// a quotation does not end the sentence that quotes it, so in
/// `It says "A. B." [1]` the citation covers the whole sentence, as it does
/// for a quotation of one sentence.
fn join_quoted(units: Vec<Unit>, quoted: &[(usize, usize)]) -> Vec<Unit> {
    let overlaps = |u: &Unit, &(s, e): &(usize, usize)| u.start < e && s < u.end;
    let mut out: Vec<Unit> = Vec::with_capacity(units.len());
    for u in units {
        match out.last_mut() {
            Some(last) if quoted.iter().any(|q| overlaps(last, q) && overlaps(&u, q)) => {
                last.end = u.end;
                for c in u.cites {
                    if !last.cites.contains(&c) {
                        last.cites.push(c);
                    }
                }
            }
            _ => out.push(u),
        }
    }
    out
}

/// Counts `u` in `m` when it has text.
fn measure(m: &mut Measure, block: &Block, u: &Unit) {
    if let Some(key) = normalize::sentence_key(&key_text(block, u)) {
        m.count += 1;
        m.keys.insert(key);
    }
}

fn refs_list(keys: &[&str]) -> String {
    match keys {
        [one] => format!("reference {one}"),
        many => format!("references {}", many.join(", ")),
    }
}

/// "reference 3 has" or "references 3, 4 have", for messages.
fn refs_have(keys: &[&str]) -> String {
    match keys {
        [one] => format!("reference {one} has"),
        many => format!("references {} have", many.join(", ")),
    }
}

impl<'a> Ctx<'a> {
    fn emit(
        &mut self,
        rule: Rule,
        sev: Severity,
        line: usize,
        message: String,
        evidence: Vec<Evidence>,
        data: Map<String, Value>,
    ) {
        let subject = format!("{}:{line}", self.file);
        push(
            &self.p.cfg,
            &mut self.out,
            rule,
            sev,
            subject,
            message,
            evidence,
            data,
            (self.order, line),
        );
    }

    fn key(&self, r: usize) -> &'a str {
        &self.p.registry.refs[r].key
    }

    /// Duplicate key → survivor key, for the references in `refs` whose texts
    /// were read from their survivor.
    fn survivors(&self, refs: &[usize]) -> Map<String, Value> {
        refs.iter()
            .filter_map(|&r| {
                let s = self.p.text_source(r);
                (s != r).then(|| (self.key(r).to_string(), json!(self.key(s))))
            })
            .collect()
    }

    fn refs_of(&self, cites: &[u32]) -> Vec<usize> {
        let mut out = Vec::new();
        for &c in cites {
            for &r in &self.resolved[c as usize] {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
        out
    }

    fn citations(&mut self) {
        let used: BTreeSet<u32> = self
            .doc
            .blocks
            .iter()
            .flat_map(|b| b.chars.iter().filter_map(|p| p.cite))
            .collect();
        self.resolved = vec![Vec::new(); self.doc.citations.len()];
        for ci in used {
            let c = &self.doc.citations[ci as usize];
            let line = self.doc.line_of(c.start);
            self.stats.citations += 1;
            if let (Some(label), [key]) = (&c.label, c.keys.as_slice())
                && label != key
            {
                let mut data = Map::new();
                data.insert("label".into(), json!(label));
                data.insert("reference".into(), json!(key));
                self.emit(
                    Rule::CitationLabelMismatch,
                    Severity::Error,
                    line,
                    format!("citation shows [{label}] but links to reference {key}"),
                    vec![],
                    data,
                );
            }
            let mut all_ok = true;
            for key in &c.keys {
                let Some(r) = self.p.registry.index(key) else {
                    all_ok = false;
                    let mut data = Map::new();
                    data.insert("reference".into(), json!(key));
                    data.insert("citation".into(), json!(c.raw));
                    self.emit(
                        Rule::CitationUnresolved,
                        Severity::Error,
                        line,
                        format!("citation key {key} is not in the registry"),
                        vec![],
                        data,
                    );
                    continue;
                };
                self.resolved[ci as usize].push(r);
                if let Some(dup) = &self.p.registry.refs[r].duplicate_of {
                    let survivor = self.p.registry.survivor(key);
                    let mut data = Map::new();
                    data.insert("reference".into(), json!(key));
                    data.insert("duplicate_of".into(), json!(dup));
                    data.insert("survivor".into(), json!(survivor));
                    let msg = match survivor {
                        Some(s) => {
                            format!("reference {key} is a duplicate entry; cite {s} instead")
                        }
                        None => format!(
                            "reference {key} is a duplicate of {dup}, whose chain does not end at a surviving entry"
                        ),
                    };
                    self.emit(
                        Rule::CitationDuplicate,
                        Severity::Warn,
                        line,
                        msg,
                        vec![],
                        data,
                    );
                }
            }
            if all_ok {
                self.stats.citations_resolved += 1;
            }
        }
    }

    fn run(&mut self) {
        self.citations();
        let doc = self.doc;
        let mut handled_groups: BTreeSet<u32> = BTreeSet::new();
        self.blockquotes(&mut handled_groups);
        for block in &doc.blocks {
            if block.in_reference_list {
                if block.is_prose() {
                    self.reference_entry(block);
                }
                continue;
            }
            if matches!(block.kind, BlockKind::Heading(_)) {
                continue;
            }
            let mut units = doc::units(block);
            let spans = doc::quotes(block);
            let mut quoted: Vec<(usize, usize)> = Vec::new();
            if block
                .quote_group
                .is_some_and(|g| handled_groups.contains(&g))
            {
                // The whole blockquote was checked as a quotation.
                quoted.push((0, block.chars.len()));
            } else {
                for &(s, e) in &spans {
                    let mut cites = block.cites_in(s, e);
                    for u in units.iter().filter(|u| u.start < e && s < u.end) {
                        for c in &u.cites {
                            if !cites.contains(c) {
                                cites.push(*c);
                            }
                        }
                    }
                    let refs = self.refs_of(&cites);
                    let chars: Vec<(char, u32)> = block.chars[s..e]
                        .iter()
                        .filter(|p| p.c != MARK)
                        .map(|p| (p.c, p.off))
                        .collect();
                    if refs.is_empty() || !self.long_enough(&chars) {
                        continue;
                    }
                    quoted.push((s, e));
                    let line = doc.line_of(block.chars[s].off as usize);
                    self.check_quote(line, &chars, &refs);
                }
                units = join_quoted(units, &quoted);
            }
            let mut block_claims = false;
            let cited_quote = block
                .quote_group
                .is_some_and(|g| handled_groups.contains(&g));
            for u in &units {
                let nums = self.numbers_in(block, u, &quoted);
                let refs = self.refs_of(&u.cites);
                let line = doc.line_of(unit_offset(block, u));
                if cited_quote || !u.cites.is_empty() {
                    measure(&mut self.stats.citing, block, u);
                }
                if !refs.is_empty() {
                    if !nums.is_empty() {
                        let linked = block.has_link(u.start, u.end);
                        let sentence = block.text(u.start, u.end).0;
                        self.check_numbers(line, &nums, &refs, &sentence, linked);
                    }
                    if block.is_prose() {
                        self.check_depth(block, u, line, &nums, &refs);
                    }
                } else if !u.cites.is_empty() {
                    // Every key is unresolved: citation-unresolved reports it,
                    // and the sentence still cites, so it is not uncited.
                } else if block.is_prose() && block.quote_group.is_none() {
                    // A number inside quotation marks belongs to quoted or
                    // illustrative text, not to a claim of the sentence.
                    let own: Vec<DocNumber> = nums
                        .iter()
                        .filter(|n| !spans.iter().any(|&(s, e)| s <= n.at && n.at < e))
                        .cloned()
                        .collect();
                    if let Some(r) = self.implied {
                        // The note's own entry cites the sentence. Its numbers
                        // are checked only against a readable text, and its
                        // quotations and claim depth not at all: see README,
                        // "Paper notes".
                        measure(&mut self.stats.citing, block, u);
                        if !nums.is_empty() && !self.p.texts_of(r).is_empty() {
                            let linked = block.has_link(u.start, u.end);
                            let sentence = block.text(u.start, u.end).0;
                            self.check_numbers(line, &nums, &[r], &sentence, linked);
                        }
                        block_claims |= self.check_uncited(block, u, line, &own, false);
                    } else {
                        // Uncited text counts only here, where uncited-claim can fire.
                        measure(&mut self.stats.uncited, block, u);
                        block_claims |= self.check_uncited(block, u, line, &own, true);
                    }
                }
                block_claims |= !nums.is_empty();
            }
            if block.is_prose() && !self.p.cfg.labels.vocabulary.is_empty() {
                self.check_label(block, block_claims);
            }
        }
    }

    fn long_enough(&self, chars: &[(char, u32)]) -> bool {
        let cfg = &self.p.cfg.quotes;
        let cjk = chars.iter().filter(|(c, _)| normalize::is_cjk(*c)).count();
        let text: String = chars
            .iter()
            .map(|(c, _)| if normalize::is_cjk(*c) { ' ' } else { *c })
            .collect();
        let words = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .count();
        words >= cfg.min_words || (cjk > 0 && cjk >= cfg.min_cjk_chars)
    }

    // ---- quotations -------------------------------------------------------

    fn blockquotes(&mut self, handled: &mut BTreeSet<u32>) {
        let doc = self.doc;
        let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
        for (i, b) in doc.blocks.iter().enumerate() {
            if let Some(g) = b.quote_group {
                match groups.last_mut() {
                    Some((last, members)) if *last == g => members.push(i),
                    _ => groups.push((g, vec![i])),
                }
            }
        }
        for (g, members) in groups {
            let first = members[0];
            if doc.blocks[first].in_reference_list {
                continue;
            }
            // A trailing attribution line ("— Author [3]") is not part of the quotation.
            let is_attribution = |b: &Block| {
                let (t, _) = b.text(0, b.chars.len());
                let t = t.trim_start();
                t.starts_with(['—', '–', '―']) || t.starts_with("--") || t.starts_with("- ")
            };
            let mut cites: Vec<u32> = Vec::new();
            for &i in &members {
                let b = &doc.blocks[i];
                for c in b.cites_in(0, b.chars.len()) {
                    if !cites.contains(&c) {
                        cites.push(c);
                    }
                }
            }
            if cites.is_empty() && first > 0 {
                let prev = &doc.blocks[first - 1];
                let (t, _) = prev.text(0, prev.chars.len());
                if prev.quote_group.is_none() && t.trim_end().ends_with([':', '：']) {
                    let units = doc::units(prev);
                    if let Some(last) = units.last() {
                        cites = last.cites.clone();
                    }
                }
            }
            let refs = self.refs_of(&cites);
            if refs.is_empty() {
                continue;
            }
            let mut chars: Vec<(char, u32)> = Vec::new();
            for &i in &members {
                let b = &doc.blocks[i];
                if members.len() > 1
                    && i == *members.last().expect("non-empty")
                    && is_attribution(b)
                {
                    continue;
                }
                chars.extend(b.chars.iter().filter(|p| p.c != MARK).map(|p| (p.c, p.off)));
                chars.push(('\n', b.chars.last().map_or(0, |p| p.off)));
            }
            if !self.long_enough(&chars) {
                continue;
            }
            handled.insert(g);
            let line = doc.line_of(doc.blocks[first].first_offset());
            self.check_quote(line, &chars, &refs);
        }
    }

    /// Splits a quotation at ellipses and bracketed insertions into skeleton segments.
    fn segments(chars: &[(char, u32)]) -> Vec<Vec<char>> {
        let mut parts: Vec<Vec<(char, u32)>> = vec![Vec::new()];
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i].0;
            let three_dots = c == '.'
                && chars.get(i + 1).is_some_and(|p| p.0 == '.')
                && chars.get(i + 2).is_some_and(|p| p.0 == '.');
            if c == '…' || three_dots {
                parts.push(Vec::new());
                i += if three_dots { 3 } else { 1 };
                while i < chars.len() && chars[i].0 == '.' {
                    i += 1;
                }
                continue;
            }
            if c == '['
                && let Some(close) = chars[i..].iter().position(|p| p.0 == ']')
            {
                parts.push(Vec::new());
                i += close + 1;
                continue;
            }
            parts.last_mut().expect("non-empty").push(chars[i]);
            i += 1;
        }
        parts
            .into_iter()
            .map(|p| normalize::skeleton(&normalize::normalize(&p)).0)
            .filter(|s| s.len() >= 4)
            .collect()
    }

    fn check_quote(&mut self, line: usize, chars: &[(char, u32)], refs: &[usize]) {
        let segments = Self::segments(chars);
        if segments.is_empty() {
            return;
        }
        self.stats.quotes_checked += 1;
        let quote: String = chars.iter().map(|(c, _)| *c).collect();
        let shown = quote_excerpt(&quote, self.p.cfg.quotes.excerpt_chars);
        let missing: Vec<&str> = refs
            .iter()
            .filter(|&&r| self.p.texts_of(r).is_empty())
            .map(|&r| self.key(r))
            .collect();
        let mut best: Option<BestMatch> = None;
        for &r in refs {
            for (entry, text) in self.p.texts_of(r) {
                let seg_strs: Vec<String> = segments.iter().map(|s| s.iter().collect()).collect();
                // Exact, in order, and not cutting a word or number of the source.
                let mut pos = 0;
                let mut in_order = true;
                let mut all = true;
                for s in &seg_strs {
                    match text.find_bounded(s, pos) {
                        Some(p) => pos = p + s.chars().count(),
                        None if text.find_bounded(s, 0).is_some() => in_order = false,
                        None => all = false,
                    }
                }
                if all && in_order {
                    self.stats.quotes_supported += 1;
                    return;
                }
                // Approximate: the worst segment decides. Anything short of a
                // bounded exact match scores below 1.
                let mut worst: Option<Approx> = None;
                let mut cuts = false;
                for (seg, s) in segments.iter().zip(&seg_strs) {
                    let exact = |p: usize, similarity: f64| Approx {
                        similarity,
                        start: p,
                        end: p + seg.len(),
                    };
                    let hit = if let Some(p) = text.find_bounded(s, 0) {
                        exact(p, 1.0)
                    } else if let Some(p) = text.find_exact(s, 0) {
                        cuts = true;
                        exact(p, 0.99)
                    } else {
                        let mut hit = worst_chunk(&text, seg).unwrap_or(Approx {
                            similarity: 0.0,
                            start: 0,
                            end: 0,
                        });
                        hit.similarity = hit.similarity.min(0.99);
                        hit
                    };
                    if worst.is_none_or(|w| hit.similarity < w.similarity) {
                        worst = Some(hit);
                    }
                }
                let mut w = worst.expect("segments are non-empty");
                if all && !in_order {
                    w.similarity = w.similarity.min(0.99);
                }
                if best
                    .as_ref()
                    .is_none_or(|b| w.similarity > b.hit.similarity)
                {
                    best = Some(BestMatch {
                        entry,
                        text,
                        hit: w,
                        key: self.key(r),
                        cuts,
                    });
                }
            }
        }
        let mut data = Map::new();
        data.insert("quote".into(), json!(shown));
        data.insert(
            "references".into(),
            json!(refs.iter().map(|&r| self.key(r)).collect::<Vec<_>>()),
        );
        let survivors = self.survivors(refs);
        if !survivors.is_empty() {
            data.insert("survivors".into(), Value::Object(survivors));
        }
        let mut evidence = Vec::new();
        if let Some(BestMatch {
            entry,
            text,
            hit,
            key,
            cuts,
        }) = &best
        {
            let sim = &hit.similarity;
            if *cuts {
                data.insert("cuts_token".into(), json!(true));
            }
            let (s, e) = text.skel_span(hit.start, hit.end);
            let src_line = text.line_of(s);
            let excerpt = text.excerpt(s, e, self.p.cfg.quotes.excerpt_chars);
            data.insert("similarity".into(), json!(round3(*sim)));
            data.insert("best_match".into(), json!({"reference": key, "path": entry.display, "line": src_line, "excerpt": excerpt}));
            evidence.push(Evidence {
                path: entry.display.clone(),
                locator: format!("best match line {src_line}, similarity {:.2}", sim),
            });
        }
        if !missing.is_empty() {
            data.insert("missing_text".into(), json!(missing));
            let msg = format!(
                "quotation cannot be verified: {} no source text",
                refs_have(&missing)
            );
            self.emit(
                Rule::SourceTextMissing,
                Severity::Warn,
                line,
                msg,
                evidence,
                data,
            );
            return;
        }
        let keys: Vec<&str> = refs.iter().map(|&r| self.key(r)).collect();
        let (sev, msg) = match &best {
            Some(b) if b.hit.similarity >= self.p.cfg.quotes.error_below => (
                Severity::Warn,
                format!(
                    "quoted text is not verbatim in the source text of {}; closest match similarity {:.2}{}",
                    refs_list(&keys),
                    b.hit.similarity,
                    if b.cuts {
                        " (the match starts or ends inside a word or number of the source)"
                    } else {
                        ""
                    }
                ),
            ),
            Some(b) => (
                Severity::Error,
                format!(
                    "quoted text not found in the source text of {}; closest match similarity {:.2}",
                    refs_list(&keys),
                    b.hit.similarity
                ),
            ),
            None => (
                Severity::Error,
                format!(
                    "quoted text not found in the source text of {}",
                    refs_list(&keys)
                ),
            ),
        };
        self.emit(Rule::QuoteNotFound, sev, line, msg, evidence, data);
    }

    // ---- numbers ----------------------------------------------------------

    fn numbers_in(&self, block: &Block, u: &Unit, quoted: &[(usize, usize)]) -> Vec<DocNumber> {
        let (text, map) = block.text(u.start, u.end);
        let norm = normalize::normalize_str(&text);
        let chars: Vec<char> = norm.iter().map(|n| n.c).collect();
        let nstr: String = chars.iter().collect();
        let mut char_of_byte = Vec::with_capacity(nstr.len() + 1);
        for (i, c) in chars.iter().enumerate() {
            for _ in 0..c.len_utf8() {
                char_of_byte.push(i);
            }
        }
        char_of_byte.push(chars.len());
        let masks: Vec<(MaskKind, usize, usize)> = MASKS
            .iter()
            .flat_map(|(kind, re)| {
                re.find_iter(&nstr)
                    .map(|m| (*kind, char_of_byte[m.start()], char_of_byte[m.end()]))
                    .collect::<Vec<_>>()
            })
            .collect();
        let cfg = &self.p.cfg.numbers;
        let block_index = |n: &NChar| map[n.off as usize];
        let mut out = Vec::new();
        for tok in numbers::scan(&chars) {
            if tok.glued_left
                || tok.glued_right
                || tok.dotted
                || masks
                    .iter()
                    .any(|&(k, s, e)| s <= tok.start && tok.end <= e && k.hides(&tok))
            {
                continue;
            }
            let bi = block_index(&norm[tok.start]);
            if block.chars[bi].code
                || block.chars[bi].link
                || quoted.iter().any(|&(s, e)| s <= bi && bi < e)
            {
                continue;
            }
            let strong = tok.percent || tok.decimal || tok.separated || tok.scaled.is_some();
            if !strong {
                let Some(v) = tok.as_plain_integer() else {
                    continue;
                };
                let digits = tok.end - tok.start;
                if cfg.exclude_years && digits == 4 && (cfg.year_min..=cfg.year_max).contains(&v) {
                    continue;
                }
                if v < cfg.min_integer {
                    continue;
                }
            }
            let shown: String = if tok.negative { "-" } else { "" }.to_string()
                + &chars[tok.start..tok.end].iter().collect::<String>()
                + if tok.percent { "%" } else { "" };
            let context = context_words(&chars, tok.start, tok.end);
            out.push(DocNumber {
                at: bi,
                tok,
                shown,
                context,
            });
        }
        out
    }

    /// `linked`: the sentence holds a link that is not a citation.
    fn check_numbers(
        &mut self,
        line: usize,
        nums: &[DocNumber],
        refs: &[usize],
        sentence: &str,
        linked: bool,
    ) {
        let sentence = quote_excerpt(sentence, 200);
        let linked = linked && self.p.cfg.numbers.links_are_evidence;
        let texts: Vec<(usize, &TextEntry, Arc<IndexedText>)> = refs
            .iter()
            .flat_map(|&r| self.p.texts_of(r).into_iter().map(move |(e, t)| (r, e, t)))
            .collect();
        let missing: Vec<&str> = refs
            .iter()
            .filter(|&&r| self.p.texts_of(r).is_empty())
            .map(|&r| self.key(r))
            .collect();
        let keys: Vec<&str> = refs.iter().map(|&r| self.key(r)).collect();
        let survivors = self.survivors(refs);
        let mut unverifiable: Vec<&str> = Vec::new();
        for n in nums {
            self.stats.numbers_checked += 1;
            if texts
                .iter()
                .any(|(_, _, t)| n.tok.lookup_keys().iter().any(|v| t.has_number(v)))
            {
                self.stats.numbers_found += 1;
                continue;
            }
            let cfg = &self.p.cfg.numbers;
            let near = texts.iter().find_map(|(r, e, t)| {
                t.near_number(&n.tok, &n.context, cfg.context_window, cfg.match_rounded)
                    .map(|m| (*r, *e, m))
            });
            if let Some((r, e, (how, value, src_line))) = near {
                self.stats.numbers_found += 1;
                let key = self.key(r);
                let how_text = match how {
                    NearMatch::Scaled(shift) => {
                        format!("is rounded from {value} in {}s", numbers::scale_word(shift))
                    }
                    NearMatch::Rounded => format!("is rounded from {value}"),
                    NearMatch::Percent => format!("is {value} as a percentage"),
                    NearMatch::RoundedPercent => {
                        format!("is rounded from {value} as a percentage")
                    }
                };
                let msg = format!(
                    "{} {how_text} in the source text of {}",
                    n.shown,
                    refs_list(&[key])
                );
                let mut data = Map::new();
                data.insert("number".into(), json!(n.shown));
                data.insert("sentence".into(), json!(sentence));
                data.insert("value".into(), json!(n.tok.signed()));
                data.insert("references".into(), json!(keys));
                data.insert(
                    "match".into(),
                    json!({"kind": how.as_str(), "reference": key, "path": e.display, "line": src_line, "value": value}),
                );
                if !survivors.is_empty() {
                    data.insert("survivors".into(), Value::Object(survivors.clone()));
                }
                let evidence = vec![Evidence {
                    path: e.display.clone(),
                    locator: format!("line {src_line}: {value}"),
                }];
                self.emit(
                    Rule::NumberNotInSource,
                    Severity::Info,
                    line,
                    msg,
                    evidence,
                    data,
                );
                continue;
            }
            if !missing.is_empty() {
                unverifiable.push(&n.shown);
                continue;
            }
            let target = n.tok.as_f64().unwrap_or(0.0);
            let mut evidence = Vec::new();
            let mut nearest = Vec::new();
            for (r, e, t) in &texts {
                for (value, src_line) in t.nearby_numbers(
                    &n.context,
                    target,
                    self.p.cfg.numbers.context_window,
                    self.p.cfg.numbers.nearest,
                ) {
                    evidence.push(Evidence {
                        path: e.display.clone(),
                        locator: format!("line {src_line}: {value}"),
                    });
                    nearest.push(json!({"reference": self.key(*r), "path": e.display, "line": src_line, "value": value}));
                }
            }
            let mut data = Map::new();
            data.insert("number".into(), json!(n.shown));
            data.insert("sentence".into(), json!(sentence));
            data.insert("value".into(), json!(n.tok.signed()));
            data.insert("references".into(), json!(keys));
            data.insert("context".into(), json!(n.context));
            data.insert("nearest".into(), Value::Array(nearest));
            if !survivors.is_empty() {
                data.insert("survivors".into(), Value::Object(survivors.clone()));
            }
            if linked {
                data.insert("linked".into(), json!(true));
            }
            let near = if evidence.is_empty() {
                String::new()
            } else {
                let vals: Vec<String> = evidence
                    .iter()
                    .take(3)
                    .map(|e| e.locator.rsplit(": ").next().unwrap_or("").to_string())
                    .collect();
                format!("; nearby numbers: {}", vals.join(", "))
            };
            let msg = format!(
                "{} does not occur in the source text of {}{near}",
                n.shown,
                refs_list(&keys)
            );
            self.emit(
                Rule::NumberNotInSource,
                if linked {
                    Severity::Info
                } else {
                    Severity::Warn
                },
                line,
                msg,
                evidence,
                data,
            );
        }
        if !unverifiable.is_empty() {
            let mut data = Map::new();
            data.insert("numbers".into(), json!(unverifiable));
            data.insert("sentence".into(), json!(sentence));
            data.insert("missing_text".into(), json!(missing));
            data.insert("references".into(), json!(keys));
            if !survivors.is_empty() {
                data.insert("survivors".into(), Value::Object(survivors));
            }
            let msg = format!(
                "numbers {} cannot be verified: {} no source text",
                unverifiable.join(", "),
                refs_have(&missing)
            );
            self.emit(
                Rule::SourceTextMissing,
                Severity::Warn,
                line,
                msg,
                vec![],
                data,
            );
        }
    }

    // ---- claim depth --------------------------------------------------------

    /// Claim kinds of a unit with the block positions of their keyword matches
    /// and, for the number kind, of checked numbers.
    fn kinds_of(&self, block: &Block, u: &Unit, nums: &[DocNumber]) -> Vec<ClaimKind> {
        let (text, map) = block.claim_text(u.start, u.end);
        let claims = &self.p.cfg.claims;
        let mut kinds: Vec<ClaimKind> = Vec::new();
        for (name, re) in &claims.kinds {
            let keywords: Vec<usize> = asserted_matches(re, &text)
                .iter()
                .map(|m| map[m.start()])
                .collect();
            if !keywords.is_empty() {
                kinds.push(ClaimKind {
                    name: name.clone(),
                    keywords,
                    numbers: Vec::new(),
                });
            }
        }
        if let Some(k) = &claims.number_kind
            && !nums.is_empty()
        {
            let numbers: Vec<usize> = nums.iter().map(|n| n.at).collect();
            match kinds.iter_mut().find(|c| &c.name == k) {
                Some(c) => c.numbers = numbers,
                None => kinds.push(ClaimKind {
                    name: k.clone(),
                    keywords: Vec::new(),
                    numbers,
                }),
            }
        }
        kinds
    }

    /// Reports references whose recorded depth is below what the unit's claim
    /// kinds require. Only a keyword next to the citation (same clause, within
    /// `window_chars`) makes an error; numbers alone, distant keywords and mixed
    /// kinds give info.
    fn check_depth(
        &mut self,
        block: &Block,
        u: &Unit,
        line: usize,
        nums: &[DocNumber],
        refs: &[usize],
    ) {
        let kinds = self.kinds_of(block, u, nums);
        if kinds.is_empty() {
            return;
        }
        self.stats.claims_classified += 1;
        let required = &self.p.cfg.claims.required_depth;
        if !kinds.iter().any(|k| required.contains_key(&k.name)) {
            return;
        }
        let sentence = quote_excerpt(&block.text(u.start, u.end).0, 200);
        let window = self.p.cfg.claims.window_chars;
        let all: Vec<&str> = kinds.iter().map(|k| k.name.as_str()).collect();
        for &r in refs {
            let key = self.key(r);
            let marks: Vec<usize> = (u.start..u.end)
                .filter(|&i| {
                    block.chars[i]
                        .cite
                        .is_some_and(|c| self.resolved[c as usize].contains(&r))
                })
                .collect();
            let near = |pos: &[usize]| {
                pos.iter().any(|&p| {
                    marks.iter().any(|&m| {
                        let (a, b) = (p.min(m), p.max(m));
                        b - a <= window && !block.chars[a..b].iter().any(|c| c.c == ';' && !c.code)
                    })
                })
            };
            // Only kinds with a required depth can make or dilute an error.
            let close: Vec<&str> = kinds
                .iter()
                .filter(|k| required.contains_key(&k.name) && near(&k.keywords))
                .map(|k| k.name.as_str())
                .collect();
            let depth = self.p.registry.refs[r].depth;
            let mut data = Map::new();
            data.insert("reference".into(), json!(key));
            data.insert("kinds".into(), json!(all));
            data.insert("near_citation".into(), json!(close));
            data.insert("sentence".into(), json!(sentence));
            data.insert("depth".into(), json!(depth));
            let Some(depth) = depth else {
                let msg = format!(
                    "claim kind {} cannot be checked: reference {key} has no recorded depth",
                    all.join("/")
                );
                self.emit(
                    Rule::DepthInsufficient,
                    Severity::Info,
                    line,
                    msg,
                    vec![],
                    data,
                );
                continue;
            };
            let short_of = |list: &[&str]| -> Vec<(String, Depth)> {
                list.iter()
                    .filter_map(|k| {
                        required
                            .get(*k)
                            .filter(|d| **d > depth)
                            .map(|d| ((*k).to_string(), *d))
                    })
                    .collect()
            };
            let (short, sev, msg) = if close.is_empty() {
                let msg = format!(
                    "possible {} claim (no claim keyword next to the citation); reference {key} has depth {depth}",
                    all.join("/")
                );
                (short_of(&all), Severity::Info, msg)
            } else {
                let short = short_of(&close);
                let (sev, what) = if short.len() == close.len() {
                    (
                        Severity::Error,
                        format!("sentence makes a {} claim", close.join("/")),
                    )
                } else {
                    (
                        Severity::Info,
                        format!("ambiguous claim kind ({})", close.join("/")),
                    )
                };
                (
                    short,
                    sev,
                    format!("{what} but reference {key} was inspected only to depth {depth}"),
                )
            };
            if short.is_empty() {
                continue;
            }
            let needs: Vec<String> = short
                .iter()
                .map(|(k, d)| format!("{k} needs {d}"))
                .collect();
            data.insert(
                "required".into(),
                json!(
                    short
                        .iter()
                        .map(|(k, d)| (k.clone(), d.as_str()))
                        .collect::<HashMap<_, _>>()
                ),
            );
            self.emit(
                Rule::DepthInsufficient,
                sev,
                line,
                format!("{msg} ({})", needs.join(", ")),
                vec![],
                data,
            );
        }
    }

    // ---- uncited claims and labels -------------------------------------------

    fn has_valid_label(&self, block: &Block) -> bool {
        doc::leading_label(block).is_some_and(|(l, _)| self.p.cfg.labels.vocabulary.contains(&l))
    }

    /// Reports an uncited claim when `report`; returns whether the unit makes
    /// a claim.
    fn check_uncited(
        &mut self,
        block: &Block,
        u: &Unit,
        line: usize,
        nums: &[DocNumber],
        report: bool,
    ) -> bool {
        let (text, _) = block.claim_text(u.start, u.end);
        let comparative = self
            .p
            .cfg
            .uncited
            .comparative
            .as_ref()
            .and_then(|re| asserted_matches(re, &text).into_iter().next());
        if nums.is_empty() && comparative.is_none() {
            return false;
        }
        // Questions and hypothetical or normative sentences ("must fit 4,096
        // tokens") assert nothing that a source could support.
        let question = text.trim_end().ends_with(['?', '\u{FF1F}']);
        if question || (comparative.is_none() && config::has_modal(&text)) {
            return false;
        }
        if !report
            || (self.p.cfg.uncited.links_are_evidence && block.links > 0)
            || self.has_valid_label(block)
        {
            return true;
        }
        let what = match comparative {
            Some(m) => format!(
                "comparative claim (\"{}\")",
                m.as_str().trim_matches(|c: char| !c.is_alphanumeric())
            ),
            None => format!(
                "quantitative claim ({})",
                nums.iter()
                    .take(3)
                    .map(|n| n.shown.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let mut data = Map::new();
        data.insert("sentence".into(), json!(quote_excerpt(&text, 200)));
        data.insert(
            "numbers".into(),
            json!(nums.iter().map(|n| &n.shown).collect::<Vec<_>>()),
        );
        self.emit(
            Rule::UncitedClaim,
            Severity::Warn,
            line,
            format!("sentence makes a {what} without a citation or evidence label"),
            vec![],
            data,
        );
        true
    }

    /// The `data` of an `evidence-label` finding about `block`, starting with
    /// `paragraph_key`. The key tells apart paragraphs whose findings would
    /// otherwise share a message and data, and survives a move. It hashes
    /// the paragraph's first sentence with a letter or digit, without
    /// citation markers, together with the citation keys of that sentence.
    /// A paragraph without such a sentence hashes only its citation keys.
    /// Sentences are split before inline quotations are joined, so the key
    /// does not depend on the registry or the configuration.
    fn label_data(&self, block: &Block) -> Map<String, Value> {
        let units = doc::units(block);
        let first = units
            .iter()
            .map(|u| (u, key_text(block, u)))
            .find(|(_, t)| t.chars().any(char::is_alphanumeric));
        let (text, cites) = match first {
            Some((u, t)) => (t, u.cites.clone()),
            None => (String::new(), block.cites_in(0, block.chars.len())),
        };
        let mut keys: Vec<&str> = cites
            .iter()
            .flat_map(|&c| self.doc.citations[c as usize].keys.iter())
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys.dedup();
        // U+001F separates the text from the keys and is not whitespace, so
        // the hashed text is never empty.
        let key = normalize::sentence_key(&format!("{}\u{1F}{}", text.trim(), keys.join(" ")))
            .expect("the separator is text");
        let mut data = Map::new();
        data.insert("paragraph_key".into(), json!(hex(&key)));
        data
    }

    fn check_label(&mut self, block: &Block, claims: bool) {
        let labels = &self.p.cfg.labels;
        let line = self.doc.line_of(block.first_offset());
        let label = doc::leading_label(block);
        let valid = label
            .as_ref()
            .filter(|(l, _)| labels.vocabulary.contains(l))
            .map(|(l, _)| l.clone());
        if let (Some((l, _)), None) = (&label, &valid)
            && let Some(near) = labels
                .vocabulary
                .iter()
                .find(|v| v.eq_ignore_ascii_case(l) || edit_distance(v, l) <= 2)
        {
            let mut data = self.label_data(block);
            data.insert("label".into(), json!(l));
            data.insert("suggestion".into(), json!(near));
            self.emit(
                Rule::EvidenceLabel,
                Severity::Error,
                line,
                format!("evidence label `{l}` is not in the vocabulary; did you mean `{near}`?"),
                vec![],
                data,
            );
            return;
        }
        let cites = block.cites_in(0, block.chars.len());
        let needs = match labels.require {
            LabelRequire::None => false,
            LabelRequire::All => true,
            LabelRequire::Claims => claims || !cites.is_empty(),
        };
        if needs && valid.is_none() {
            let mut data = self.label_data(block);
            data.insert("vocabulary".into(), json!(labels.vocabulary));
            self.emit(
                Rule::EvidenceLabel,
                Severity::Warn,
                line,
                format!(
                    "paragraph makes claims without an evidence label ({})",
                    labels.vocabulary.join(", ")
                ),
                vec![],
                data,
            );
            return;
        }
        let Some(l) = valid else { return };
        let Some(&min) = labels.min_depth.get(&l) else {
            return;
        };
        for r in self.refs_of(&cites) {
            let key = self.key(r).to_string();
            let depth = self.p.registry.refs[r].depth;
            if depth.is_some_and(|d| d >= min) {
                continue;
            }
            let mut data = self.label_data(block);
            data.insert("label".into(), json!(l));
            data.insert("reference".into(), json!(key));
            data.insert("depth".into(), json!(depth));
            data.insert("required".into(), json!(min));
            let (sev, msg) = match depth {
                Some(d) => (
                    Severity::Error,
                    format!(
                        "paragraph labelled `{l}` cites reference {key} at depth {d} (needs {min})"
                    ),
                ),
                None => (
                    Severity::Info,
                    format!(
                        "paragraph labelled `{l}` cites reference {key}, which has no recorded depth (needs {min})"
                    ),
                ),
            };
            self.emit(Rule::EvidenceLabel, sev, line, msg, vec![], data);
        }
    }

    // ---- reference lists ------------------------------------------------------

    fn reference_entry(&mut self, block: &Block) {
        let line = self.doc.line_of(block.first_offset());
        let lead = block.chars.iter().find(|p| !p.c.is_whitespace());
        let (keys, from_marker): (Vec<String>, bool) = match lead {
            Some(p) if p.c == MARK => (
                self.doc.citations[p.cite.expect("marker has a citation") as usize]
                    .keys
                    .clone(),
                true,
            ),
            _ => match block.ordinal {
                Some(n) => (vec![n.to_string()], false),
                None => return,
            },
        };
        let [key] = keys.as_slice() else { return };
        let Some(r) = self.p.registry.get(key) else {
            if !from_marker {
                let mut data = Map::new();
                data.insert("reference".into(), json!(key));
                self.emit(
                    Rule::ReferenceListMismatch,
                    Severity::Error,
                    line,
                    format!("reference-list entry {key} has no registry record"),
                    vec![],
                    data,
                );
            }
            return;
        };
        let (text, _) = block.text(0, block.chars.len());
        let entry_skel: Vec<char> = normalize::skeleton(&normalize::normalize_str(&text)).0;
        let entry_str: String = entry_skel.iter().collect();
        let mut problems: Vec<String> = Vec::new();
        let mut data = Map::new();
        if let Some(title) = &r.title {
            let t: Vec<char> = normalize::skeleton(&normalize::normalize_str(title)).0;
            let ts: String = t.iter().collect();
            if !t.is_empty() && !entry_str.contains(&ts) {
                let (d, _, _) = matcher::align(&t, &entry_skel);
                let sim = 1.0 - d as f64 / t.len() as f64;
                if sim < self.p.cfg.reference_list.title_similarity {
                    problems.push(format!(
                        "title differs from registry title \"{}\" (similarity {:.2})",
                        quote_excerpt(title, 120),
                        sim.max(0.0)
                    ));
                    data.insert("title_similarity".into(), json!(round3(sim.max(0.0))));
                }
            }
        }
        if let Some(year) = r.year {
            let years = entry_years(&text);
            if years.is_empty() {
                problems.push(format!("year {year} is missing"));
            } else if !years.contains(&year) {
                problems.push(format!(
                    "year {} differs from registry year {year}",
                    years
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join("/")
                ));
                data.insert("entry_years".into(), json!(years));
            }
        }
        if let Some(first) = r.authors.first() {
            let surname = surname(first);
            let s = normalize::skeleton_str(surname);
            if !s.is_empty() && !entry_str.contains(&s) {
                problems.push(format!("first author {surname} is missing"));
            }
        }
        if problems.is_empty() {
            return;
        }
        data.insert("reference".into(), json!(key));
        data.insert("problems".into(), json!(problems));
        data.insert(
            "registry".into(),
            json!({"title": r.title, "year": r.year, "first_author": r.authors.first()}),
        );
        let evidence = vec![Evidence {
            path: "registry".into(),
            locator: format!("line {}", r.line),
        }];
        self.emit(
            Rule::ReferenceListMismatch,
            Severity::Error,
            line,
            format!("reference-list entry {key}: {}", problems.join("; ")),
            evidence,
            data,
        );
    }
}

/// The closest placement of a quotation in one cited source text.
struct BestMatch<'a> {
    entry: &'a TextEntry,
    text: Arc<IndexedText>,
    hit: Approx,
    key: &'a str,
    /// The placement is exact but starts or ends inside a source token.
    cuts: bool,
}

struct ClaimKind {
    name: String,
    /// Block positions of keyword matches.
    keywords: Vec<usize>,
    /// Block positions of numbers, for the number kind.
    numbers: Vec<usize>,
}

#[derive(Clone)]
struct DocNumber {
    tok: NumTok,
    /// Block character index of the number.
    at: usize,
    shown: String,
    context: Vec<String>,
}

fn unit_offset(block: &Block, u: &Unit) -> usize {
    block.chars[u.start..u.end]
        .iter()
        .find(|p| !p.c.is_whitespace())
        .map_or(0, |p| p.off as usize)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Approximate match of a segment; long segments are scored by their worst chunk.
fn worst_chunk(text: &IndexedText, seg: &[char]) -> Option<Approx> {
    const CHUNK: usize = 400;
    if seg.len() <= CHUNK + CHUNK / 2 {
        return matcher::best_approx(text, seg);
    }
    let mut worst: Option<Approx> = None;
    for chunk in seg.chunks(CHUNK) {
        let hit = matcher::best_approx(text, chunk).unwrap_or(Approx {
            similarity: 0.0,
            start: 0,
            end: 0,
        });
        if worst.is_none_or(|w| hit.similarity < w.similarity) {
            worst = Some(hit);
        }
    }
    worst
}

fn surname(author: &str) -> &str {
    match author.split_once(',') {
        Some((last, _)) => last.trim(),
        None => author.split_whitespace().last().unwrap_or(author),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "these", "those", "are", "was", "were",
    "has", "have", "had", "its", "their", "our", "which", "into", "than", "then", "also", "only",
    "both", "each", "per", "all", "any", "not", "but", "can", "may", "more", "most", "less",
    "over", "under", "between", "among", "about", "after", "before", "when", "while", "where",
    "such", "other", "using", "used", "based", "via", "reports", "reported", "shows", "achieves",
    "reaches",
];

/// Up to six content words nearest to a number, closest first.
fn context_words(chars: &[char], start: usize, end: usize) -> Vec<String> {
    let mut words: Vec<(usize, String)> = normalize::words(chars)
        .into_iter()
        .filter(|(_, _, w)| !STOPWORDS.contains(&w.as_str()))
        .map(|(s, e, w)| {
            let dist = if e <= start {
                start - e
            } else {
                s.saturating_sub(end)
            };
            (dist, w)
        })
        .collect();
    words.sort_by_key(|(d, _)| *d);
    let mut out: Vec<String> = Vec::new();
    for (_, w) in words {
        if !out.contains(&w) {
            out.push(w);
        }
        if out.len() == 6 {
            break;
        }
    }
    out
}

/// A year in a reference-list entry, with an optional disambiguating letter
/// ("2025a").
static YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(1[89]\d\d|20\d\d)[a-z]?\b").expect("valid regex"));

/// The years in a reference-list entry, skipping digits inside identifiers
/// such as `arXiv:2012.01234` or `10.1145/2009916`.
fn entry_years(text: &str) -> Vec<i64> {
    let b = text.as_bytes();
    YEAR.captures_iter(text)
        .filter_map(|c| {
            let m = c.get(1)?;
            let before = m.start().checked_sub(1).map(|i| b[i]);
            let after = &b[m.end()..];
            let in_id = matches!(before, Some(b'.' | b'/' | b':'))
                || matches!(after, [b'/', ..] | [b'.', b'0'..=b'9', ..]);
            if in_id { None } else { m.as_str().parse().ok() }
        })
        .collect()
}

/// What a masked span is; it decides which numbers the mask may hide.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MaskKind {
    /// URLs, DOIs, arXiv ids, `#N`, `10-K`: hide any number inside.
    Identifier,
    /// Dates and times: hide plain integers only.
    Date,
    /// Section, table and other locators: hide integers and dotted
    /// numbers such as `3.2`, never percentages or thousands.
    Locator,
}

impl MaskKind {
    fn hides(self, tok: &NumTok) -> bool {
        let quantity = tok.percent || tok.separated || tok.scaled.is_some();
        match self {
            MaskKind::Identifier => true,
            MaskKind::Date => !quantity && !tok.decimal,
            MaskKind::Locator => !quantity,
        }
    }
}

/// Spans whose digits are identifiers, dates or locators, not quantities. A
/// mask hides a number only when it covers the whole number.
static MASKS: LazyLock<Vec<(MaskKind, Regex)>> = LazyLock::new(|| {
    const MONTH: &str = r"(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b\.?";
    const DAY: &str = r"(?:0?[1-9]|[12]\d|3[01])";
    const MON: &str = r"(?:0?[1-9]|1[0-2])";
    const YEAR: &str = r"(?:19|20)\d{2}";
    const LOC: &str = r"[a-z]?\.?[1-9]\d*(?:\.\d+)*[a-z]?";
    let section_words = r"sections?|secs?\.|tables?|tabs?\.|figures?|figs?\.|appendix|appendices|app\.|equations?|eqs?\.|algorithms?|alg\.|chapters?|ch\.|lemmas?|theorems?|definitions?|propositions?|corollar(?:y|ies)|steps?|lines?|pages?|pp?\.|nos?\.|refs?\.|references?|items?|versions?|v\.|examples?|rows?|columns?|cols?\.|questions?|q|rq|footnotes?|notes?|exhibits?|parts?|annex|rules?|articles?|art\.|paragraphs?|para\.|claims?|hypothes[ie]s|stages?|phases?|rounds?|runs?|iterations?|epochs?|layers?|seed|sources?|ids?|identifiers?";
    use MaskKind::{Date, Identifier, Locator};
    [
        (Identifier, r"(?i)\b(?:https?://|www\.)\S+".to_string()),
        (Identifier, r"\b10\.\d{4,9}/\S+".to_string()),
        (Identifier, r"\b\d{4}\.\d{4,5}(?:v\d+)?\b".to_string()),
        (Identifier, r"[#№]\s*\d+".to_string()),
        (Identifier, r"\b\d{1,2}-[A-Z]\b".to_string()),
        // 2026-10-10, 2026/10, 2026.10.10
        (Date, format!(r"\b{YEAR}(?:[-/]{MON}(?:[-/]{DAY})?|\.{MON}\.{DAY})\b")),
        // 10/10/2026, 10-10-26, 10.10.2026
        (Date, format!(r"\b(?:{DAY}/{DAY}/|{DAY}-{DAY}-|{DAY}\.{DAY}\.)(?:{YEAR}|\d{{2}})\b")),
        (Date, r"\b(?:[01]?\d|2[0-3]):[0-5]\d(?::[0-5]\d)?\b".to_string()),
        (Date, format!(r"(?i)\b{DAY}(?:st|nd|rd|th)?\s*{MONTH}(?:,?\s*{YEAR})?\b")),
        (Date, format!(r"(?i)\b{MONTH}\s*{DAY}(?:st|nd|rd|th)?(?:,?\s*{YEAR})?\b")),
        (Locator, format!(r"(?i)(?:§+\s*|\b(?:{section_words})\s*)(?:{LOC})(?:\s*(?:[-,&/]|and|to|or)\s*(?:{LOC}))*")),
    ]
    .into_iter()
    .map(|(k, p)| (k, Regex::new(&p).expect("valid mask regex")))
    .collect()
});

/// Checks one document and returns its findings and counts.
pub fn check_document(p: &Project, file: &str, src: String, order: usize) -> (Vec<Finding>, Stats) {
    let doc = Doc::parse(src, &p.cfg.citations, &p.cfg.reference_list.headings);
    let mut ctx = Ctx {
        p,
        doc: &doc,
        file,
        order,
        out: Vec::new(),
        stats: Stats {
            files: 1,
            ..Stats::default()
        },
        resolved: Vec::new(),
        implied: p.note_reference(file),
    };
    ctx.run();
    let Ctx { mut out, stats, .. } = ctx;
    out.sort_by(|a, b| a.order.cmp(&b.order).then(a.rule.cmp(&b.rule)));
    (out, stats)
}
