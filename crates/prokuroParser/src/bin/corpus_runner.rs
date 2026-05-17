//! Corpus runner: parses every file in corpus/raw/ and compares against corpus/expected/.
//!
//! Usage:
//!   cargo run --bin corpus-runner              # auto-discovers corpus/ from workspace root
//!   cargo run --bin corpus-runner /path/to/corpus
//!
//! Exits 0 if all files with expected ground truth pass; exits 1 if any fail.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use prokuro_parser::pipeline::parse_file;
use serde::Deserialize;
use serde_json::json;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

// Values in expected columnMapping that our system should account for.
const COMPARABLE_VALUES: &[&str] = &[
    "mpn",
    "qty",
    "reference",
    "refdes",
    "manufacturer",
    "description",
    "value",
    "footprint",
    "alternate_mpn",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expected {
    header_row_index: usize,
    sheet_name: Option<String>,
    #[serde(default)]
    column_mapping: HashMap<String, String>,
}

enum MatchResult {
    Pass,
    Fail(Vec<String>),
    NoExpected,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        find_corpus_dir()
    };

    let raw_dir = corpus_dir.join("raw");
    let expected_dir = corpus_dir.join("expected");
    let reports_dir = corpus_dir.join("reports");
    fs::create_dir_all(&reports_dir).expect("should create reports dir");

    let mut entries: Vec<PathBuf> = fs::read_dir(&raw_dir)
        .expect("corpus/raw should be readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("csv" | "xlsx")))
        .collect();
    entries.sort();

    // (filename, confidence, warning_count, match_label)
    let mut table_rows: Vec<(String, f32, usize, String)> = Vec::new();
    let mut mapping_failures: Vec<serde_json::Value> = Vec::new();
    let mut any_fail = false;

    for path in &entries {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();

        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("ERROR reading {filename}: {e}");
                continue;
            }
        };

        let result = match parse_file(&bytes, &filename).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR parsing {filename}: {e}");
                any_fail = true;
                table_rows.push((filename, 0.0, 0, format!("error: {e}")));
                continue;
            }
        };

        let expected_path = expected_dir.join(format!("{stem}.json"));
        let match_result = if expected_path.exists() {
            compare_result(&result, &expected_path)
        } else {
            MatchResult::NoExpected
        };

        let match_label = match &match_result {
            MatchResult::Pass => "pass".to_string(),
            MatchResult::Fail(msgs) => {
                any_fail = true;
                eprintln!("FAIL {filename}:");
                for m in msgs {
                    eprintln!("  {m}");
                }
                mapping_failures.push(json!({ "file": filename, "failures": msgs }));
                "FAIL".to_string()
            }
            MatchResult::NoExpected => "no_expected".to_string(),
        };

        table_rows.push((filename, result.mapping_confidence, result.warnings.len(), match_label));
    }

    let table = render_table(&table_rows);
    print!("{table}");

    fs::write(reports_dir.join("latest.md"), &table).expect("should write latest.md");
    fs::write(
        reports_dir.join("mapping-failures.json"),
        serde_json::to_string_pretty(&mapping_failures).unwrap(),
    )
    .expect("should write mapping-failures.json");

    std::process::exit(if any_fail { 1 } else { 0 });
}

fn compare_result(
    result: &prokuro_parser::pipeline::ParseResult,
    expected_path: &Path,
) -> MatchResult {
    let content = match fs::read_to_string(expected_path) {
        Ok(s) => s,
        Err(e) => return MatchResult::Fail(vec![format!("cannot read expected: {e}")]),
    };
    let expected: Expected = match serde_json::from_str(&content) {
        Ok(e) => e,
        Err(e) => return MatchResult::Fail(vec![format!("bad expected JSON: {e}")]),
    };

    let mut failures: Vec<String> = Vec::new();

    if result.header_row_index != expected.header_row_index {
        failures.push(format!(
            "headerRowIndex: got {} want {}",
            result.header_row_index, expected.header_row_index
        ));
    }

    if let Some(ref exp_sheet) = expected.sheet_name {
        if result.sheet_name.as_deref() != Some(exp_sheet.as_str()) {
            failures.push(format!(
                "sheetName: got {:?} want {:?}",
                result.sheet_name, exp_sheet
            ));
        }
    }

    for (header, canonical) in &expected.column_mapping {
        if !COMPARABLE_VALUES.contains(&canonical.as_str()) {
            continue;
        }
        if !result.column_mapping.contains_key(header.as_str()) {
            failures.push(format!("missing mapping key '{header}' → '{canonical}'"));
        }
    }

    if failures.is_empty() {
        MatchResult::Pass
    } else {
        MatchResult::Fail(failures)
    }
}

fn render_table(rows: &[(String, f32, usize, String)]) -> String {
    let mut out = String::new();
    out.push_str("| filename | confidence | warnings | expected_match |\n");
    out.push_str("|---|---|---|---|\n");
    for (filename, conf, warn_count, label) in rows {
        out.push_str(&format!(
            "| {filename} | {conf:.2} | {warn_count} | {label} |\n"
        ));
    }
    out
}

fn find_corpus_dir() -> PathBuf {
    let mut dir = PathBuf::from(MANIFEST_DIR);
    loop {
        let candidate = dir.join("corpus");
        if candidate.is_dir() {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }
    PathBuf::from("corpus")
}
