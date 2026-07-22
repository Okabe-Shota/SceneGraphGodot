//! `sg`: command-line tool for inspecting and validating Godot text
//! resource files (`.tscn` / `.tres`) using scenegraph-core.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use scenegraph_core::Document;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Parse { file } => cmd_parse(&file),
        Command::Roundtrip { file } => cmd_roundtrip(&file),
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
