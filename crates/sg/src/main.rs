//! `sg`: command-line tool for inspecting and validating Godot text
//! resource files (`.tscn` / `.tres`) using scenegraph-core.

mod config;
mod diff;
mod engine;
mod fix;
mod i18n;
mod json;
mod nodegraph;
mod paths;
mod respath;
mod rules;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use scenegraph_core::Document;

use config::ConfigCache;
use rules::{Issue, Severity};

#[derive(Parser)]
#[command(name = "sg", about = "Inspect and validate Godot text resource files", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse a file and print structural statistics.
    Parse {
        /// Path to a .tscn or .tres file.
        file: PathBuf,
    },
    /// Parse a file, serialize it back, and verify the result is
    /// byte-for-byte identical to the input. Exits non-zero on mismatch or
    /// on any read/parse error.
    Roundtrip {
        /// Path to a .tscn or .tres file.
        file: PathBuf,
    },
    /// Report structural problems in .tscn/.tres files: load_steps
    /// mismatches, broken/circular/duplicate resource references, nodes
    /// declared before their parent, and unused resources.
    Check {
        /// Files or directories to check (directories are searched
        /// recursively for *.tscn/*.tres).
        paths: Vec<PathBuf>,
        /// Emit a machine-readable JSON array instead of text lines.
        #[arg(long)]
        json: bool,
        /// After the static check passes without a parse error, also
        /// verify each file actually loads in a headless Godot engine
        /// instance - the engine's own judgment, not just sg's static
        /// rules. See README.md, "sg check --engine".
        #[arg(long)]
        engine: bool,
        /// Path to the Godot executable used by --engine. Overrides the
        /// SG_GODOT environment variable and PATH search (see README.md).
        #[arg(long)]
        godot_path: Option<PathBuf>,
        /// Per-project timeout, in seconds, for the headless Godot
        /// process launched by --engine.
        #[arg(long, default_value_t = 30)]
        engine_timeout: u64,
    },
    /// Fix everything `sg check` reports as mechanically fixable, in
    /// place. Issues it cannot safely fix (broken/circular/duplicate
    /// references, orphan nodes, multiple roots) are left in the file and
    /// reported.
    Fix {
        /// Files or directories to fix (directories are searched
        /// recursively for *.tscn/*.tres).
        paths: Vec<PathBuf>,
        /// Show what would change (including a unified diff) without
        /// writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete unused ext_resource/sub_resource sections.
        #[arg(long)]
        keep_unused: bool,
    },
    /// Localization tooling built on scenegraph-core's structural model.
    /// See README.md, "sg i18n extract".
    I18n {
        #[command(subcommand)]
        command: I18nCommand,
    },
}

#[derive(Subcommand)]
enum I18nCommand {
    /// Scan scene files for translatable UI strings and emit a gettext PO
    /// (default) or CSV translation file, with per-string context
    /// (node type, screen, property, and a source reference).
    Extract {
        /// Files or directories to scan (directories are searched
        /// recursively for *.tscn/*.tres, same as `sg check`).
        paths: Vec<PathBuf>,
        /// Output format.
        #[arg(long, value_enum, default_value = "po")]
        format: i18n::extract::Format,
        /// Write the result to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Parse { file } => cmd_parse(&file),
        Command::Roundtrip { file } => cmd_roundtrip(&file),
        Command::Check {
            paths,
            json,
            engine,
            godot_path,
            engine_timeout,
        } => cmd_check(&paths, json, engine, godot_path.as_deref(), engine_timeout),
        Command::Fix {
            paths,
            dry_run,
            keep_unused,
        } => cmd_fix(&paths, dry_run, keep_unused),
        Command::I18n { command } => match command {
            I18nCommand::Extract { paths, format, output } => i18n::extract::run(&paths, format, output.as_deref()),
        },
    }
}

fn read_source(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    String::from_utf8(bytes).map_err(|e| format!("'{}' is not valid UTF-8: {e}", path.display()))
}

fn cmd_parse(path: &Path) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let (doc, diagnostics) = Document::parse_tolerant(&source);
    for d in &diagnostics {
        eprintln!("warning: {d}");
    }

    let stats = doc.stats();
    println!("file: {}", path.display());
    println!("sections: {}", stats.section_count);
    println!("ext_resources: {}", stats.ext_resource_count);
    println!("sub_resources: {}", stats.sub_resource_count);
    println!("nodes: {}", stats.node_count);
    println!("connections: {}", stats.connection_count);
    println!("references: {}", stats.reference_count);

    if let Some(fd) = doc.file_descriptor() {
        println!("format: {:?}", fd.format);
        println!("load_steps: {:?}", fd.load_steps);
    }

    if diagnostics.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("{} diagnostic(s) recovered in tolerant mode", diagnostics.len());
        ExitCode::FAILURE
    }
}

fn cmd_roundtrip(path: &Path) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let doc = match Document::parse(&source) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("parse error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let output = doc.serialize();
    if output == source {
        println!(
            "OK: '{}' round-trips byte-for-byte ({} bytes)",
            path.display(),
            source.len()
        );
        return ExitCode::SUCCESS;
    }

    // Find and report the first point of divergence.
    let a = source.as_bytes();
    let b = output.as_bytes();
    let mismatch = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()));

    let line = 1 + a[..mismatch.min(a.len())].iter().filter(|&&c| c == b'\n').count();
    let col = match a[..mismatch.min(a.len())].iter().rposition(|&c| c == b'\n') {
        Some(nl) => mismatch - nl,
        None => mismatch + 1,
    };

    eprintln!("MISMATCH: '{}' does not round-trip", path.display());
    eprintln!("  first difference at byte {mismatch} (line {line}, column {col})");
    eprintln!(
        "  original length: {} bytes, serialized length: {} bytes",
        a.len(),
        b.len()
    );

    let context = |data: &[u8], at: usize| -> String {
        let start = at.saturating_sub(20);
        let end = (at + 20).min(data.len());
        String::from_utf8_lossy(&data[start..end])
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    eprintln!("  original : ...{}...", context(a, mismatch));
    eprintln!("  serialized: ...{}...", context(b, mismatch));

    ExitCode::FAILURE
}

fn print_issue_line(file: &Path, issue: &Issue) {
    println!(
        "{}:{}: {} [{}] {}",
        file.display(),
        issue.line,
        issue.severity.as_str(),
        issue.code,
        issue.message
    );
}

fn parse_error_issue(e: &scenegraph_core::ParseError) -> Issue {
    Issue {
        code: "parse-error",
        severity: Severity::Error,
        line: e.line,
        message: e.message.clone(),
        fixable: false,
    }
}

/// Look up the `sg.toml` (if any) governing `file` through `cache`,
/// printing and deduplicating a config error exactly once per distinct
/// `sg.toml` path (many files commonly share one governing config, and
/// repeating the same "your sg.toml is broken" message once per file
/// would just be noise). Returns `None` when a config error was
/// encountered - the caller should treat the file like a parse failure
/// (same exit code, skip further processing) - `Some(_)` (possibly
/// `Some(None)` for "no sg.toml at all") otherwise.
fn load_file_config(
    file: &Path,
    cache: &mut ConfigCache,
    reported: &mut HashSet<PathBuf>,
) -> Option<Option<std::rc::Rc<config::RuleConfig>>> {
    match cache.load_for_file(file) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            if reported.insert(e.path.clone()) {
                eprintln!("error: {e}");
            }
            None
        }
    }
}

fn cmd_check(paths: &[PathBuf], json: bool, engine: bool, godot_path: Option<&Path>, engine_timeout: u64) -> ExitCode {
    let files = paths::collect_target_files(paths);
    let mut all: Vec<(PathBuf, Issue)> = Vec::new();
    let mut had_parse_error = false;
    let mut had_issue = false;
    let mut config_cache = ConfigCache::new();
    let mut reported_config_errors: HashSet<PathBuf> = HashSet::new();

    for file in &files {
        let source = match read_source(file) {
            Ok(s) => s,
            Err(msg) => {
                eprintln!("error: {msg}");
                had_parse_error = true;
                continue;
            }
        };
        let Some(file_config) = load_file_config(file, &mut config_cache, &mut reported_config_errors) else {
            had_parse_error = true;
            continue;
        };
        match Document::parse(&source) {
            Ok(doc) => {
                let issues = config::apply_to_issues(rules::check(&doc, file), file_config.as_deref());
                if !issues.is_empty() {
                    had_issue = true;
                }
                for issue in issues {
                    all.push((file.clone(), issue));
                }
            }
            Err(e) => {
                had_parse_error = true;
                all.push((file.clone(), parse_error_issue(&e)));
            }
        }
    }

    // The engine gate never runs on top of a parse error: a file sg
    // itself could not parse has nothing meaningful to hand to Godot, and
    // running the (potentially slow) engine pass over the files that did
    // parse would just delay reporting a failure that's already final.
    let mut env_error: Option<String> = None;
    if engine && !had_parse_error {
        match engine::run_engine_checks(&files, godot_path, Duration::from_secs(engine_timeout)) {
            Ok(engine_issues) => {
                if !engine_issues.is_empty() {
                    had_issue = true;
                }
                all.extend(engine_issues);
            }
            Err(msg) => env_error = Some(msg),
        }
    }

    if json {
        println!("{}", json::issues_array(&all));
    } else {
        for (file, issue) in &all {
            print_issue_line(file, issue);
        }
    }

    // An environment error (no Godot binary, or it could not be launched)
    // is reported last, after whatever static results were already found,
    // and takes priority over the exit code those results would otherwise
    // produce: it means engine verification did not run at all, which is
    // a different and more urgent problem than "some issues were found".
    if let Some(msg) = env_error {
        eprintln!("error: {msg}");
        return ExitCode::from(3);
    }

    if had_parse_error {
        ExitCode::from(2)
    } else if had_issue {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_fix(paths: &[PathBuf], dry_run: bool, keep_unused: bool) -> ExitCode {
    let files = paths::collect_target_files(paths);
    let mut had_parse_error = false;
    let mut had_remaining = false;
    let mut config_cache = ConfigCache::new();
    let mut reported_config_errors: HashSet<PathBuf> = HashSet::new();

    for file in &files {
        let source = match read_source(file) {
            Ok(s) => s,
            Err(msg) => {
                eprintln!("error: {msg}");
                had_parse_error = true;
                continue;
            }
        };
        let Some(file_config) = load_file_config(file, &mut config_cache, &mut reported_config_errors) else {
            had_parse_error = true;
            continue;
        };

        let result = match fix::fix_file(file, &source, keep_unused, file_config.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: parse error: {e}", file.display());
                had_parse_error = true;
                continue;
            }
        };

        let fixed_count = result.before.iter().filter(|i| i.fixable).count();
        if dry_run {
            if result.changed {
                println!("{}: would fix {fixed_count} issue(s)", file.display());
                print!("{}", result.diff);
            } else {
                println!("{}: clean", file.display());
            }
        } else if result.changed {
            match fs::write(file, &result.new_source) {
                Ok(()) => println!("{}: fixed {fixed_count} issue(s)", file.display()),
                Err(e) => {
                    eprintln!("error: failed to write '{}': {e}", file.display());
                    had_remaining = true;
                }
            }
        } else {
            println!("{}: clean", file.display());
        }

        for issue in &result.after {
            print_issue_line(&result.path, issue);
        }
        if !result.after.is_empty() {
            had_remaining = true;
        }
    }

    if had_parse_error {
        ExitCode::from(2)
    } else if had_remaining {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
