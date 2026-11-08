use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use claimcheck::check::{Project, Stats};
use claimcheck::config::Config;
use claimcheck::finding::{self, Finding, Rule, Severity};
use claimcheck::registry::Registry;
use claimcheck::{Error, collect_inputs, run};

#[derive(Parser)]
#[command(
    version,
    about = "Check that research documents are supported by the sources they cite"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check Markdown files or directories (default: [claimcheck] paths).
    Check(CheckArgs),
    /// List the rules with their ids, default severities and descriptions.
    Rules,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Jsonl,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// Markdown files or directories.
    paths: Vec<PathBuf>,
    /// Path to research.toml (default: search upward from the working directory).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Reference registry (overrides the configuration).
    #[arg(long)]
    registry: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Print counts by rule and severity (on stderr with --format jsonl).
    #[arg(long)]
    summary: bool,
    /// Hide findings below this severity (the exit code still counts every error).
    #[arg(long, value_enum, default_value_t = MinSeverity::Info)]
    min_severity: MinSeverity,
    /// After the findings, print one rf.measure/1 line counting the sentences that cite a reference.
    #[arg(long)]
    measure: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MinSeverity {
    Info,
    Warn,
    Error,
}

impl MinSeverity {
    fn severity(self) -> Severity {
        match self {
            MinSeverity::Info => Severity::Info,
            MinSeverity::Warn => Severity::Warn,
            MinSeverity::Error => Severity::Error,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Rules => print_rules()
            .map(|()| false)
            .map_err(|e| Error::new(e.to_string())),
        Command::Check(args) => check(args),
    };
    match result {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(1),
        Err(e) => {
            eprintln!("claimcheck: {e}");
            ExitCode::from(2)
        }
    }
}

fn print_rules() -> io::Result<()> {
    let mut out = io::stdout().lock();
    for r in Rule::ALL {
        writeln!(
            out,
            "{:<24} {:<5}  {}",
            r.id(),
            r.default_severity(),
            r.summary()
        )?;
    }
    Ok(())
}

/// Runs a check; returns whether any finding is an error.
fn check(args: CheckArgs) -> Result<bool, Error> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::new(format!("cannot read working directory: {e}")))?;
    let mut cfg = Config::load(&cwd, args.config.as_deref())?;
    if let Some(r) = &args.registry {
        cfg.registry = cwd.join(r);
    }
    let inputs: Vec<PathBuf> = if args.paths.is_empty() {
        if cfg.paths.is_empty() {
            return Err(Error::new(
                "no input paths: pass files or directories, or set [claimcheck] paths",
            ));
        }
        cfg.paths.clone()
    } else {
        args.paths.iter().map(|p| cwd.join(p)).collect()
    };
    let files = collect_inputs(&inputs, &cfg.exclude)?;
    let registry_display = cfg.display_path(&cfg.registry);
    let registry = Registry::load(&cfg.registry, &registry_display)?;
    let project = Project::new(cfg, registry);
    let (findings, stats) = run(&project, &files)?;
    let any_error = findings.iter().any(|f| f.severity == Severity::Error);
    let shown: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.severity >= args.min_severity.severity())
        .collect();
    let write = |w: &mut dyn Write| -> io::Result<()> {
        match args.format {
            Format::Jsonl => {
                let at = finding::now_rfc3339();
                for f in &shown {
                    writeln!(w, "{}", f.to_json(&at))?;
                }
            }
            Format::Text => {
                for f in &shown {
                    write_text(w, f)?;
                }
            }
        }
        if args.measure {
            writeln!(w, "{}", stats.measure())?;
        }
        Ok(())
    };
    let io_err = |e: io::Error| Error::new(format!("cannot write output: {e}"));
    write(&mut io::stdout().lock()).map_err(io_err)?;
    if args.summary {
        let text = summary(&findings, &stats);
        if args.format == Format::Jsonl {
            eprint!("{text}");
        } else {
            print!("{text}");
        }
    }
    Ok(any_error)
}

fn write_text(w: &mut dyn Write, f: &Finding) -> io::Result<()> {
    writeln!(
        w,
        "{}: {}[{}] {}",
        f.subject,
        f.severity,
        f.rule.id(),
        f.message
    )?;
    for e in &f.evidence {
        writeln!(w, "    --> {} ({})", e.path, e.locator)?;
    }
    if let Some(q) = f.data.get("quote").and_then(|v| v.as_str()) {
        writeln!(w, "    quote: \"{q}\"")?;
    }
    if let Some(m) = f
        .data
        .get("best_match")
        .and_then(|v| v.get("excerpt"))
        .and_then(|v| v.as_str())
    {
        writeln!(w, "    match: \"{m}\"")?;
    }
    Ok(())
}

fn summary(findings: &[Finding], stats: &Stats) -> String {
    let mut counts: BTreeMap<&str, [usize; 3]> = BTreeMap::new();
    for r in Rule::ALL {
        counts.insert(r.id(), [0; 3]);
    }
    for f in findings {
        let c = counts.entry(f.rule.id()).or_default();
        c[f.severity as usize] += 1;
    }
    let mut s = String::new();
    s.push_str(&format!(
        "\n{:<24} {:>6} {:>6} {:>6}\n",
        "rule", "error", "warn", "info"
    ));
    let mut total = [0; 3];
    for (rule, c) in &counts {
        s.push_str(&format!("{rule:<24} {:>6} {:>6} {:>6}\n", c[2], c[1], c[0]));
        for i in 0..3 {
            total[i] += c[i];
        }
    }
    s.push_str(&format!(
        "{:<24} {:>6} {:>6} {:>6}\n\n",
        "total", total[2], total[1], total[0]
    ));
    s.push_str(&format!(
        "files {}; citations {} ({} resolved); quotations checked {} ({} verbatim); numbers checked {} ({} found); claims classified {}\n",
        stats.files,
        stats.citations,
        stats.citations_resolved,
        stats.quotes_checked,
        stats.quotes_supported,
        stats.numbers_checked,
        stats.numbers_found,
        stats.claims_classified,
    ));
    s
}
