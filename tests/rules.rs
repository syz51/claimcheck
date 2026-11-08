//! End-to-end rule tests on synthetic projects.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use claimcheck::check::{Project, check_document};
use claimcheck::config::Config;
use claimcheck::finding::{Finding, Rule, Severity};
use claimcheck::registry::Registry;
use serde_json::json;
use sha2::{Digest, Sha256};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_project(files: &[(&str, &str)]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("claimcheck-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, content) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    root
}

fn reference(key: &str, depth: Option<&str>, texts: &[&str]) -> serde_json::Value {
    json!({
        "schema": "rf.reference/1",
        "key": key,
        "type": "paper",
        "title": format!("Synthetic Paper {key}"),
        "authors": ["Ada Example", "Bo Sample"],
        "year": 2025,
        "depth": depth,
        "texts": texts.iter().map(|t| json!({"path": t, "sha256": null, "origin": null})).collect::<Vec<_>>(),
        "duplicate_of": null,
    })
}

struct Fixture {
    project: Project,
}

impl Fixture {
    fn new(refs: &[serde_json::Value], texts: &[(&str, &str)], toml: &str) -> Fixture {
        let root = temp_project(texts);
        let lines: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
        let registry = Registry::parse(&lines.join("\n"), "references.jsonl").unwrap();
        let cfg = Config::from_toml(&root, toml).unwrap();
        Fixture {
            project: Project::new(cfg, registry),
        }
    }

    fn check(&self, md: &str) -> Vec<Finding> {
        check_document(&self.project, "doc.md", md.to_string(), 0).0
    }
}

fn rules(findings: &[Finding]) -> Vec<(Rule, Severity)> {
    findings.iter().map(|f| (f.rule, f.severity)).collect()
}

const SOURCE: &str = "Abstract\nWe study ﬁnancial evidence retriev-\nal for annual reports. Our method reaches 12.3 % accuracy\non 1234 questions and improves recall by 0.5 points over BM25.\nTable 2: nDCG@10 is 71.2 on FinQA.\n";

fn basic() -> Fixture {
    Fixture::new(
        &[
            reference("1", Some("full"), &["texts/1.txt"]),
            reference("2", Some("abstract"), &["texts/2.txt"]),
            reference("3", None, &[]),
            reference("4", Some("method"), &["texts/4.txt"]),
        ],
        &[
            ("texts/1.txt", SOURCE),
            ("texts/2.txt", "We propose a retriever.\n"),
            (
                "texts/4.txt",
                "本研究では金融文書の検索精度を改善する手法を提案する。\n",
            ),
        ],
        "",
    )
}

#[test]
fn unresolved_duplicate_and_label_mismatch() {
    let mut dup = reference("5", Some("full"), &[]);
    dup["duplicate_of"] = json!("1");
    let fx = Fixture::new(&[reference("1", Some("full"), &[]), dup], &[], "");
    let f =
        fx.check("See [9](references.md#reference-9), [5] and [1](references.md#reference-5).\n");
    assert_eq!(
        rules(&f),
        [
            (Rule::CitationUnresolved, Severity::Error),
            (Rule::CitationDuplicate, Severity::Warn),
            (Rule::CitationDuplicate, Severity::Warn),
            (Rule::CitationLabelMismatch, Severity::Error),
        ]
    );
    assert!(f[1].message.contains("cite 1 instead"), "{}", f[1].message);
    assert_eq!(f[0].subject, "doc.md:1");
}

#[test]
fn exact_quote_survives_pdf_artifacts() {
    let fx = basic();
    let f = fx.check("The authors study “financial evidence retrieval for annual reports” [1].\n");
    assert!(f.is_empty(), "{f:#?}");
}

#[test]
fn near_quote_warns_and_fabricated_quote_errors() {
    let fx = basic();
    let f = fx.check("They study “financial evidence retrieval for quarterly reports” [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)]);
    assert!(
        f[0].evidence[0].locator.starts_with("best match line 2"),
        "{:?}",
        f[0].evidence
    );
    assert_eq!(f[0].evidence[0].path, "texts/1.txt");
    let f = fx.check("They claim \"graph neural networks solve every retrieval task\" [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Error)]);
}

#[test]
fn short_scare_quotes_are_not_checked() {
    let fx = basic();
    assert!(fx.check("A “safe” stop [1].\n").is_empty());
}

#[test]
fn cjk_quotes() {
    let fx = basic();
    assert!(
        fx.check("著者は「金融文書の検索精度を改善する」と述べる［4］[4]。\n")
            .is_empty()
    );
    let f = fx.check("著者は「金融文書の検索速度を大幅に向上させる」と述べる[4]。\n");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].rule, Rule::QuoteNotFound);
}

#[test]
fn ellipsis_quotes_match_in_order() {
    let fx = basic();
    assert!(
        fx.check("“We study financial evidence … improves recall by 0.5 points” [1].\n")
            .is_empty()
    );
    let f = fx.check("“improves recall by 0.5 points … We study financial evidence” [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)]);
}

#[test]
fn missing_text_is_never_supported() {
    let fx = basic();
    // The quotation is real text of reference 1, but reference 3 has no text.
    let f = fx.check("“financial evidence retrieval for annual reports” [3].\n");
    assert_eq!(rules(&f), [(Rule::SourceTextMissing, Severity::Warn)]);
    // A number cited only to reference 3 cannot be verified either.
    let f = fx.check("Accuracy is 12.3% [3].\n");
    assert!(
        rules(&f).contains(&(Rule::SourceTextMissing, Severity::Warn)),
        "{f:#?}"
    );
    assert!(!rules(&f).contains(&(Rule::NumberNotInSource, Severity::Warn)));
}

fn duplicate(key: &str, of: &str, texts: &[&str]) -> serde_json::Value {
    let mut r = reference(key, Some("full"), texts);
    r["duplicate_of"] = json!(of);
    r
}

#[test]
fn duplicate_without_texts_is_checked_against_its_survivor() {
    let fx = Fixture::new(
        &[
            reference("1", Some("full"), &["texts/1.txt"]),
            duplicate("5", "6", &[]),
            duplicate("6", "1", &[]),
        ],
        &[("texts/1.txt", SOURCE)],
        "",
    );
    // Supported through the chain 5 → 6 → 1; only the duplicate citation is reported.
    let f = fx.check("“financial evidence retrieval for annual reports” and 12.3% [5].\n");
    assert_eq!(
        rules(&f),
        [(Rule::CitationDuplicate, Severity::Warn)],
        "{f:#?}"
    );
    let f = fx.check("“financial evidence retrieval for quarterly reports” [5].\n");
    assert_eq!(
        rules(&f),
        [
            (Rule::CitationDuplicate, Severity::Warn),
            (Rule::QuoteNotFound, Severity::Warn)
        ]
    );
    assert_eq!(f[1].data["survivors"], json!({"5": "1"}));
    assert_eq!(f[1].evidence[0].path, "texts/1.txt");
    let f = fx.check("nDCG@10 is 71.5 on FinQA [5].\n");
    let n = f
        .iter()
        .find(|f| f.rule == Rule::NumberNotInSource)
        .expect("number finding");
    assert_eq!(n.data["survivors"], json!({"5": "1"}));
    assert_eq!(n.data["nearest"][0]["value"], "71.2");
}

#[test]
fn duplicate_with_own_texts_uses_only_them() {
    let fx = Fixture::new(
        &[
            reference("1", Some("full"), &["texts/1.txt"]),
            duplicate("5", "1", &["texts/5.txt"]),
        ],
        &[
            ("texts/1.txt", SOURCE),
            ("texts/5.txt", "Unrelated text.\n"),
        ],
        "",
    );
    let f = fx.check("“financial evidence retrieval for annual reports” [5].\n");
    assert_eq!(
        rules(&f),
        [
            (Rule::CitationDuplicate, Severity::Warn),
            (Rule::QuoteNotFound, Severity::Error)
        ]
    );
    assert!(f[1].data.get("survivors").is_none(), "{:#?}", f[1].data);
}

#[test]
fn broken_or_cyclic_duplicate_chain_is_missing_text() {
    let fx = Fixture::new(
        &[
            reference("1", Some("full"), &["texts/1.txt"]),
            duplicate("5", "9", &[]),
            duplicate("6", "7", &[]),
            duplicate("7", "6", &[]),
        ],
        &[("texts/1.txt", SOURCE)],
        "",
    );
    for key in ["5", "6"] {
        let f = fx.check(&format!(
            "“financial evidence retrieval for annual reports” [{key}].\n"
        ));
        assert_eq!(
            rules(&f),
            [
                (Rule::CitationDuplicate, Severity::Warn),
                (Rule::SourceTextMissing, Severity::Warn)
            ],
            "{key}: {f:#?}"
        );
        assert!(f[1].data.get("survivors").is_none(), "{:#?}", f[1].data);
        let f = fx.check(&format!("Accuracy is 12.3% [{key}].\n"));
        assert!(
            rules(&f).contains(&(Rule::SourceTextMissing, Severity::Warn)),
            "{key}: {f:#?}"
        );
    }
}

#[test]
fn unreadable_text_is_missing_not_supported() {
    let fx = Fixture::new(
        &[reference("1", Some("full"), &["texts/absent.txt"])],
        &[],
        "",
    );
    let f = fx.check("“financial evidence retrieval for annual reports” [1].\n");
    assert_eq!(rules(&f), [(Rule::SourceTextMissing, Severity::Warn)]);
    let g = fx.project.text_findings(1);
    assert_eq!(rules(&g), [(Rule::SourceTextMissing, Severity::Warn)]);
    assert_eq!(g[0].subject, "1");
}

#[test]
fn blockquote_attached_to_citation() {
    let fx = basic();
    let md =
        "As the paper puts it [1]:\n\n> Our method reaches 12.3 % accuracy on 1234 questions\n";
    assert!(fx.check(md).is_empty(), "{:#?}", fx.check(md));
    let md = "> Our method reaches 99 % accuracy on every question\n>\n> — Example [1]\n";
    let f = fx.check(md);
    assert_eq!(
        f.iter().filter(|f| f.rule == Rule::QuoteNotFound).count(),
        1,
        "{f:#?}"
    );
}

#[test]
fn numbers_normalize() {
    let fx = basic();
    for s in [
        "It reaches 12.3% accuracy on 1,234 questions [1].",
        "Recall improves by .5 points [1].",
        "Accuracy is １２．３％ [1].",
        "nDCG@10 is 71.20 on FinQA [1].",
    ] {
        let f = fx.check(s);
        assert!(
            !f.iter().any(|f| f.rule == Rule::NumberNotInSource),
            "{s}: {f:#?}"
        );
    }
}

#[test]
fn missing_number_reports_nearby_values() {
    let fx = basic();
    let f = fx.check("nDCG@10 is 71.5 on FinQA [1].\n");
    let n: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == Rule::NumberNotInSource)
        .collect();
    assert_eq!(n.len(), 1, "{f:#?}");
    assert!(n[0].message.contains("71.5"), "{}", n[0].message);
    assert_eq!(n[0].data["nearest"][0]["value"], "71.2");
    assert_eq!(n[0].data["nearest"][0]["line"], 5);
}

#[test]
fn years_sections_and_identifiers_are_not_numbers() {
    let fx = basic();
    let md = "In 2024, Section 4.3 and Table 12 of the 2604.15882v1 preprint (H133, F1, 2026-10-10, 12 October) discuss it [1].\n";
    let f = fx.check(md);
    assert!(
        !f.iter().any(|f| f.rule == Rule::NumberNotInSource),
        "{f:#?}"
    );
}

#[test]
fn depth_rules() {
    let fx = basic();
    // A method claim citing an abstract-depth reference.
    let f = fx.check("The retriever [2] uses a two-stage pipeline.\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Error)]);
    // Unknown depth: cannot decide.
    let f = fx.check("The retriever [3] uses a two-stage pipeline.\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Info)]);
    // Deep enough.
    assert!(
        fx.check("The retriever [1] uses a two-stage pipeline.\n")
            .is_empty()
    );
    // No claim keyword.
    assert!(fx.check("See also [2].\n").is_empty());
}

#[test]
fn depth_keyword_must_be_near_the_citation() {
    let fx = basic();
    // "uses" describes our own setup, far from the citation and in another clause.
    let f = fx.check("Our route uses a fixed model; the vendor documentation describes the request format [2].\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Info)]);
    // A citation after the full stop belongs to the sentence it ends.
    let f = fx.check("The retriever uses a two-stage pipeline. [2] Next we discuss data.\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Error)]);
    assert_eq!(
        f[0].data["sentence"],
        "The retriever uses a two-stage pipeline."
    );
}

#[test]
fn numbers_alone_are_weak_claim_evidence() {
    let fx = basic();
    let f = fx.check("A 0.9 threshold corresponds to 90% [2].\n");
    let depth: Vec<_> = f
        .iter()
        .filter(|f| f.rule == Rule::DepthInsufficient)
        .collect();
    assert_eq!(depth.len(), 1, "{f:#?}");
    assert_eq!(depth[0].severity, Severity::Info);
    // A result keyword next to the citation still makes an error.
    let f = fx.check("It achieves 90% accuracy [2].\n");
    assert!(
        f.iter()
            .any(|f| f.rule == Rule::DepthInsufficient && f.severity == Severity::Error),
        "{f:#?}"
    );
}

#[test]
fn ambiguous_claim_kind_is_info() {
    let toml = "[claimcheck.claims]\nnumber_kind = \"result\"\n[claimcheck.claims.kinds]\nresult = [\"outperforms\"]\nbackground = [\"is related to\"]\n[claimcheck.claims.required_depth]\nresult = \"method\"\nbackground = \"abstract\"\n";
    let fx = Fixture::new(&[reference("2", Some("abstract"), &[])], &[], toml);
    let f = fx.check("It is related to and outperforms BM25 [2].\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Info)]);
    let f = fx.check("It outperforms BM25 [2].\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Error)]);
}

#[test]
fn uncited_claims() {
    let fx = basic();
    let f = fx.check("Our system outperforms every baseline.\n");
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn)]);
    let f = fx.check("Accuracy rose to 87.5% in the pilot.\n");
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn)]);
    assert!(
        fx.check("Accuracy rose to 87.5% in the [pilot](pilot.md).\n")
            .is_empty()
    );
    assert!(fx.check("We met 12 people in 2024.\n").is_empty());
    // Questions and hypothetical or normative sentences assert nothing.
    assert!(
        fx.check("Does it outperform BM25 on 500 queries?\n")
            .is_empty()
    );
    assert!(fx.check("The prompt must fit 4,096 tokens.\n").is_empty());
    // "May" the month is not a modal.
    let f = fx.check("May 2025 accuracy rose to 87.5% in the pilot.\n");
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn)]);
    // Numbers in quoted example text are not the sentence's own claim.
    assert!(
        fx.check("For example, a filing says \"profit was 1,250 yen\".\n")
            .is_empty()
    );
}

#[test]
fn evidence_labels() {
    let toml = "[claimcheck.labels]\nvocabulary = [\"Verified\", \"Author-reported\", \"Hypothesis\"]\nrequire = \"claims\"\n[claimcheck.labels.min_depth]\nVerified = \"full\"\n";
    let fx = Fixture::new(
        &[
            reference("1", Some("full"), &[]),
            reference("2", Some("abstract"), &[]),
        ],
        &[],
        toml,
    );
    assert!(fx.check("**Verified:** see [1].\n").is_empty());
    let f = fx.check("**Verfied:** see [1].\n");
    assert_eq!(rules(&f), [(Rule::EvidenceLabel, Severity::Error)]);
    assert!(f[0].message.contains("did you mean `Verified`"));
    let f = fx.check("See [1].\n");
    assert_eq!(rules(&f), [(Rule::EvidenceLabel, Severity::Warn)]);
    let f = fx.check("Verified: see [2].\n");
    assert_eq!(rules(&f), [(Rule::EvidenceLabel, Severity::Error)]);
    assert!(fx.check("Plain prose without claims.\n").is_empty());
    // A labelled paragraph is not an uncited claim.
    assert!(
        fx.check("Hypothesis: accuracy could reach 90.5%.\n")
            .is_empty()
    );
}

#[test]
fn reference_list_entries() {
    let mut r = reference("7", Some("full"), &[]);
    r["title"] = json!("Retrieval for Annual Reports");
    r["authors"] = json!(["Example, Ada", "Sample, Bo"]);
    let fx = Fixture::new(&[r], &[], "");
    let good = "Text [7].\n\n## References\n\n[7] Example, A. and Sample, B. Retrieval for annual reports. 2025.\n";
    assert!(fx.check(good).is_empty(), "{:#?}", fx.check(good));
    let bad = "Text [7].\n\n## References\n\n[7] Other, C. Retrieval for annual reports. 2019.\n";
    let f = fx.check(bad);
    assert_eq!(rules(&f), [(Rule::ReferenceListMismatch, Severity::Error)]);
    assert!(
        f[0].message.contains("year 2019") && f[0].message.contains("Example"),
        "{}",
        f[0].message
    );
    // A letter suffix is part of the year; a missing year is a mismatch.
    let suffixed = "## References\n\n[7] Example, A. Retrieval for annual reports. 2025a.\n";
    assert!(fx.check(suffixed).is_empty(), "{:#?}", fx.check(suffixed));
    let suffixed = "## References\n\n[7] Example, A. Retrieval for annual reports. 2019b.\n";
    let f = fx.check(suffixed);
    assert!(f[0].message.contains("year 2019"), "{}", f[0].message);
    let no_year = "## References\n\n[7] Example, A. Retrieval for annual reports.\n";
    let f = fx.check(no_year);
    assert_eq!(rules(&f), [(Rule::ReferenceListMismatch, Severity::Error)]);
    assert!(
        f[0].message.contains("year 2025 is missing"),
        "{}",
        f[0].message
    );
    // Digits inside an identifier are not the year.
    let id_only = "## References\n\n[7] Example, A. Retrieval for annual reports. https://doi.org/10.1000/2025.5\n";
    let f = fx.check(id_only);
    assert!(
        f[0].message.contains("year 2025 is missing"),
        "{}",
        f[0].message
    );
    let wrong_title = "## Bibliography\n\n1. Example, A. Graph networks for chemistry. 2025.\n";
    let f = fx.check(wrong_title);
    assert_eq!(rules(&f), [(Rule::ReferenceListMismatch, Severity::Error)]);
    assert!(
        f[0].message.contains("entry 1 has no registry record"),
        "{}",
        f[0].message
    );
}

#[test]
fn hash_mismatch_is_reported_once() {
    let mut r = reference("1", Some("full"), &["texts/1.txt"]);
    r["texts"][0]["sha256"] = json!("00");
    let fx = Fixture::new(&[r], &[("texts/1.txt", SOURCE)], "");
    let f = fx.check("“financial evidence retrieval for annual reports” [1].\n");
    assert!(f.is_empty());
    assert_eq!(
        rules(&fx.project.text_findings(1)),
        [(Rule::SourceTextChanged, Severity::Warn)]
    );
}

#[test]
fn severity_overrides() {
    let fx = Fixture::new(
        &[reference("1", Some("full"), &[])],
        &[],
        "[claimcheck.severity]\nuncited-claim = \"error\"\ncitation-unresolved = \"off\"\n",
    );
    let f = fx.check("It outperforms everything. See [2].\n");
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Error)]);
}

#[test]
fn cli_exit_codes_and_jsonl() {
    let root = temp_project(&[
        (
            "research.toml",
            "[project]\nname = \"t\"\n\n[claimcheck]\npaths = [\"docs\"]\n",
        ),
        ("texts/1.txt", SOURCE),
        (
            "docs/ok.md",
            "“financial evidence retrieval for annual reports” [1].\n",
        ),
        ("docs/sub/bad.md", "Cites [8].\n"),
    ]);
    let line = reference("1", Some("full"), &["texts/1.txt"]).to_string();
    std::fs::write(root.join("references.jsonl"), format!("{line}\n")).unwrap();
    let bin = env!("CARGO_BIN_EXE_claimcheck");
    let out = Command::new(bin)
        .args(["check", "--format", "jsonl"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["schema"], "rf.finding/1");
    assert_eq!(rows[0]["tool"], "claimcheck");
    assert_eq!(rows[0]["rule"], "citation-unresolved");
    assert_eq!(rows[0]["subject"], "docs/sub/bad.md:1");

    let out = Command::new(bin)
        .args(["check", "docs/ok.md", "--summary"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("quotations checked 1 (1 verbatim)"),
        "{stdout}"
    );

    std::fs::write(
        root.join("references.jsonl"),
        line.replace("rf.reference/1", "rf.reference/9"),
    )
    .unwrap();
    let out = Command::new(bin)
        .args(["check"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("references.jsonl:1: unsupported schema")
    );
}

fn inline_quote_fixture() -> Fixture {
    Fixture::new(
        &[reference("1", Some("full"), &["texts/1.txt"])],
        &[(
            "texts/1.txt",
            "Our retriever outperforms BM25 on every split. Evidence is scattered across many sections.\n",
        )],
        "",
    )
}

const TWO_SENTENCE_QUOTE: &str = "The authors write \"Our retriever outperforms BM25 on every split. Evidence is scattered across many sections.\"";

#[test]
fn cited_inline_quotation_sentences_are_not_uncited() {
    let fx = inline_quote_fixture();
    // The quotation is checked against [1] as a whole, so its first
    // sentence, before the marker, is not an uncited claim.
    let f = fx.check(&format!("{TWO_SENTENCE_QUOTE} [1]\n"));
    assert!(f.is_empty(), "{f:#?}");
    // Without a citation, or with only an unresolved one, the quotation is
    // not checked, so its comparative claim is still uncited.
    let f = fx.check(&format!("{TWO_SENTENCE_QUOTE}\n"));
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn)]);
    let f = fx.check(&format!("{TWO_SENTENCE_QUOTE} [9]\n"));
    assert_eq!(
        rules(&f),
        [
            (Rule::CitationUnresolved, Severity::Error),
            (Rule::UncitedClaim, Severity::Warn),
        ]
    );
    // Too short to be checked as a quotation: unchanged.
    let f = fx.check("It \"outperforms. Yes.\" [1]\n");
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn)]);
}

#[test]
fn claim_outside_a_cited_inline_quotation_is_checked_against_its_citation() {
    let fx = inline_quote_fixture();
    // "40 %" is in the sentence that quotes, not in the quotation. As with a
    // quotation of one sentence, the sentence cites [1], so the number is
    // checked against [1], whose text lacks it.
    let one = "Evidence is scattered across many sections.";
    let two = "Our retriever outperforms BM25 on every split. Evidence is scattered across many sections.";
    for quote in [one, two] {
        let f = fx.check(&format!(
            "Revenue rose 40 % and the authors write \"{quote}\" [1]\n"
        ));
        assert_eq!(
            rules(&f),
            [(Rule::NumberNotInSource, Severity::Warn)],
            "{quote}: {f:#?}"
        );
        assert_eq!(f[0].subject, "doc.md:1");
    }
    // The same after the quotation, with the citation before it.
    let f = fx.check(&format!(
        "Smith [1] writes \"{two}\" and revenue rose 40 % in May.\n"
    ));
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Warn)],
        "{f:#?}"
    );
}

#[test]
fn measure_counts_a_sentence_quoting_several_as_one_citing_sentence() {
    let fx = inline_quote_fixture();
    let measure = |md: &str| {
        let st = check_document(&fx.project, "doc.md", md.to_string(), 0).1;
        (st.citing.count, st.uncited.count)
    };
    // The sentence with the cited quotation is one citing sentence; the next
    // sentence does not cite.
    assert_eq!(
        measure(&format!("{TWO_SENTENCE_QUOTE} [1] Then we stop.\n")),
        (1, 1)
    );
    // Not checked as a quotation: split at the stop inside it, as before.
    assert_eq!(measure(&format!("{TWO_SENTENCE_QUOTE}\n")), (0, 2));
    assert_eq!(measure(&format!("{TWO_SENTENCE_QUOTE} [9]\n")), (1, 1));
    assert_eq!(measure("It \"outperforms. Yes.\" [1]\n"), (1, 1));
}

#[test]
fn measure_counts_citing_and_uncited_sentences() {
    let doc_a = "# Heading [1]\n\n\
        Retrieval helps [1]. No citation here.\nIt reaches 12.3 % accuracy\n  [1].\n\n\
        | Model | Score |\n| --- | --- |\n| A | 71.2 [1] |\n| B | 64.0 |\n\n\
        As the paper notes [1]:\n\n\
        > We study financial evidence retrieval for annual reports and more.\n\n\
        > A plain aside.\n\n\
        [1]\n\n\
        ## References\n\n1. Ada Example. Synthetic Paper 1. 2025.\n";
    let root = temp_project(&[
        (
            "research.toml",
            "[project]\nname = \"t\"\n\n[claimcheck]\npaths = [\"docs\"]\n",
        ),
        ("docs/a.md", doc_a),
        (
            "docs/b.md",
            "Retrieval helps.[1] See [8] too. No citation here.\n\nRetrieval helps.\n",
        ),
    ]);
    let line = reference("1", Some("full"), &[]).to_string();
    std::fs::write(root.join("references.jsonl"), format!("{line}\n")).unwrap();
    let bin = env!("CARGO_BIN_EXE_claimcheck");
    let run = |args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    let stdout = run(&["check", "--format", "jsonl", "--measure"]);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let measures: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r["schema"] == "rf.measure/1")
        .collect();
    assert_eq!(measures.len(), 2, "{stdout}");
    assert_eq!(
        rows[rows.len() - 2..].iter().collect::<Vec<_>>(),
        measures,
        "the records follow the findings"
    );
    // Citing: two sentences, the table row, the blockquote and the sentence
    // that cites it in a.md; two sentences in b.md. In neither unit: the
    // heading, the paragraph of only a citation and the reference list, and
    // the uncited table rows and blockquote (not prose). Uncited tables and
    // blockquotes leave the citing unit as it was.
    // "Retrieval helps" is one key in both files, wherever its citation
    // stands. Keys hash the text without citation markers or the whitespace
    // before them, e.g.
    // printf 'It reaches 12.3 %% accuracy.' | shasum -a 256
    let citing = json!({
        "schema": "rf.measure/1",
        "tool": "claimcheck",
        "unit": "citing-sentence",
        "count": 7,
        "keys": [
            "45195695e16dae10", // As the paper notes:
            "491aee4147607ebd", // We study financial evidence retrieval for annual reports and more.
            "527955041d16026a", // It reaches 12.3 % accuracy.
            "7c6f9ee69ba13da4", // See too.
            "d453de4d99621a35", // A 71.2
            "e94b58cad0101603", // Retrieval helps.
        ],
    });
    // Uncited: the prose sentences without a citation outside blockquotes:
    // "No citation here." in both files and "Retrieval helps." without a
    // citation in b.md, whose key is also a citing key. Not the table header
    // "Model Score", the row "B 64.0" or the blockquote "A plain aside.".
    let uncited = json!({
        "schema": "rf.measure/1",
        "tool": "claimcheck",
        "unit": "uncited-sentence",
        "count": 3,
        "keys": [
            "1acbcc3c80e3d6c2", // No citation here.
            "e94b58cad0101603", // Retrieval helps.
        ],
    });
    assert_eq!(measures, [&citing, &uncited]);

    let plain = run(&["check", "--format", "jsonl"]);
    assert!(!plain.contains("rf.measure/1"), "{plain}");
    assert_eq!(plain.lines().count(), rows.len() - 2);

    // In text mode the records are the last lines, after the summary.
    let text = run(&["check", "--measure", "--summary"]);
    assert!(text.contains("citations 8"), "{text}");
    let last: Vec<serde_json::Value> = text
        .lines()
        .rev()
        .take(2)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(last, [uncited, citing]);
}

#[test]
fn measure_counts_only_prose_outside_blockquotes_as_uncited() {
    let fx = basic();
    let measure = |md: &str| {
        let st = check_document(&fx.project, "doc.md", md.to_string(), 0).1;
        (st.citing.count, st.uncited.count)
    };
    // Prose: a paragraph and a list item.
    assert_eq!(
        measure("Accuracy rose. It fell.\n\n- A list item.\n"),
        (0, 3)
    );
    // Table rows, header included, and blockquote sentences that cite
    // nothing are in no unit; a cited row still cites.
    assert_eq!(
        measure("| Model | Score |\n| --- | --- |\n| A | 64.0 |\n| B | 71.2 [1] |\n"),
        (1, 0)
    );
    assert_eq!(measure("> An aside. Another one.\n"), (0, 0));
    // A cited blockquote still cites, sentence by sentence.
    assert_eq!(
        measure(
            "> We study financial evidence retrieval for annual reports. Our method reaches 12.3 % accuracy [1].\n"
        ),
        (2, 0)
    );
    // The paragraph next to them still counts.
    assert_eq!(measure("> An aside.\n\nNo citation here.\n"), (0, 1));
    // A blockquote not checked as a quotation, because it is too short or
    // its key does not resolve: its cited sentences cite, the others are in
    // no unit. So is a list item inside a blockquote.
    assert_eq!(measure("> Short. Yes [1].\n"), (1, 0));
    assert_eq!(measure("> It rose. Accuracy rose 12 % [9].\n"), (1, 0));
    assert_eq!(measure("> - A list item.\n"), (0, 0));
    // Every sentence uncited-claim reports is an uncited-sentence unit.
    let md = "Accuracy rose to 87.5% in the pilot. Our system outperforms every baseline.\n\n- Recall fell to 40% overall.\n\n> Recall fell to 30% overall.\n";
    let (f, st) = check_document(&fx.project, "doc.md", md.to_string(), 0);
    assert_eq!(rules(&f), [(Rule::UncitedClaim, Severity::Warn); 3]);
    assert_eq!((st.citing.count, st.uncited.count), (0, 3));
}

#[test]
fn measure_puts_each_prose_sentence_in_one_unit() {
    // (sentence as written, its key text, whether it cites)
    let sentences = [
        ("Retrieval helps [1].", "Retrieval helps.", true),
        (
            "Prices rose 4 % in 2024.",
            "Prices rose 4 % in 2024.",
            false,
        ),
        (
            "Cafe\u{301}  opens\n   daily.",
            "Caf\u{e9} opens daily.",
            false,
        ),
        (
            "It reaches 12.3 %\n accuracy [1, 8].",
            "It reaches 12.3 % accuracy.",
            true,
        ),
        (
            "An unresolved key still cites [8].",
            "An unresolved key still cites.",
            true,
        ),
        ("Most teams agree.", "Most teams agree.", false),
    ];
    let doc: Vec<&str> = sentences.iter().map(|s| s.0).collect();
    let doc = format!("{}\n", doc.join(" "));
    let root = temp_project(&[
        (
            "research.toml",
            "[project]\nname = \"t\"\n\n[claimcheck]\npaths = [\"docs\"]\n",
        ),
        ("docs/a.md", &doc),
    ]);
    let line = reference("1", Some("full"), &[]).to_string();
    std::fs::write(root.join("references.jsonl"), format!("{line}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_claimcheck"))
        .args(["check", "--format", "jsonl", "--measure"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let measures: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|r| r["schema"] == "rf.measure/1")
        .collect();

    let key = |text: &str| -> String {
        let digest = Sha256::digest(text.as_bytes());
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    };
    let record = |unit: &str, cites: bool| {
        let mut keys: Vec<String> = sentences
            .iter()
            .filter(|s| s.2 == cites)
            .map(|s| key(s.1))
            .collect();
        keys.sort();
        json!({
            "schema": "rf.measure/1",
            "tool": "claimcheck",
            "unit": unit,
            "count": keys.len(),
            "keys": keys,
        })
    };
    assert_eq!(
        measures,
        [
            record("citing-sentence", true),
            record("uncited-sentence", false)
        ],
        "{stdout}"
    );
    let total = measures[0]["count"].as_u64().unwrap() + measures[1]["count"].as_u64().unwrap();
    assert_eq!(total, sentences.len() as u64);
}

#[test]
fn quote_that_cuts_a_number_is_not_verbatim() {
    let fx = basic();
    let f = fx.check("They write that \"our method reaches 12\" [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)], "{f:#?}");
    assert_eq!(f[0].data["cuts_token"], true);
    assert!(
        f[0].message.contains("inside a word or number"),
        "{}",
        f[0].message
    );
    // A quotation that starts inside a word is not verbatim either.
    let f = fx.check("They write that \"nancial evidence retrieval\" [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)], "{f:#?}");
    // The whole number, with its percent sign, is verbatim.
    assert!(
        fx.check("They write that \"our method reaches 12.3 % accuracy\" [1].\n")
            .is_empty()
    );
}

#[test]
fn minus_signs_are_significant() {
    let text = "The change in recall was \u{2212}2.1 points, and precision rose 0.5 points.\n";
    let fx = Fixture::new(
        &[reference("1", Some("full"), &["texts/1.txt"])],
        &[("texts/1.txt", text)],
        "",
    );
    let f = fx.check("They report that \"the change in recall was 2.1 points\" [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)], "{f:#?}");
    assert!(
        fx.check("They report that \"the change in recall was -2.1 points\" [1].\n")
            .is_empty()
    );
    // A negative number needs a negative source number; an unsigned one does not.
    let f = fx.check("Precision changed by -0.5 points [1].\n");
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Warn)],
        "{f:#?}"
    );
    assert_eq!(f[0].data["number"], "-0.5");
    assert!(fx.check("Recall changed by -2.1 points [1].\n").is_empty());
    assert!(fx.check("Recall fell by 2.1 points [1].\n").is_empty());
}

fn unmatched_numbers(fx: &Fixture, md: &str) -> Vec<String> {
    fx.check(md)
        .iter()
        .filter(|f| f.rule == Rule::NumberNotInSource)
        .map(|f| f.data["number"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn month_like_words_and_measurement_words_do_not_mask_numbers() {
    let fx = basic();
    for (md, want) in [
        ("Error decreased 45.6% relative to BM25 [1].\n", "45.6%"),
        ("Recall declined 345 points [1].\n", "345"),
        ("The market grew 234 points [1].\n", "234"),
        ("A novel 567 item benchmark [1].\n", "567"),
        ("At significance level 0.05 the gain holds [1].\n", "0.05"),
        ("At confidence level 95% the gain holds [1].\n", "95%"),
        ("The table has rows 2,378 in total [1].\n", "2,378"),
    ] {
        assert_eq!(unmatched_numbers(&fx, md), [want], "{md}");
    }
    // Real dates, times and locators stay masked.
    let md = "On Jan 5, 2026 at 10:30, Section 4.3 and rows 2-4 of 12/10/2026 [1].\n";
    assert!(unmatched_numbers(&fx, md).is_empty(), "{md}");
}

#[test]
fn large_decimals_and_decimal_ranges_are_not_dates() {
    let fx = basic();
    assert_eq!(
        unmatched_numbers(&fx, "It costs 9999.99 dollars [1].\n"),
        ["9999.99"]
    );
    assert_eq!(
        unmatched_numbers(&fx, "Scores ranged 55.1-58.4 across runs [1].\n"),
        ["55.1", "58.4"]
    );
    assert_eq!(
        unmatched_numbers(&fx, "Scores ranged 12.1-15.4 across runs [1].\n"),
        ["12.1", "15.4"]
    );
}

#[test]
fn citation_between_full_stop_and_space_ends_the_sentence() {
    let fx = basic();
    let f = fx.check(
        "The retriever uses a two-stage pipeline.[2] Our system outperforms every baseline.\n",
    );
    assert_eq!(
        rules(&f),
        [
            (Rule::DepthInsufficient, Severity::Error),
            (Rule::UncitedClaim, Severity::Warn),
        ],
        "{f:#?}"
    );
    assert_eq!(
        f[0].data["sentence"],
        "The retriever uses a two-stage pipeline."
    );
    assert!(f[1].message.contains("outperforms"), "{}", f[1].message);
}

#[test]
fn unresolved_citation_is_not_an_uncited_claim() {
    let fx = basic();
    let f = fx.check("Our system outperforms every baseline [9].\n");
    assert_eq!(
        rules(&f),
        [(Rule::CitationUnresolved, Severity::Error)],
        "{f:#?}"
    );
}

#[test]
fn numbers_in_a_checked_blockquote_are_not_rechecked() {
    let fx = basic();
    let md = "> Our method reaches 12.4 % accuracy on 1234 questions [1]\n";
    let f = fx.check(md);
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Warn)], "{f:#?}");
}

#[test]
fn semicolon_separated_numeric_keys_are_citations() {
    let fx = basic();
    let f = fx.check("See [2; 9] for details.\n");
    assert_eq!(
        rules(&f),
        [(Rule::CitationUnresolved, Severity::Error)],
        "{f:#?}"
    );
    assert_eq!(f[0].data["reference"], "9");
    // A locator after the semicolon is not a key.
    assert!(fx.check("See [1; p. 4] and [1; note].\n").is_empty());
}

#[test]
fn severity_override_applies_to_every_finding_of_the_rule() {
    let fx = Fixture::new(
        &[reference("1", Some("full"), &["texts/1.txt"])],
        &[("texts/1.txt", SOURCE)],
        "[claimcheck.severity]\nquote-not-found = \"info\"\n",
    );
    // The rule's default severity is error; its near misses are warn.
    let f = fx.check("They study “financial evidence retrieval for quarterly reports” [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Info)]);
    let f = fx.check("They claim \"graph neural networks solve every retrieval task\" [1].\n");
    assert_eq!(rules(&f), [(Rule::QuoteNotFound, Severity::Info)]);
    let fx = Fixture::new(
        &[reference("2", Some("abstract"), &[])],
        &[],
        "[claimcheck.severity]\ndepth-insufficient = \"error\"\n",
    );
    // A numbers-only depth finding is info by default.
    let f = fx.check("It reaches 71.2 on FinQA [2].\n");
    assert_eq!(
        rules(&f),
        [
            (Rule::SourceTextMissing, Severity::Warn),
            (Rule::DepthInsufficient, Severity::Error)
        ]
    );
}

#[test]
fn depth_ignores_kinds_without_required_depth() {
    let toml = "[claimcheck.claims]\nnumber_kind = \"result\"\n[claimcheck.claims.kinds]\nresult = [\"outperforms\"]\ndataset = [\"benchmark\"]\n[claimcheck.claims.required_depth]\nresult = \"method\"\n";
    let fx = Fixture::new(&[reference("2", Some("abstract"), &[])], &[], toml);
    let f = fx.check("It outperforms BM25 on the benchmark [2].\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Error)]);
}

#[test]
fn kinds_only_claims_config_loads() {
    let root = temp_project(&[]);
    let toml = "[claimcheck.claims]\nnumber_kind = \"result\"\n[claimcheck.claims.kinds]\nresult = [\"outperforms\"]\n";
    let cfg = Config::from_toml(&root, toml).unwrap();
    assert_eq!(
        cfg.claims.required_depth.keys().collect::<Vec<_>>(),
        ["result"]
    );
    let fx = Fixture::new(&[reference("2", Some("abstract"), &[])], &[], toml);
    let f = fx.check("It outperforms BM25 [2].\n");
    assert_eq!(rules(&f), [(Rule::DepthInsufficient, Severity::Error)]);
    // An explicit table is still validated.
    let bad = format!("{toml}[claimcheck.claims.required_depth]\nmethod = \"method\"\n");
    let err = Config::from_toml(&root, &bad).unwrap_err().to_string();
    assert!(err.contains("unknown claim kind `method`"), "{err}");
}

fn names(root: &Path, files: &[PathBuf]) -> Vec<String> {
    let root = root.canonicalize().unwrap();
    files
        .iter()
        .map(|f| {
            let f = f.canonicalize().unwrap();
            f.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn symlink_loops_are_walked_once_and_broken_links_are_errors() {
    use std::os::unix::fs::symlink;
    let root = temp_project(&[("docs/a.md", "A.\n"), ("docs/sub/b.md", "B.\n")]);
    symlink("..", root.join("docs/sub/up")).unwrap();
    symlink("sub/b.md", root.join("docs/b-link.md")).unwrap();
    let files =
        claimcheck::collect_inputs(&[root.join("docs"), root.join("docs/sub")], &[]).unwrap();
    assert_eq!(names(&root, &files), ["docs/a.md", "docs/sub/b.md"]);

    symlink("missing", root.join("docs/sub/gone")).unwrap();
    let err = claimcheck::collect_inputs(&[root.join("docs")], &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("gone"), "{err}");
    // An excluded broken link is not followed.
    let files =
        claimcheck::collect_inputs(&[root.join("docs")], &[root.join("docs/sub/gone")]).unwrap();
    assert_eq!(files.len(), 2);
}

#[test]
fn exclude_applies_to_relative_and_dotted_inputs() {
    let root = temp_project(&[
        (
            "research.toml",
            "[claimcheck]\nexclude = [\"docs/drafts\", \"./docs/old/../tmp.md\"]\n",
        ),
        ("references.jsonl", ""),
        ("docs/a.md", "A.\n"),
        ("docs/drafts/x.md", "Cites [8].\n"),
        ("docs/tmp.md", "Cites [8].\n"),
        ("sub/keep", ""),
    ]);
    let bin = env!("CARGO_BIN_EXE_claimcheck");
    for (cwd, arg) in [("sub", "../docs"), (".", "./docs/../docs")] {
        let out = Command::new(bin)
            .args(["check", arg, "--summary"])
            .current_dir(root.join(cwd))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{arg}: {stdout}");
        assert!(stdout.contains("files 1"), "{arg}: {stdout}");
    }
}

#[test]
fn check_without_paths_is_a_usage_error() {
    let root = temp_project(&[("research.toml", ""), ("references.jsonl", "")]);
    let out = Command::new(env!("CARGO_BIN_EXE_claimcheck"))
        .args(["check"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no input paths"));
}

#[test]
fn a_link_in_the_same_sentence_lowers_a_missing_number_to_info_when_enabled() {
    let refs = [reference("1", Some("full"), &["texts/1.txt"])];
    let texts = [("texts/1.txt", SOURCE)];
    let on = Fixture::new(
        &refs,
        &texts,
        "[claimcheck.numbers]\nlinks_are_evidence = true\n",
    );
    let off = Fixture::new(&refs, &texts, "");
    let linked = "The corpus has 4321 filings [1], counted in [our audit](audit.md).\n";
    let plain = "The corpus has 4321 filings [1].\n";
    // A link in another sentence of the same paragraph.
    let elsewhere = "The corpus has 4321 filings [1]. See [our audit](audit.md).\n";

    let f = on.check(linked);
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Info)],
        "{f:#?}"
    );
    assert_eq!(f[0].data["linked"], true);
    let f = on.check(plain);
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Warn)],
        "{f:#?}"
    );
    assert!(f[0].data.get("linked").is_none());
    let f = on.check(elsewhere);
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Warn)],
        "{f:#?}"
    );
    // Off by default.
    let f = off.check(linked);
    assert_eq!(
        rules(&f),
        [(Rule::NumberNotInSource, Severity::Warn)],
        "{f:#?}"
    );
    assert!(f[0].data.get("linked").is_none());
    // A number the source has is still not reported.
    assert!(
        on.check("They answer 1234 questions [1], see [our audit](audit.md).\n")
            .is_empty()
    );
}
