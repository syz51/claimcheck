# claimcheck

`claimcheck` checks that a Markdown research document is supported by the sources it cites. It resolves citations against a reference registry. It finds quoted passages and numbers in the cited source texts. It flags claims that cite sources inspected too shallowly, and claims with no source at all.

It is built to avoid false positives on the "supported" side. A check that cannot be decided is reported as undecided and never counted as supported. A quotation that cites a source with no text on file gets `source-text-missing`, not a pass.

```text
notes.md:3: warn[quote-not-found] quoted text is not verbatim in the source text of reference 1; closest match similarity 0.95
    --> texts/1.txt (best match line 1, similarity 0.95)
    quote: "the retriever gathers supplementary evidence before answering"
    match: "Abstract. The retriever gathers complementary evi- dence before answering. Accuracy reaches 71.2 % on the held-out split of 1,234 questions."
notes.md:4: warn[number-not-in-source] 72.1% does not occur in the source text of reference 1; nearby numbers: 71.2, 1234
    --> texts/1.txt (line 2: 71.2)
    --> texts/1.txt (line 3: 1234)
notes.md:5: error[depth-insufficient] sentence makes a result claim but reference 2 was inspected only to depth abstract (result needs method)
notes.md:6: warn[uncited-claim] sentence makes a comparative claim ("outperforms") without a citation or evidence label
notes.md:7: error[citation-unresolved] citation key 9 is not in the registry
```

## Install

```sh
cargo install --path .
```

It needs Rust 1.99 or later.

## Usage

```sh
claimcheck check [PATHS]... [--config research.toml] [--registry references.jsonl]
                 [--format text|jsonl] [--summary] [--min-severity info|warn|error]
                 [--measure]
claimcheck rules
```

`PATHS` are Markdown files or directories. Directories are searched recursively for `.md` and `.markdown` files, and hidden entries are skipped. When no path is given, the `paths` from the configuration are used; if those are empty too (the default), `check` exits with status 2.

The search follows symbolic links, but walks each directory only once, so a link loop cannot repeat files. A file reached by several paths is checked once. A link that cannot be followed (dangling, or a loop the operating system refuses) is an error, since skipping it could silently hide documents; add it to `exclude` to skip it.

- **Text output** (the default) prints one finding per line as `file:line: severity[rule] message`. Indented lines give the source evidence, the quotation and the closest match. Excerpts are about 200 characters.
- **`--format jsonl`** prints one `rf.finding/1` record per line (schema below).
- **`--summary`** adds a table of counts by rule and severity, plus totals: files, citations resolved, quotations verbatim and numbers found. It goes to stdout in text mode and to stderr with `--format jsonl`.
- **`--min-severity`** hides lower-severity findings. The exit code still counts every error.
- **`--measure`** prints two `rf.measure/1` lines, last on stdout, in both formats: the sentences that cite a reference, then the sentences without a citation in paragraphs and list items outside blockquotes (see [below](#denominator-rfmeasure1)). Without it, the output is unchanged.
- **`claimcheck rules`** lists every rule with its default severity.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | No errors (warnings and info allowed). |
| 1 | At least one finding has severity `error`. |
| 2 | Usage, configuration, registry or file read failure. |

Documents are checked in parallel. Source texts are read, hashed and indexed once, when a check first needs them, so a large corpus costs little more than the documents themselves.

## Inputs

### Reference registry: `rf.reference/1`

The registry is a JSON Lines file with one reference per line, as written by `papers`. `claimcheck` reads these fields and ignores unknown ones:

| Field | Use |
| --- | --- |
| `schema` | Must be `rf.reference/1` or `rf.reference/1.x`. Another major version is rejected. |
| `key` | The citation key (unique). |
| `title`, `authors`, `year` | Compared with a document's own reference list. |
| `depth` | How far the source was inspected: `metadata` < `abstract` < `method` < `full` < `artifact` < `reproduced`. Null means unknown. |
| `texts[].path`, `texts[].sha256` | Extracted source text, relative to the project root, with an optional hash. |
| `duplicate_of` | The key this entry duplicates. An entry with no `texts` is checked against the texts of the survivor at the end of its chain; findings then name it in `data.survivors`. |
| `source.path` | The file the entry was built from, such as its paper note, relative to the project root. See [Paper notes](#paper-notes). |

The registry is located by the first of these that is set:

1. `--registry`;
2. `[claimcheck] registry`;
3. `[papers] registry`;
4. `references.jsonl` in the project root.

### Configuration: `research.toml`

`research.toml` is found by walking up from the working directory; `--config` overrides the search. Relative paths in it resolve against its directory, the project root. All keys are optional; unknown keys are an error. The defaults:

```toml
[claimcheck]
registry = "references.jsonl"
paths = []                # checked when no path is given, e.g. ["docs"]
exclude = []              # paths skipped while walking directories; compared after
                          # resolving symlinks, `.` and `..`, so `check ../docs` honours them

[claimcheck.citations]
# Regexes with named groups: `key`, or `keys` (a list such as "3, 5-7" or
# "@a; @b"), optional `label` (the visible number, compared with `key`) and
# optional `cite` (the span that counts as the citation; default: the whole match).
patterns = [
  '\[(?P<label>\d+)\]\([^)\s]*#reference-(?P<key>\d+)\)',              # [3](references.md#reference-3)
  '\[(?P<keys>[1-9]\d*(?:\s*[,;\x{2013}-]\s*[1-9]\d*)*)(?:\s*;\s*[^\[\]\n\d\s][^\[\]\n]*)?\]', # [3], [3, 5-7], [3; 9], [3; p. 4]
  '\[(?P<keys>[^\[\]]*@[^\[\]]+)\]',                                    # [@key], [see @a; @b]
]

[claimcheck.quotes]
min_words = 3          # shorter quotations are terms, not quotations
min_cjk_chars = 6      # the same for Chinese and Japanese text
error_below = 0.8      # closest-match similarity below this is an error; above, a warning
excerpt_chars = 200

[claimcheck.numbers]
min_integer = 100      # plain integers below this are not checked
exclude_years = true
year_min = 1900
year_max = 2100
context_window = 150   # characters searched around a context word for nearby numbers
nearest = 3            # nearby numbers reported
links_are_evidence = false  # a non-citation link in the same sentence lowers number-not-in-source to info
match_rounded = true   # a number may match a nearby source number it rounds (info)

[claimcheck.claims]
number_kind = "result" # kind suggested by a checked number (info only, never an error)
window_chars = 80      # a keyword must be this close to the citation, in the same clause

[claimcheck.claims.kinds]
result = ["outperforms", "achieves", "improves", "accuracy", "state of the art",
          "better than", "shows that", "reduces", "increases", "significantly", "..."]
method = ["proposes", "introduces", "algorithm", "pipeline", "trained", "fine-tuned",
          "implements", "uses", "applies", "loss function", "ablation", "..."]

[claimcheck.claims.required_depth]
# When this table is absent, `result` and `method` need "method", for whichever
# of them are kinds (or `number_kind`). When present, it replaces the defaults
# and every key must be a kind.
result = "method"
method = "method"

[claimcheck.uncited]
comparative = ["outperforms", "state of the art", "first to", "better than",
               "superior to", "more accurate than", "best-performing", "..."]
links_are_evidence = true  # a non-citation link in the paragraph counts as evidence

[claimcheck.labels]
vocabulary = []        # e.g. ["Verified", "Author-reported", "Inferred", "Hypothesis"]
require = "none"       # "none", "claims" (paragraphs that cite or claim) or "all"

[claimcheck.labels.min_depth]
# Verified = "full"    # every reference cited under this label needs this depth

[claimcheck.reference_list]
headings = ["references", "reference list", "bibliography", "works cited",
            "literature cited", "sources", "参考文献", "引用文献"]
title_similarity = 0.9

[claimcheck.paper_notes]
commentary_sections = []  # headings of the author's own commentary in paper notes,
                          # e.g. ["Trading implications", "Related"]

[claimcheck.severity]
# rule-id = "error" | "warn" | "info" | "off"

# [[claimcheck.kept_quote]]  # repeatable; see Kept quotations
# file = "docs/notes.md"
# quote = "Search and rank source material using the initial query"
# reason = "slide 2's own text beside its citation, not a quote of the paper"
```

`claimcheck rules` and the source (`src/config.rs`) give the full keyword lists. Keyword phrases match whole words, case-insensitively. A keyword within three words after a modal or conditional word does not count. Those words are must, should, could, would, might, may, can, cannot, whether, if and unless. A capitalised "May" is read as the month, unless it opens the sentence with no number after it. A keyword hyphenated to another word, as in "outcome-trained agents", does not count either.

A severity override sets the severity of every finding of the rule, whatever severity the rule would have given it. For example, `quote-not-found = "info"` makes both near misses (warn) and fabricated quotations (error) info, and `depth-insufficient = "error"` makes its info findings errors too. `off` drops every finding of the rule. A [kept quotation](#kept-quotations) stays info under any override except `off`, which drops it too.

## Rules

| Rule id | Default | What it reports |
| --- | --- | --- |
| `citation-unresolved` | error | A citation key is not in the registry. |
| `citation-duplicate` | warn | A citation uses a key whose registry entry duplicates another key. The message names the surviving key, or says that the `duplicate_of` chain is broken or cycles. |
| `citation-label-mismatch` | error | A citation's visible number differs from the key in its link, as in `[3](references.md#reference-4)`. |
| `quote-not-found` | error | Quoted text attached to a citation does not occur in the cited source text. Below `error_below` similarity it is an error; between that and verbatim, a warning. Both report the closest match with its line and similarity. |
| `source-text-missing` | warn | A quotation or number needs a cited source's text, but the registry lists none or the file cannot be read. The check is undecided, never "supported". Since 0.12.0, the finding on a document line adds to its `evidence` each text path the registry gives for those references, once, with locator `missing text of reference <key>`, after a quotation's best match in another cited text. A duplicate gives its survivor's paths, and a reference that lists no text adds none. The finding for an unreadable file, under the reference key, names the file with locator `file`. |
| `source-text-changed` | warn | A source text file's SHA-256 differs from the registry's `sha256`. It is reported once for the file, under the reference key. A quotation or number that occurs only in such a file is unverified, never "supported": it gets its own finding on the document line, with the file in `data.changed_text`. A match in another cited text whose hash agrees still verifies it. |
| `number-not-in-source` | warn | A number in a citing sentence occurs in none of the cited source texts. Up to `nearest` numbers near the sentence's words in the source are reported. It is info when `numbers.links_are_evidence` is set and the sentence holds a non-citation link, and when the number matches a nearby source number only as a rounding or a percentage (see Numbers). |
| `depth-insufficient` | error | A sentence makes a claim of a kind that needs deeper inspection than the cited reference's `depth`. It is info when the kind is uncertain (see below). |
| `uncited-claim` | warn | A sentence makes a quantitative or comparative claim with no citation, link or evidence label. A sentence in a paper note cites the note's entry, except in a commentary section (see [Paper notes](#paper-notes)). |
| `evidence-label` | warn | A label is outside the vocabulary but close to it (error), a required label is missing (warn), or a label's `min_depth` is not met (error). |
| `reference-list-mismatch` | error | An entry in a document's own reference list disagrees with the registry on title, year or first author's surname, or names an unknown key. |
| `stale-kept-quote` | warn | Since 0.13.0: a `[[claimcheck.kept_quote]]` entry matches no unverified quotation in its file, and the run checked that file. See [Kept quotations](#kept-quotations). |

### Citations

Citation patterns run on the raw Markdown, so link syntax such as `[3](references.md#reference-3)` is seen as written. Several things are not citations:

- citations inside code spans, code blocks, HTML or front matter;
- an inline link whose text happens to be a number, as in `[3](https://example.org)`;
- bracketed intervals that start at zero or below, such as `[0, 1]` or `[-3, 0]`. `[1, 2]` reads as two citations.

A citation placed right after a full stop, as in `… pipeline. [2]` or `… pipeline.[2] Next`, belongs to the sentence it ends. In `[3; 9]` both numbers are keys; text after a semicolon that does not start with a digit, as in `[3; p. 4]`, is a locator.

### Quotations

These are checked:

- text in straight `"…"` or curly `“…”` double quotes;
- curly single quotes `‘…’` (apostrophes are not quotes);
- guillemets `«…»`;
- Japanese `「…」` and `『…』`;
- blockquotes.

A quotation is checked when it has a citation in the same sentence or inside it, and has at least `min_words` words or `min_cjk_chars` CJK characters. Shorter ones are terms or scare quotes. A blockquote is checked when it contains a citation, or when the paragraph just before it cites and ends with a colon. A trailing attribution line such as `— Author [3]` is not part of the quoted text.

A sentence stop inside a checked inline quotation does not end the sentence that quotes it. In `Revenue rose 40 % and the CEO said "We grew fast this year. We will grow again." [1]`, the citation covers the whole sentence, as it would for a quotation of one sentence. The 40 % outside the quotation is checked against reference 1, and the quoted sentences are not uncited claims. A quotation that is too short or whose citations do not resolve is not checked, so a stop inside it still ends a sentence.

Both sides are normalized before comparison:

- Unicode NFKC, which covers full-width forms and ligatures such as `ﬁ`;
- quote and dash variants unified;
- soft hyphens and zero-width characters dropped.

The comparison then uses a skeleton of lowercase letters, digits and meaning-bearing symbols (`% < > = + ± × ≤ ≥ ≈ $ € £ ¥`). Whitespace, punctuation and line-break hyphenation (`evi-\ndence`) disappear. The skeleton keeps decimal points, minus signs and dashes between digits, so `1.5` and `15`, `−2.1` and `2.1`, and `10–20` and `1020` stay distinct.

A quotation is verbatim when its skeleton occurs exactly and the occurrence neither starts nor ends inside a word or number of the source. "our method reaches 12" is not verbatim in "Our method reaches 12.3 % accuracy", and neither is "reaches 12.3": the percent sign belongs to the number. Whitespace otherwise ends a number, so "66.31" is a whole token of the table cells "68.56 66.31", as it is for number checks. A word split by line-break hyphenation ("inter-⏎annotator") is one word, so a quotation ending at "inter-" cuts it. Chinese and Japanese text has no word boundaries, so only numbers are checked there. A quotation that occurs only by cutting a token is reported as a warning with similarity 0.99, and its `data` has `cuts_token: true`.

An ellipsis (`...`, `…`) or an editorial insertion in brackets (`[…]`, `[sic]`) splits a quotation into segments. Each segment must occur, in order. When a segment does not, it is located approximately. Candidate regions come from character-trigram votes, and the best placement is scored by edit distance as `similarity = 1 − distance / length`. The worst segment decides the result. The comparison is per character, so it works for Chinese and Japanese text.

#### Kept quotations

Some quoted text beside a citation does not quote the cited source. Examples are the project's own slide text, a search query string or a venue name. Each `quote-not-found` finding on such text needs a reviewed decision. Since 0.13.0, a `[[claimcheck.kept_quote]]` entry records that decision with three fields, all required:

- `file`: the document, relative to the project root, as findings show it. A path that is absolute or has `.`, `..` or empty segments is an error.
- `quote`: the quoted text. It matches a checked quotation in `file` when both have the same skeleton (see above), so case, punctuation and spacing do not matter. A quote too short to be checked (fewer than `min_words` words and `min_cjk_chars` CJK characters, or fewer than 4 letters and digits) is an error, since no quotation could match it.
- `reason`: why the text is not a quote of the source. It must not be empty.

A matching `quote-not-found` finding becomes info, whatever its severity and any severity override other than `off`. Its message ends with `; kept by [[claimcheck.kept_quote]]: <reason>`, and `data.kept_reason` holds the reason. The same text in another file, and any other quotation in the same file, is still checked. A quotation whose cited text is missing or changed keeps its `source-text-missing` or `source-text-changed` finding.

An entry whose file the run checked, but that matched no quotation that was not found verbatim, is a `stale-kept-quote` warning. A match whose cited text is missing or changed counts as a match. The text may now be verbatim in a cited source, or no longer quoted beside a citation. The subject is the file; `data` holds `quote` and `reason`; the evidence names the configuration file that was loaded, such as `research.toml`, with locator `[[claimcheck.kept_quote]] entry <n>`. A run that does not check the file reports nothing for the entry. The same file and quote twice is an error.

### Numbers

A number in a sentence that cites a reference with text must occur in that text. Matching uses canonical values:

- `1,234` = `1234`, `.5` = `0.5` and `12.30` = `12.3`;
- `12.3 %` matches `12.3%`;
- `1.2k`, `3M` and `4bn` also match their scaled values. A scale may follow after one space (`26.6 M`, `4.57 bn`) and may be a word: `thousand`, `million`, `billion`, `trillion` (any case) or `mn`. This holds in documents and in source texts, so a source's `8.65 million` matches 8,650,000. A single letter after a space (`k`, `K`, `M`, `B`) is a scale only after a decimal or a currency sign (`$`, `€`, `£`, `¥`), so `Panel 5 B` keeps the plain 5. In a range, a scale after the second number also applies to the first: `$0.38–2.01M` matches 380,000 and 2,010,000, and `1.2–3.4bn` scales both. A range here is two numbers joined by a dash with no space on either side; the first number must have no sign, unit or scale of its own. A currency sign before the range counts for the second number, so `$10–20 M` scales both, while `10–20 M` and `5 - 10M` leave the first number plain. Spelled numbers such as `two thousand` are not read;
- a negative number (`-2.1`, `(−2.1)`) needs a negative number in the source. An unsigned number ("fell by 2.1") matches either sign;
- a dash right after `%`, `‰` or `pp` is a range, not a minus sign: `52%–90%` holds 52% and 90%. In a source text such a number also counts as negative, because extracted table cells can run together (`-18.94%-43.50%`);
- in a source text, thousands groups split by a space or by a comma and a space (`1, 590, 436`, `$1 454`) also match the joined number (1,590,436 and 1,454). The groups keep their own values too, so table cells still match. A run starts with one to three digits and continues with groups of exactly three; a group with a fraction ends it. Only the whole run matches exactly. A scaled match also tries each prefix of two or more groups, because a table row's next cell can continue the run: in `$970 436 241`, a volume of 970,436 followed by 241 swaps, $0.97M matches 970,436. The whole run wins over its prefixes.

A number that matches no source number exactly may match one of the source numbers near the sentence's context words: the same candidates `number-not-in-source` reports as nearby, found within `context_window` characters. The window counts each run of spaces or tabs as one character, so the padding in a `pdftotext -layout` table row does not push its cells out of reach of the row or panel label; a line break still counts as one character. The match is reported as an info `number-not-in-source` finding that names the source value, such as "8.02 is rounded from 8.016 in the source text of reference 1". Its `data.match` holds the `kind`, `reference`, `path`, `line` and source `value`. Three kinds of match count:

- **Rounding** (`kind` `rounded`): the source number has more fractional digits than the document number has as written, and rounding it half-up or half-even to those digits gives the document number. So 8.016 matches 8.02, 1030.90 matches 1,031 and 41.8713 matches 41.9. Truncation does not match: 1,218.66 is not 1,218. A document number with fewer than 2 significant digits never matches by rounding, so 8 does not match 7.6, but 8.0 matches 7.96. `match_rounded = false` in `[claimcheck.numbers]` turns rounding off.
- **Scaled** (`kind` `scaled`): a document number with a scale of 10^k matches a source number whose full value, divided by 10^k, rounds to it as above. So 75.1M matches 75,088,497 ("75.1 is rounded from 75088497 in millions"), $4.3K matches 4,264.33 and $8.7M matches 8.65 million. The full value is the source number with its own scale applied, and space-split thousands groups count as the joined number (`$1 881 277` for 1.88 M) or as a joined prefix of two or more groups (`$1 881 277 251` for 1.88 M). The rounding rules above apply: 75.0M does not match 75,088,497, $4K does not match 4,264.33, and `match_rounded = false` turns this off too.
- **Fraction as a percentage** (`kind` `percent`, or `rounded-percent`): a document percentage (`%` or `pp`, not `‰`) matches a source number between 0 and 1 without a percent sign whose value × 100 equals it, so 8.2% matches 0.082. With rounding on, the value × 100 may also round to it as above, so 15.8% matches 0.1578.

Signs follow the exact rule: a negative document number needs a negative source number. When several candidates match, an exact percentage wins over a scaled match, and a scaled match over a rounding. Among matches of one kind, the candidate near more context words wins, then the one closest in value.

These are not checked:

- years from `year_min` to `year_max`;
- plain integers below `min_integer`;
- numbers glued to letters (`A12`, `F1`, `gpt-4`) or dotted versions (`1.2.3`);
- section, table, figure and equation locators;
- reference numbers, `#N`, URLs, DOIs, arXiv ids, dates and times;
- shortened hex addresses (`0x0d4a…1852`, `0x0d4a...1852`);
- the number in an index name after Nikkei, TOPIX, S&P, Russell, FTSE, STOXX, KOSPI, CSI, Nasdaq, DAX or CAC (`Nikkei 225`, `S&P 500`), any case, with at most one space before it;
- numbers inside code, links or quotations that are checked as quotations.

A date, locator or identifier hides a number only when it covers the whole number. Dates need a plausible year, month and day, so `9999.99` and `55.1-58.4` are checked. Dates, locators and index names never hide a percentage, a scaled number or a number with thousands separators, so "level 95%" and "rows 2,378" are checked. Index names and dates also never hide a decimal. So in "the Nikkei 225 fell 12.4%" the 12.4% is checked.

A sentence whose cited references have no text gets one `source-text-missing` finding listing its numbers.

With `links_are_evidence` set in `[claimcheck.numbers]` (off by default), a number missing from the cited texts is reported at info, not warn, when the same sentence holds a link that is not a citation, such as a link to the project's own measurement of a cited dataset. Such a finding has `"linked": true` in its data. A link in another sentence of the paragraph or table row does not count, so it cannot hide a misquoted number elsewhere. A bare `[N]` is still read as a citation, not as a link.

### Claim kinds and depth

Sentences that cite references are classified by keyword into claim kinds, `result` and `method` by default. A kind listed in `required_depth` has a required depth; other kinds are ignored by this rule, so they neither cause a finding nor make a sentence ambiguous. The severity depends on the evidence for the kind:

- **Error:** a keyword of the kind is within `window_chars` of the citation, in the same clause, with no `;` between them, and the reference's depth is below the kind's requirement.
- **Info:**
  - several kinds are close and only some are short of depth (the kind is ambiguous);
  - only distant keywords, or only numbers, suggest a kind;
  - the reference has no recorded depth (`null`).

### Uncited claims

`uncited-claim` reports a sentence when it cites nothing and makes either of these claims:

- a quantitative claim, meaning it contains a checked number;
- a comparative claim, meaning it contains a `comparative` phrase.

These do not count:

- questions;
- sentences with a modal or conditional word (for numbers);
- numbers inside quotation marks;
- headings, tables and blockquotes;
- sentences in a paper note outside its commentary sections, which cite the note's entry (see [Paper notes](#paper-notes)).

A non-citation link in the same paragraph counts as evidence when `links_are_evidence` is set, and so does a valid evidence label.

### Paper notes

A paper note is a file that exactly one registry entry names as its `source.path`. Each prose sentence in it without a citation cites that entry, as if it ended with `[key]`. Prose means paragraphs and list items outside blockquotes. If the entry is a duplicate, the sentence cites the survivor at the end of its `duplicate_of` chain. A broken or cyclic chain gives no implied citation.

A file that two or more entries name, such as a shared catalogue, is not a paper note. Paths are compared after resolving `.`, `..` and symlinks.

An implied citation is checked less than a written one:

- `uncited-claim` does not fire, and the sentence counts as a `citing-sentence`.
- Numbers are checked against the entry's texts only when one of them can be read. Otherwise they are not checked, and no `source-text-missing` is reported for the sentence.
- Quotations are not checked. Notes quote titles and their own labels for methods, which are not verbatim in the text.
- Claim depth is not checked. Depth errors need a citation marker next to the claim keyword, and an implied citation has none.
- An evidence label is still required where `labels.require` asks for one.

A written citation in a sentence replaces the implied one for that sentence.

A note can also hold its author's own commentary, with numbers from other sources or the author's own arithmetic. `[claimcheck.paper_notes] commentary_sections` lists the headings of such sections. A sentence in one of them gets no implied citation, so the normal rules apply:

- An uncited claim gives `uncited-claim`. A link (with `uncited.links_are_evidence`) or a valid evidence label still counts as evidence.
- A sentence with a written citation is checked against what it cites.
- The sentence counts as an `uncited-sentence` unless it cites.

A heading matches when its visible text equals a listed text, ignoring case, surrounding spaces and a trailing colon. "Related" matches `## Related:` and `#### related`, but not `## Related work`. Headings match at any level. The section runs to the next heading of the same or a higher level, so its subsections are in it. This is how reference-list headings match too. The default lists no headings.

### Evidence labels

A label is the leading `Label:` or `**Label:**` of a paragraph or list item. With a vocabulary set, the rule reports:

- a near-miss label, differing only in case or by an edit distance of 2 or less (error);
- a missing label when `require` asks for one (warn);
- a cited reference shallower than the label's `min_depth` (error, or info when its depth is unknown).

Each of these findings has `paragraph_key` in its `data`: 16 hex digits of a SHA-256 hash. The hash covers the paragraph's first sentence with a letter or digit, without citation markers, and the reference keys that sentence cites. A paragraph without such a sentence hashes only the keys it cites. Sentences are split as claimcheck splits them before it checks inline quotations, so the key does not depend on the registry or the configuration. So the same finding in two paragraphs has distinct data, unless their first sentences have the same text and citations. A paragraph keeps its key when it moves, and when a sentence after its first one changes. Lab counts an introduced finding by its message and data, so this lets it count each paragraph.

### Reference lists

Entries under a reference-list heading are matched to the registry. The key comes from a leading citation such as `[N]` or `[@key]`, or from the number of an ordered-list item (`N.`). The entry text must contain the registry title or match it with at least `title_similarity`. It must also contain the year and the first author's surname, when the registry knows them. An entry with no year at all is a mismatch, a letter suffix counts as the year ("2025a" is 2025), and digits inside identifiers (`arXiv:2012.01234`, `10.1145/2009916`) are not years.

## Output: `rf.finding/1`

```json
{"schema":"rf.finding/1","tool":"claimcheck","rule":"number-not-in-source","severity":"warn",
 "subject":"notes.md:4","message":"72.1% does not occur in the source text of reference 1; nearby numbers: 71.2, 1234",
 "evidence":[{"path":"texts/1.txt","locator":"line 2: 71.2"},{"path":"texts/1.txt","locator":"line 3: 1234"}],
 "data":{"number":"72.1%","references":["1"],"sentence":"Accuracy reaches 72.1% on 1,234 questions."},
 "at":"2026-01-01T00:00:00Z"}
```

`subject` is `path:line`, with the path relative to the project root. Registry-level findings such as `source-text-changed` use the reference key. `evidence` points into source texts. `data` carries rule-specific fields such as `quote`, `similarity`, `best_match`, `sentence`, `kinds`, `near_citation`, `required` and `paragraph_key`.

### Denominator: `rf.measure/1`

```json
{"schema":"rf.measure/1","tool":"claimcheck","unit":"citing-sentence","count":412,"keys":["0a3f5c1e9b2d7480","…"]}
{"schema":"rf.measure/1","tool":"claimcheck","unit":"uncited-sentence","count":958,"keys":["03b7e2a9c4d16f58","…"]}
```

`lab check` stores these records, so lab can judge a check-run lesson by findings per new sentence ([interfaces](https://github.com/syz51/research-factory/blob/main/docs/interfaces.md), "Check-run denominators"). `uncited-claim` fires only on sentences that do not cite, so `citing-sentence` never grows when it fires; `uncited-sentence` does. Lab 0.9.0 and earlier read a check that prints several units as measuring no interval, so they need a lab that picks the unit per rule.

There is one line per unit, always in this order:

1. **`citing-sentence`**: the sentences that cite at least one reference, resolved or not, including the uncited prose sentences of a [paper note](#paper-notes). A non-prose block, such as a table row, counts as one sentence. Each sentence of a blockquote that is checked as a cited quotation also counts. A sentence with a checked inline quotation counts once, however many sentences it quotes (see [Quotations](#quotations)).
2. **`uncited-sentence`**: the sentences without a citation in prose, meaning paragraphs and list items, outside blockquotes and outside paper notes. These are the sentences where `uncited-claim` can fire.

A checked sentence is in at most one unit. These are in neither: table rows that cite nothing; sentences without a citation in a blockquote that is not checked as a cited quotation, including its list items; headings; reference-list entries; and units with no text besides citations. So a commit that adds an uncited table adds no units, just as `uncited-claim` cannot fire in it.

- `count` is the number of the unit's sentences over all checked files.
- `keys` are the sorted, distinct first 16 lowercase hex digits of the SHA-256 of each sentence's text. The text is normalized as a finding's `sentence`, with whitespace collapsed and trimmed, but in NFC and not truncated. Citation markers are left out together with the whitespace before them, so moving a citation within a sentence keeps its key: `helps [1].` and `helps.[1]` both read `helps.`. The cited references are not part of the key, so citing another reference adds no new unit. The same sentence in two places has one key, so a unit has at most `count` keys.
- Both units make keys the same way. A prose sentence that gains a citation therefore keeps its key but moves from `uncited-sentence` to `citing-sentence`. The same text cited in one place and uncited in another has its key in both units.

## Limits

- **Strings, not meaning.** A quotation that occurs verbatim is supported only in the sense that the words are in the source. `claimcheck` does not judge whether the source supports the claim made with them. Numbers are matched by value, so a number that occurs anywhere in the source counts as found, even when it measures something else. A rounding, scaled or percentage match is weaker still: any nearby source number that rounds to the document number counts, so it can be a coincidence. Such matches are info findings so that they can be reviewed.
- **Keyword heuristics.** Claim kinds and uncited claims come from keyword and number heuristics. Plans, design parameters, imperatives ("Keep 500 items in reserve") and negated statements can still be reported as uncited claims. Lower `uncited-claim` to `info` or turn it off where that is noise.
- **Registry depth.** `depth-insufficient` is only as good as the registry's `depth`.
- **Source texts.** Text extracted from PDFs may lose tables, formulas and reading order. A quotation from a table or a figure may then be reported as not found.
- **Approximate search.** It aligns only the six best candidate regions of a long source, so the reported closest match may not be the global best. Exact matches are always found.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests use synthetic fixtures only.

## Licence

Licensed under either the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT licence](LICENSE-MIT), at your option.
