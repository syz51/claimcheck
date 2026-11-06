//! Markdown documents as blocks of plain text with citation markers.
//!
//! Citation patterns run on the raw Markdown. The CommonMark parser then
//! yields the visible text of each leaf block (paragraph, list item, heading,
//! table row); code, HTML, images and front matter are dropped. Visible
//! characters inside a citation's span collapse into one [`MARK`] character
//! that points at the citation, so every later step sees `text MARK text`
//! with the original byte offset of every character.

use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;

use crate::config::CitationPattern;

/// Stands for one citation in block text.
pub const MARK: char = '\u{FFFC}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Citation {
    pub keys: Vec<String>,
    /// Visible label when the pattern has a `label` group.
    pub label: Option<String>,
    pub start: usize,
    pub end: usize,
    pub raw: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    ListItem,
    Heading(u8),
    TableRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PChar {
    pub c: char,
    pub off: u32,
    pub code: bool,
    pub strong: bool,
    /// Inside the text of a link that is not a citation.
    pub link: bool,
    pub cite: Option<u32>,
}

impl PChar {
    fn separator(off: u32) -> PChar {
        PChar {
            c: '\n',
            off,
            code: false,
            strong: false,
            link: false,
            cite: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    /// Blockquote this block belongs to (outermost), numbered per document.
    pub quote_group: Option<u32>,
    pub chars: Vec<PChar>,
    /// Links that are not citations.
    pub links: usize,
    /// Number of an ordered-list item.
    pub ordinal: Option<u64>,
    pub in_reference_list: bool,
}

impl Block {
    fn new(kind: BlockKind, quote_group: Option<u32>, ordinal: Option<u64>) -> Block {
        Block {
            kind,
            quote_group,
            chars: Vec::new(),
            links: 0,
            ordinal,
            in_reference_list: false,
        }
    }

    pub fn is_prose(&self) -> bool {
        matches!(self.kind, BlockKind::Paragraph | BlockKind::ListItem)
    }

    /// Citation indexes in `start..end`, in order, without repeats.
    pub fn cites_in(&self, start: usize, end: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for p in &self.chars[start..end] {
            if let Some(c) = p.cite
                && !out.contains(&c)
            {
                out.push(c);
            }
        }
        out
    }

    /// Visible text of `start..end` without citation markers, with a map from
    /// each byte of the returned string to its index in `chars`.
    pub fn text(&self, start: usize, end: usize) -> (String, Vec<usize>) {
        let mut s = String::with_capacity(end - start);
        let mut map = Vec::with_capacity(end - start);
        for (i, p) in self.chars[start..end].iter().enumerate() {
            if p.c == MARK {
                continue;
            }
            s.push(p.c);
            map.resize(s.len(), start + i);
        }
        (s, map)
    }

    /// Like [`Block::text`], but also without the text of non-citation links
    /// (for example `[A12 method audit](notes.md)`), which labels a pointer
    /// rather than stating a claim.
    pub fn claim_text(&self, start: usize, end: usize) -> (String, Vec<usize>) {
        let mut s = String::with_capacity(end - start);
        let mut map = Vec::with_capacity(end - start);
        for (i, p) in self.chars[start..end].iter().enumerate() {
            if p.c == MARK || p.link {
                continue;
            }
            s.push(p.c);
            map.resize(s.len(), start + i);
        }
        (s, map)
    }

    pub fn first_offset(&self) -> usize {
        self.chars
            .iter()
            .find(|p| !p.c.is_whitespace())
            .or(self.chars.first())
            .map_or(0, |p| p.off as usize)
    }
}

/// A sentence (prose) or a whole heading or table row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub start: usize,
    pub end: usize,
    pub cites: Vec<u32>,
}

pub struct Doc {
    pub src: String,
    line_starts: Vec<usize>,
    pub citations: Vec<Citation>,
    pub blocks: Vec<Block>,
}

static PANDOC_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@([\p{L}\p{N}_][\p{L}\p{N}_:.#$%&+?<>~/-]*)").expect("valid regex")
});

/// Splits the text of a `keys` group into keys: `@key` tokens when present,
/// otherwise comma- or semicolon-separated items with numeric ranges expanded.
pub fn parse_keys(s: &str) -> Vec<String> {
    if s.contains('@') {
        return PANDOC_KEY
            .captures_iter(s)
            .map(|c| c[1].trim_end_matches(['.', ':', ',', ';', '?']).to_string())
            .filter(|k| !k.is_empty())
            .collect();
    }
    let mut out = Vec::new();
    for part in s.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let range = part.split_once(['-', '\u{2013}']).and_then(|(a, b)| {
            let a: u64 = a.trim().parse().ok()?;
            let b: u64 = b.trim().parse().ok()?;
            (b >= a && b - a <= 200).then_some((a, b))
        });
        match range {
            Some((a, b)) => out.extend((a..=b).map(|n| n.to_string())),
            None => out.push(part.to_string()),
        }
    }
    out
}

fn find_citations(src: &str, patterns: &[CitationPattern]) -> Vec<Citation> {
    let mut found: Vec<Citation> = Vec::new();
    for p in patterns {
        for caps in p.regex.captures_iter(src) {
            let Some(span) = caps.name("cite").or_else(|| caps.get(0)) else {
                continue;
            };
            // `[3](url)` is an inline link. The label of a reference-style link
            // (`[text][3]`) never reaches the visible text, so it never becomes a marker.
            if src[span.end()..].starts_with('(') {
                continue;
            }
            let keys = match (caps.name("key"), caps.name("keys")) {
                (Some(k), _) => vec![k.as_str().trim().to_string()],
                (None, Some(k)) => parse_keys(k.as_str()),
                (None, None) => continue,
            };
            if keys.is_empty() || keys.iter().any(String::is_empty) {
                continue;
            }
            if found
                .iter()
                .any(|c| c.start < span.end() && span.start() < c.end)
            {
                continue;
            }
            found.push(Citation {
                keys,
                label: caps.name("label").map(|m| m.as_str().to_string()),
                start: span.start(),
                end: span.end(),
                raw: span.as_str().to_string(),
            });
        }
    }
    found.sort_by_key(|c| c.start);
    found
}

struct Builder<'a> {
    src: &'a str,
    citations: &'a [Citation],
    blocks: Vec<Block>,
    cur: Option<Block>,
    quote_depth: usize,
    quote_group: u32,
    skip: usize,
    strong: usize,
    lists: Vec<Option<u64>>,
    /// For each open link: whether it is a non-citation link.
    link_stack: Vec<bool>,
}

impl Builder<'_> {
    fn group(&self) -> Option<u32> {
        (self.quote_depth > 0).then_some(self.quote_group)
    }

    fn flush(&mut self) {
        if let Some(b) = self.cur.take()
            && b.chars.iter().any(|p| !p.c.is_whitespace())
        {
            self.blocks.push(b);
        }
    }

    fn open(&mut self, kind: BlockKind, ordinal: Option<u64>) {
        self.flush();
        self.cur = Some(Block::new(kind, self.group(), ordinal));
    }

    fn ensure_open(&mut self) -> &mut Block {
        if self.cur.is_none() {
            self.cur = Some(Block::new(BlockKind::Paragraph, self.group(), None));
        }
        self.cur.as_mut().expect("block is open")
    }

    fn citation_at(&self, off: usize) -> Option<u32> {
        let i = self.citations.partition_point(|c| c.end <= off);
        let c = self.citations.get(i)?;
        (c.start <= off).then_some(i as u32)
    }

    fn push_text(&mut self, text: &str, range: std::ops::Range<usize>, code: bool) {
        let linear = self.src.get(range.clone()) == Some(text);
        let strong = self.strong > 0;
        let link = self.link_stack.contains(&true);
        let cites: Vec<Option<u32>> = text
            .char_indices()
            .map(|(i, _)| {
                if code {
                    None
                } else {
                    self.citation_at(if linear { range.start + i } else { range.start })
                }
            })
            .collect();
        let block = self.ensure_open();
        for ((i, c), cite) in text.char_indices().zip(cites) {
            let off = if linear { range.start + i } else { range.start };
            let off = u32::try_from(off).unwrap_or(u32::MAX);
            if let Some(ci) = cite {
                if block.chars.last().and_then(|p| p.cite) != Some(ci) {
                    block.chars.push(PChar {
                        c: MARK,
                        off,
                        code: false,
                        strong,
                        link: false,
                        cite: Some(ci),
                    });
                }
                continue;
            }
            block.chars.push(PChar {
                c,
                off,
                code,
                strong,
                link,
                cite: None,
            });
        }
    }

    fn event(&mut self, event: Event<'_>, range: std::ops::Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag),
            _ if self.skip > 0 => {}
            Event::Text(t) => self.push_text(&t, range, false),
            Event::Code(t) => self.push_text(&t, range, true),
            Event::SoftBreak | Event::HardBreak => {
                let off = u32::try_from(range.start).unwrap_or(u32::MAX);
                self.ensure_open().chars.push(PChar::separator(off));
            }
            Event::Rule => self.flush(),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: std::ops::Range<usize>) {
        if self.skip > 0 {
            if matches!(
                tag,
                Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::MetadataBlock(_) | Tag::Image { .. }
            ) {
                self.skip += 1;
            }
            return;
        }
        match tag {
            Tag::Paragraph => {
                let keep = self.cur.as_ref().is_some_and(|b| {
                    b.kind == BlockKind::ListItem && b.chars.iter().all(|p| p.c.is_whitespace())
                });
                if !keep {
                    self.open(BlockKind::Paragraph, None);
                }
            }
            Tag::Heading { level, .. } => self.open(BlockKind::Heading(level as u8), None),
            Tag::BlockQuote(_) => {
                self.flush();
                if self.quote_depth == 0 {
                    self.quote_group += 1;
                }
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::MetadataBlock(_) => {
                self.flush();
                self.skip += 1;
            }
            Tag::Image { .. } => self.skip += 1,
            Tag::List(first) => {
                self.flush();
                self.lists.push(first);
            }
            Tag::Item => {
                let ordinal = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let v = *n;
                        *n += 1;
                        Some(v)
                    }
                    _ => None,
                };
                self.open(BlockKind::ListItem, ordinal);
            }
            Tag::TableHead | Tag::TableRow => self.open(BlockKind::TableRow, None),
            Tag::Table(_) | Tag::FootnoteDefinition(_) => self.flush(),
            Tag::Strong => self.strong += 1,
            Tag::Link { .. } => {
                let plain = self.citation_at(range.start).is_none();
                if plain {
                    self.ensure_open().links += 1;
                }
                self.link_stack.push(plain);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if self.skip > 0 {
            if matches!(
                tag,
                TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::MetadataBlock(_) | TagEnd::Image
            ) {
                self.skip -= 1;
            }
            return;
        }
        match tag {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::Item
            | TagEnd::TableHead
            | TagEnd::TableRow => {
                self.flush();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::TableCell => {
                if let Some(b) = self.cur.as_mut() {
                    let off = b.chars.last().map_or(0, |p| p.off);
                    b.chars.push(PChar::separator(off));
                }
            }
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Link => {
                self.link_stack.pop();
            }
            _ => {}
        }
    }
}

fn heading_key(s: &str) -> String {
    s.trim().trim_end_matches([':', '：']).trim().to_lowercase()
}

impl Doc {
    pub fn parse(src: String, patterns: &[CitationPattern], reference_headings: &[String]) -> Doc {
        let citations = find_citations(&src, patterns);
        let mut line_starts = vec![0];
        line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        let opts = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
        let mut b = Builder {
            src: &src,
            citations: &citations,
            blocks: Vec::new(),
            cur: None,
            quote_depth: 0,
            quote_group: 0,
            skip: 0,
            strong: 0,
            lists: Vec::new(),
            link_stack: Vec::new(),
        };
        for (event, range) in Parser::new_ext(&src, opts).into_offset_iter() {
            b.event(event, range);
        }
        b.flush();
        let mut blocks = b.blocks;
        let headings: Vec<String> = reference_headings.iter().map(|h| heading_key(h)).collect();
        let mut list_level: Option<u8> = None;
        for block in &mut blocks {
            if let BlockKind::Heading(level) = block.kind {
                let (text, _) = block.text(0, block.chars.len());
                if headings.contains(&heading_key(&text)) {
                    list_level = Some(level);
                } else if list_level.is_some_and(|l| level <= l) {
                    list_level = None;
                }
            } else {
                block.in_reference_list = list_level.is_some();
            }
        }
        Doc {
            src,
            line_starts,
            citations,
            blocks,
        }
    }

    /// 1-based line of a byte offset.
    pub fn line_of(&self, off: usize) -> usize {
        self.line_starts.partition_point(|s| *s <= off)
    }
}

const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "et al", "al", "vs", "cf", "fig", "figs", "sec", "secs", "eq", "eqs", "no",
    "nos", "vol", "pp", "p", "ch", "approx", "resp", "incl", "dr", "mr", "ms", "mrs", "prof", "st",
    "jr", "ref", "refs", "tab", "app", "est", "ca", "viz",
];

fn is_closing(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | ')' | ']' | '”' | '’' | '」' | '』' | '）' | '»'
    )
}

/// Sentences of a prose block, or the whole block for headings and table rows.
pub fn units(block: &Block) -> Vec<Unit> {
    let chars = &block.chars;
    let n = chars.len();
    let make = |s: usize, e: usize| Unit {
        start: s,
        end: e,
        cites: block.cites_in(s, e),
    };
    if !block.is_prose() {
        return vec![make(0, n)];
    }
    let mut out: Vec<Unit> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < n {
        let c = chars[i].c;
        let boundary = if chars[i].code {
            false
        } else if matches!(c, '。' | '！' | '？') {
            true
        } else if matches!(c, '!' | '?' | '.') {
            let mut j = i + 1;
            while j < n && is_closing(chars[j].c) {
                j += 1;
            }
            let spaced = j == n || chars[j].c.is_whitespace();
            let next = chars[j..]
                .iter()
                .map(|p| p.c)
                .find(|c| !c.is_whitespace() && *c != MARK);
            spaced
                && next.is_none_or(|c| !c.is_lowercase())
                && (c != '.' || !abbreviation_before(chars, i))
        } else {
            false
        };
        if boundary {
            let mut j = i + 1;
            while j < n && is_closing(chars[j].c) {
                j += 1;
            }
            // Citations right after a full stop belong to the sentence it ends.
            let mut k = j;
            while k < n && (chars[k].c.is_whitespace() || chars[k].c == MARK) {
                k += 1;
            }
            if k == n || chars[j..k].iter().any(|p| p.c == MARK) {
                j = k;
            }
            out.push(make(start, j));
            start = j;
            i = j;
            continue;
        }
        i += 1;
    }
    if start < n {
        out.push(make(start, n));
    }
    out.retain(|u| {
        chars[u.start..u.end].iter().any(|p| p.c.is_alphanumeric()) || !u.cites.is_empty()
    });
    out
}

fn abbreviation_before(chars: &[PChar], dot: usize) -> bool {
    let mut s = dot;
    while s > 0 && !chars[s - 1].c.is_whitespace() && !matches!(chars[s - 1].c, '(' | '[' | MARK) {
        s -= 1;
    }
    let word: String = chars[s..dot]
        .iter()
        .flat_map(|p| p.c.to_lowercase())
        .collect();
    if word.chars().count() == 1 && word.chars().all(char::is_alphabetic) {
        return true; // an initial such as "J. Smith"
    }
    if word.ends_with('.') || word.is_empty() {
        return false;
    }
    if ABBREVIATIONS.contains(&word.as_str()) {
        return true;
    }
    // "et al." is two words.
    word == "al" || {
        let before: String = chars[..s]
            .iter()
            .rev()
            .take(4)
            .map(|p| p.c)
            .collect::<String>();
        before.starts_with(" te") && word == "al"
    }
}

/// Character ranges (content, without the marks) of quotations in a block.
/// Quotations nested in another quotation are dropped.
pub fn quotes(block: &Block) -> Vec<(usize, usize)> {
    let chars = &block.chars;
    let n = chars.len();
    let alnum = |i: Option<usize>| {
        i.and_then(|i| chars.get(i))
            .is_some_and(|p| p.c.is_alphanumeric())
    };
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut curly: Vec<usize> = Vec::new();
    let mut single: Vec<usize> = Vec::new();
    let mut cjk: Vec<(char, usize)> = Vec::new();
    let mut straight: Option<usize> = None;
    for i in 0..n {
        if chars[i].code {
            continue;
        }
        let prev = i.checked_sub(1);
        match chars[i].c {
            '“' => curly.push(i),
            '”' => {
                if let Some(s) = curly.pop() {
                    found.push((s + 1, i));
                }
            }
            '‘' if !alnum(prev) => single.push(i),
            '’' if !alnum(Some(i + 1)) => {
                if let Some(s) = single.pop() {
                    found.push((s + 1, i));
                }
            }
            '「' | '『' | '«' => cjk.push((chars[i].c, i)),
            c @ ('」' | '』' | '»') => {
                let open = match c {
                    '」' => '「',
                    '』' => '『',
                    _ => '«',
                };
                if let Some(pos) = cjk.iter().rposition(|(o, _)| *o == open) {
                    let (_, s) = cjk[pos];
                    cjk.truncate(pos);
                    found.push((s + 1, i));
                }
            }
            '"' => match straight {
                None if !alnum(prev) && chars.get(i + 1).is_some_and(|p| !p.c.is_whitespace()) => {
                    straight = Some(i)
                }
                Some(s) if prev.is_some_and(|p| !chars[p].c.is_whitespace()) => {
                    found.push((s + 1, i));
                    straight = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
    found.sort_unstable();
    let all = found.clone();
    found.retain(|&(s, e)| {
        !all.iter()
            .any(|&(s2, e2)| (s2, e2) != (s, e) && s2 <= s && e <= e2)
    });
    found
}

/// A leading `Label:` of a block: the label text and whether it is bold.
pub fn leading_label(block: &Block) -> Option<(String, bool)> {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*([^\s:：][^:：\n]{0,40}?)\s*[:：](?:\s|$)").expect("valid regex")
    });
    let (text, map) = block.text(0, block.chars.len());
    let caps = LABEL.captures(&text)?;
    let m = caps.get(1)?;
    let strong = block.chars[map[m.start()]].strong;
    Some((m.as_str().trim().to_string(), strong))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    fn doc(src: &str) -> Doc {
        let cfg = Config::from_toml(Path::new("/p"), "").unwrap();
        Doc::parse(
            src.to_string(),
            &cfg.citations,
            &cfg.reference_list.headings,
        )
    }

    fn unit_texts(d: &Doc, b: usize) -> Vec<String> {
        units(&d.blocks[b])
            .iter()
            .map(|u| d.blocks[b].text(u.start, u.end).0.trim().to_string())
            .collect()
    }

    #[test]
    fn citation_patterns() {
        let d = doc(
            "A [87](references.md#reference-87) and [3, 5–7] and [@smith2020; @doe:x]. See [x](http://e.com) and [2](u).\n\n```\n[9]\n```\n",
        );
        let keys: Vec<Vec<String>> = d.citations.iter().map(|c| c.keys.clone()).collect();
        assert_eq!(
            keys,
            vec![
                vec!["87".to_string()],
                vec!["3", "5", "6", "7"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                vec!["smith2020".to_string(), "doe:x".to_string()],
                vec!["9".to_string()]
            ]
        );
        // The fenced [9] never reaches block text.
        let marks: usize = d
            .blocks
            .iter()
            .flat_map(|b| &b.chars)
            .filter(|p| p.c == MARK)
            .count();
        assert_eq!(marks, 3);
        assert_eq!(d.blocks[0].links, 2);
    }

    #[test]
    fn adjacent_bare_citations_and_reference_links() {
        let d = doc(
            "Prior [87][174] work; see [the docs][1] and [0, 0].\n\n[1]: https://example.com\n",
        );
        let used: Vec<Vec<String>> = d.blocks[0]
            .chars
            .iter()
            .filter_map(|p| p.cite)
            .map(|c| d.citations[c as usize].keys.clone())
            .collect();
        assert_eq!(used, [vec!["87".to_string()], vec!["174".to_string()]]);
        assert_eq!(d.blocks[0].links, 1);
    }

    #[test]
    fn label_mismatch_is_captured() {
        let d = doc("See [12](references.md#reference-21).");
        assert_eq!(d.citations[0].label.as_deref(), Some("12"));
        assert_eq!(d.citations[0].keys, ["21"]);
    }

    #[test]
    fn sentences_and_citation_attachment() {
        let d =
            doc("HiREC [87] uses queries, e.g. BM25. Dr. Smith agrees. It reports 12.3%. [5]\n");
        let u = units(&d.blocks[0]);
        assert_eq!(
            unit_texts(&d, 0),
            [
                "HiREC  uses queries, e.g. BM25.",
                "Dr. Smith agrees.",
                "It reports 12.3%."
            ]
        );
        assert_eq!(u[0].cites, [0]);
        assert_eq!(u[2].cites, [1]);
    }

    #[test]
    fn cjk_sentences() {
        let d = doc("本手法は有効である。精度は向上した［3］。");
        assert_eq!(units(&d.blocks[0]).len(), 2);
    }

    #[test]
    fn quote_kinds() {
        let d =
            doc("He said “a b c” and ‘d e f’ but HiREC’s \"g h\" and 「日本語の引用」 『内側』.");
        let b = &d.blocks[0];
        let got: Vec<String> = quotes(b).into_iter().map(|(s, e)| b.text(s, e).0).collect();
        assert_eq!(got, ["a b c", "d e f", "g h", "日本語の引用", "内側"]);
    }

    #[test]
    fn nested_quotes_are_dropped() {
        let d = doc("「外の『内』引用」");
        let b = &d.blocks[0];
        assert_eq!(quotes(b).len(), 1);
    }

    #[test]
    fn blocks_lists_tables_and_reference_sections() {
        let d = doc(
            "# Title\n\n1. first\n2. second [4]\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quoted [3]\n\n## References\n\n[4] Smith. A title. 2020.\n\n## Next\n\nafter\n",
        );
        let kinds: Vec<BlockKind> = d.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading(1),
                BlockKind::ListItem,
                BlockKind::ListItem,
                BlockKind::TableRow,
                BlockKind::TableRow,
                BlockKind::Paragraph,
                BlockKind::Heading(2),
                BlockKind::Paragraph,
                BlockKind::Heading(2),
                BlockKind::Paragraph,
            ]
        );
        assert_eq!(d.blocks[2].ordinal, Some(2));
        assert_eq!(d.blocks[5].quote_group, Some(1));
        assert!(d.blocks[7].in_reference_list);
        assert!(!d.blocks[9].in_reference_list);
        assert_eq!(d.line_of(d.blocks[7].first_offset()), 14);
    }

    #[test]
    fn labels() {
        let d = doc("**Verified:** text.\n\nHypothesis: more.\n\nNo label here.");
        assert_eq!(
            leading_label(&d.blocks[0]),
            Some(("Verified".to_string(), true))
        );
        assert_eq!(
            leading_label(&d.blocks[1]),
            Some(("Hypothesis".to_string(), false))
        );
        assert_eq!(leading_label(&d.blocks[2]), None);
    }
}
