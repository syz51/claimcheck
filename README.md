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
claimcheck rules
```

`PATHS` are Markdown files or directories. Directories are searched recursively for `.md` and `.markdown` files, and hidden entries are skipped. When no path is given, the `paths` from the configuration are used.

- **Text output** (the default) prints one finding per line as `file:line: severity[rule] message`. Indented lines give the source evidence, the quotation and the closest match. Excerpts are about 200 characters.
- **`--format jsonl`** prints one `rf.finding/1` record per line (schema below).
- **`--summary`** adds a table of counts by rule and severity, plus totals: files, citations resolved, quotations verbatim and numbers found. It goes to stdout in text mode and to stderr with `--format jsonl`.
- **`--min-severity`** hides lower-severity findings. The exit code still counts every error.
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
| `duplicate_of` | The key this entry duplicates. |

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
paths = ["docs"]          # checked when no path is given on the command line
exclude = []              # paths skipped while walking directories

[claimcheck.citations]
# Regexes with named groups: `key`, or `keys` (a list such as "3, 5-7" or
# "@a; @b"), optional `label` (the visible number, compared with `key`) and
# optional `cite` (the span that counts as the citation; default: the whole match).
patterns = [
  '\[(?P<label>\d+)\]\([^)\s]*#reference-(?P<key>\d+)\)',              # [3](references.md#reference-3)
  '\[(?P<keys>[1-9]\d*(?:\s*[,\x{2013}-]\s*[1-9]\d*)*)(?:\s*;[^\[\]\n]*)?\]', # [3], [3, 5-7], [3; p. 4]
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

[claimcheck.claims]
number_kind = "result" # kind suggested by a checked number (info only, never an error)
window_chars = 80      # a keyword must be this close to the citation, in the same clause

[claimcheck.claims.kinds]
result = ["outperforms", "achieves", "improves", "accuracy", "state of the art",
          "better than", "shows that", "reduces", "increases", "significantly", "..."]
method = ["proposes", "introduces", "algorithm", "pipeline", "trained", "fine-tuned",
          "implements", "uses", "applies", "loss function", "ablation", "..."]

[claimcheck.claims.required_depth]
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

[claimcheck.severity]
# rule-id = "error" | "warn" | "info" | "off"
```

`claimcheck rules` and the source (`src/config.rs`) give the full keyword lists. Keyword phrases match whole words, case-insensitively. A keyword within three words after a modal or conditional word does not count. Those words are must, should, could, would, might, may, can, cannot, whether, if and unless. A keyword hyphenated to another word, as in "outcome-trained agents", does not count either.

A severity override changes the findings a rule reports at its default severity. `depth-insufficient` and `evidence-label` also report at other severities, and those findings keep them: an info stays info. `off` drops every finding of the rule.

## Rules

| Rule id | Default | What it reports |
| --- | --- | --- |
| `citation-unresolved` | error | A citation key is not in the registry. |
| `citation-duplicate` | warn | A citation uses a key whose registry entry duplicates another key. The message names the surviving key, or says that the `duplicate_of` chain is broken or cycles. |
| `citation-label-mismatch` | error | A citation's visible number differs from the key in its link, as in `[3](references.md#reference-4)`. |
| `quote-not-found` | error | Quoted text attached to a citation does not occur in the cited source text. Below `error_below` similarity it is an error; between that and verbatim, a warning. Both report the closest match with its line and similarity. |
| `source-text-missing` | warn | A quotation or number needs a cited source's text, but the registry lists none or the file cannot be read. The check is undecided, never "supported". |
| `source-text-changed` | warn | A source text file's SHA-256 differs from the registry's `sha256`. The text is still used. |
| `number-not-in-source` | warn | A number in a citing sentence occurs in none of the cited source texts. Up to `nearest` numbers near the sentence's words in the source are reported. |
| `depth-insufficient` | error | A sentence makes a claim of a kind that needs deeper inspection than the cited reference's `depth`. It is info when the kind is uncertain (see below). |
| `uncited-claim` | warn | A sentence makes a quantitative or comparative claim with no citation, link or evidence label. |
| `evidence-label` | warn | A label is outside the vocabulary but close to it (error), a required label is missing (warn), or a label's `min_depth` is not met (error). |
| `reference-list-mismatch` | error | An entry in a document's own reference list disagrees with the registry on title, year or first author's surname, or names an unknown key. |

### Citations

Citation patterns run on the raw Markdown, so link syntax such as `[3](references.md#reference-3)` is seen as written. Several things are not citations:

- citations inside code spans, code blocks, HTML or front matter;
- an inline link whose text happens to be a number, as in `[3](https://example.org)`;
- bracketed intervals that start at zero or below, such as `[0, 1]` or `[-3, 0]`. `[1, 2]` reads as two citations.

A citation placed right after a full stop, as in `… pipeline. [2]`, belongs to the sentence it ends.

### Quotations

These are checked:

- text in straight `"…"` or curly `“…”` double quotes;
- curly single quotes `‘…’` (apostrophes are not quotes);
- guillemets `«…»`;
- Japanese `「…」` and `『…』`;
- blockquotes.

A quotation is checked when it has a citation in the same sentence or inside it, and has at least `min_words` words or `min_cjk_chars` CJK characters. Shorter ones are terms or scare quotes. A blockquote is checked when it contains a citation, or when the paragraph just before it cites and ends with a colon. A trailing attribution line such as `— Author [3]` is not part of the quoted text.

Both sides are normalized before comparison:

- Unicode NFKC, which covers full-width forms and ligatures such as `ﬁ`;
- quote and dash variants unified;
- soft hyphens and zero-width characters dropped.

The comparison then uses a skeleton of lowercase letters, digits and meaning-bearing symbols (`% < > = + ± × ≤ ≥ ≈ $ € £ ¥`). Whitespace, punctuation and line-break hyphenation (`evi-\ndence`) disappear, but the skeleton keeps decimal points, so `1.5` and `15` stay distinct. A quotation is verbatim when its skeleton occurs exactly.

An ellipsis (`...`, `…`) or an editorial insertion in brackets (`[…]`, `[sic]`) splits a quotation into segments. Each segment must occur, in order. When a segment does not, it is located approximately. Candidate regions come from character-trigram votes, and the best placement is scored by edit distance as `similarity = 1 − distance / length`. The worst segment decides the result. The comparison is per character, so it works for Chinese and Japanese text.

### Numbers

A number in a sentence that cites a reference with text must occur in that text. Matching uses canonical values:

- `1,234` = `1234`, `.5` = `0.5` and `12.30` = `12.3`;
- `12.3 %` matches `12.3%`;
- `1.2k`, `3M` and `4bn` also match their scaled values.

These are not checked:

- years from `year_min` to `year_max`;
- plain integers below `min_integer`;
- numbers glued to letters (`A12`, `F1`, `gpt-4`) or dotted versions (`1.2.3`);
- section, table, figure and equation locators;
- reference numbers, `#N`, URLs, DOIs, arXiv ids, dates and times;
- numbers inside code, links or quotations that are checked as quotations.

A sentence whose cited references have no text gets one `source-text-missing` finding listing its numbers.

### Claim kinds and depth

Sentences that cite references are classified by keyword into claim kinds, `result` and `method` by default. Each kind has a required depth. The severity depends on the evidence for the kind:

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
- headings, tables and blockquotes.

A non-citation link in the same paragraph counts as evidence when `links_are_evidence` is set, and so does a valid evidence label.

### Evidence labels

A label is the leading `Label:` or `**Label:**` of a paragraph or list item. With a vocabulary set, the rule reports:

- a near-miss label, differing only in case or by an edit distance of 2 or less (error);
- a missing label when `require` asks for one (warn);
- a cited reference shallower than the label's `min_depth` (error, or info when its depth is unknown).

### Reference lists

Entries under a reference-list heading are matched to the registry. The key comes from a leading citation such as `[N]` or `[@key]`, or from the number of an ordered-list item (`N.`). The entry text must contain the registry title or match it with at least `title_similarity`. It must also contain the year and the first author's surname, when the registry knows them.

## Output: `rf.finding/1`

```json
{"schema":"rf.finding/1","tool":"claimcheck","rule":"number-not-in-source","severity":"warn",
 "subject":"notes.md:4","message":"72.1% does not occur in the source text of reference 1; nearby numbers: 71.2, 1234",
 "evidence":[{"path":"texts/1.txt","locator":"line 2: 71.2"},{"path":"texts/1.txt","locator":"line 3: 1234"}],
 "data":{"number":"72.1%","references":["1"],"sentence":"Accuracy reaches 72.1% on 1,234 questions."},
 "at":"2026-01-01T00:00:00Z"}
```

`subject` is `path:line`, with the path relative to the project root. Registry-level findings such as `source-text-changed` use the reference key. `evidence` points into source texts. `data` carries rule-specific fields such as `quote`, `similarity`, `best_match`, `sentence`, `kinds`, `near_citation` and `required`.

## Limits

- **Strings, not meaning.** A quotation that occurs verbatim is supported only in the sense that the words are in the source. `claimcheck` does not judge whether the source supports the claim made with them. Numbers are matched by value: the sign is ignored, and a number that occurs anywhere in the source counts as found.
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
