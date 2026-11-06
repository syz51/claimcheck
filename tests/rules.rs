//! End-to-end rule tests on synthetic projects.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use claimcheck::check::{Project, check_document};
use claimcheck::config::Config;
use claimcheck::finding::{Finding, Rule, Severity};
use claimcheck::registry::Registry;
use serde_json::json;

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
