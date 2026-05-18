//! Corpus runner: parses every file in corpus/raw/ and compares against corpus/expected/.
//!
//! Usage:
//!   cargo run --bin corpus-runner                     # auto-discovers corpus/ from workspace root
//!   cargo run --bin corpus-runner /path/to/corpus     # uses corpus/raw/ + corpus/expected/
//!   cargo run --bin corpus-runner --raw-only /path    # skips expected comparison; writes batch-results.md

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use prokuro_parser::pipeline::parse_file;
use serde::Deserialize;
use serde_json::json;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

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
}

struct FileResult {
    filename: String,
    ext: String,
    confidence: f32,
    warn_count: usize,
    match_label: String,
    error: Option<String>,
    headers: Vec<String>,
}

#[tokio::main]
async fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let raw_only = raw_args.iter().any(|a| a == "--raw-only");
    let positional: Vec<&String> = raw_args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();

    let input_dir = if let Some(p) = positional.first() {
        PathBuf::from(p)
    } else {
        find_corpus_dir()
    };

    let raw_dir = input_dir.join("raw");
    let expected_dir = input_dir.join("expected");
    // Always write reports to the workspace corpus dir.
    let corpus_dir = find_corpus_dir();
    let reports_dir = corpus_dir.join("reports");
    fs::create_dir_all(&reports_dir).expect("should create reports dir");

    let mut entries: Vec<PathBuf> = fs::read_dir(&raw_dir)
        .expect("raw/ dir should be readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("csv" | "xlsx" | "txt")
            )
        })
        .collect();
    entries.sort();

    let mut results: Vec<FileResult> = Vec::new();
    let mut mapping_failures: Vec<serde_json::Value> = Vec::new();
    let mut any_fail = false;

    for path in &entries {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("ERROR reading {filename}: {e}");
                results.push(FileResult {
                    filename,
                    ext,
                    confidence: 0.0,
                    warn_count: 0,
                    match_label: format!("error: {e}"),
                    error: Some(format!("read error: {e}")),
                    headers: vec![],
                });
                continue;
            }
        };

        let parse_result = parse_file(&bytes, &filename).await;

        match parse_result {
            Err(e) => {
                if !raw_only {
                    any_fail = true;
                }
                results.push(FileResult {
                    filename,
                    ext,
                    confidence: 0.0,
                    warn_count: 0,
                    match_label: format!("error: {e}"),
                    error: Some(e.to_string()),
                    headers: vec![],
                });
            }
            Ok(result) => {
                let headers: Vec<String> = result.raw_headers.clone();

                let match_label = if raw_only {
                    // Skip expected comparison; just categorize by confidence.
                    if result.mapping_confidence >= 0.5 {
                        "ok".to_string()
                    } else if result.mapping_confidence >= 0.3 {
                        "low_confidence".to_string()
                    } else {
                        "very_low_confidence".to_string()
                    }
                } else {
                    let expected_path = expected_dir.join(format!("{stem}.json"));
                    if expected_path.exists() {
                        match compare_result(&result, &expected_path) {
                            MatchResult::Pass => "pass".to_string(),
                            MatchResult::Fail(msgs) => {
                                any_fail = true;
                                eprintln!("FAIL {filename}:");
                                for m in &msgs {
                                    eprintln!("  {m}");
                                }
                                mapping_failures
                                    .push(json!({ "file": filename, "failures": msgs }));
                                "FAIL".to_string()
                            }
                        }
                    } else {
                        "no_expected".to_string()
                    }
                };

                results.push(FileResult {
                    filename,
                    ext,
                    confidence: result.mapping_confidence,
                    warn_count: result.warnings.len(),
                    match_label,
                    error: None,
                    headers,
                });
            }
        }
    }

    // Always write the basic table.
    let table_rows: Vec<(String, f32, usize, String)> = results
        .iter()
        .map(|r| (r.filename.clone(), r.confidence, r.warn_count, r.match_label.clone()))
        .collect();
    let table = render_table(&table_rows);
    print!("{table}");
    fs::write(reports_dir.join("latest.md"), &table).expect("should write latest.md");
    fs::write(
        reports_dir.join("mapping-failures.json"),
        serde_json::to_string_pretty(&mapping_failures).unwrap(),
    )
    .expect("should write mapping-failures.json");

    if raw_only {
        // Load synonyms for gap analysis.
        let synonyms_path = corpus_dir.join("synonyms.toml");
        let synonyms = prokuro_parser::detect::synonyms::load_synonyms(&synonyms_path);
        let synonym_set: std::collections::HashSet<String> = synonyms
            .iter()
            .flat_map(|group| group.iter().map(|s| s.to_lowercase()))
            .collect();

        write_batch_results(&reports_dir, &results);
        write_synonym_improvements(&reports_dir, &results, &synonym_set);
        println!("\nReports written to {}", reports_dir.display());
    }

    std::process::exit(if any_fail { 1 } else { 0 });
}

fn write_batch_results(reports_dir: &Path, results: &[FileResult]) {
    let total = results.len();
    let success: Vec<&FileResult> =
        results.iter().filter(|r| r.confidence > 0.5).collect();
    let low_conf: Vec<&FileResult> =
        results.iter().filter(|r| r.confidence >= 0.3 && r.confidence <= 0.5).collect();
    let failed: Vec<&FileResult> = results
        .iter()
        .filter(|r| r.error.is_some() || r.confidence < 0.3)
        .collect();

    // File type distribution.
    let mut by_ext: HashMap<&str, usize> = HashMap::new();
    for r in results {
        *by_ext.entry(r.ext.as_str()).or_insert(0) += 1;
    }

    // Failure reason counts.
    let mut failure_reasons: HashMap<String, usize> = HashMap::new();
    for r in results {
        if let Some(ref e) = r.error {
            let key = truncate_error(e);
            *failure_reasons.entry(key).or_insert(0) += 1;
        } else if r.confidence < 0.3 {
            *failure_reasons.entry("very low mapping confidence".to_string()).or_insert(0) += 1;
        }
    }
    let mut failure_reasons_sorted: Vec<(String, usize)> = failure_reasons.into_iter().collect();
    failure_reasons_sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    // Top 10 column header patterns.
    let mut header_counts: HashMap<String, usize> = HashMap::new();
    for r in results {
        for h in &r.headers {
            *header_counts.entry(h.clone()).or_insert(0) += 1;
        }
    }
    let mut header_sorted: Vec<(String, usize)> = header_counts.into_iter().collect();
    header_sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let top_headers: Vec<(String, usize)> = header_sorted.into_iter().take(10).collect();

    // Sample of up to 5 failed files with errors.
    let failed_samples: Vec<&FileResult> = results
        .iter()
        .filter(|r| r.error.is_some())
        .take(5)
        .collect();

    let mut md = String::new();
    md.push_str("# Batch Parse Results\n\n");
    md.push_str(&format!("**Total files attempted:** {total}\n\n"));
    md.push_str(&format!(
        "**Parsed successfully** (confidence > 0.5): {}\n\n",
        success.len()
    ));
    md.push_str(&format!(
        "**Low confidence** (0.3–0.5): {}\n\n",
        low_conf.len()
    ));
    md.push_str(&format!("**Failed** (error or confidence < 0.3): {}\n\n", failed.len()));

    md.push_str("## File Type Distribution\n\n");
    md.push_str("| Extension | Count |\n|---|---|\n");
    let mut ext_sorted: Vec<(&str, usize)> = by_ext.into_iter().collect();
    ext_sorted.sort_by_key(|(e, _)| *e);
    for (ext, count) in &ext_sorted {
        md.push_str(&format!("| .{ext} | {count} |\n"));
    }

    md.push_str("\n## Most Common Failure Reasons\n\n");
    md.push_str("| Reason | Count |\n|---|---|\n");
    for (reason, count) in &failure_reasons_sorted {
        md.push_str(&format!("| {reason} | {count} |\n"));
    }
    if failure_reasons_sorted.is_empty() {
        md.push_str("| *(none)* | — |\n");
    }

    md.push_str("\n## Top 10 Column Header Patterns\n\n");
    md.push_str("| Header | Files |\n|---|---|\n");
    for (header, count) in &top_headers {
        md.push_str(&format!("| `{header}` | {count} |\n"));
    }
    if top_headers.is_empty() {
        md.push_str("| *(none)* | — |\n");
    }

    md.push_str("\n## Failed File Samples\n\n");
    if failed_samples.is_empty() {
        md.push_str("*(no hard failures)*\n");
    } else {
        for r in failed_samples {
            let err = r.error.as_deref().unwrap_or("unknown");
            md.push_str(&format!("- **{}**: {}\n", r.filename, err));
        }
    }

    fs::write(reports_dir.join("batch-results.md"), &md)
        .expect("should write batch-results.md");
}

fn write_synonym_improvements(
    reports_dir: &Path,
    results: &[FileResult],
    synonym_set: &std::collections::HashSet<String>,
) {
    // Count how many files each header appears in.
    let mut header_file_count: HashMap<String, usize> = HashMap::new();
    for r in results {
        for h in &r.headers {
            *header_file_count.entry(h.clone()).or_insert(0) += 1;
        }
    }

    // Filter to headers appearing in 3+ files that are NOT in the synonym set.
    let mut gaps: Vec<(String, usize)> = header_file_count
        .into_iter()
        .filter(|(h, count)| *count >= 3 && !synonym_set.contains(h.as_str()))
        .collect();
    gaps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut md = String::new();
    md.push_str("# Synonym Gaps\n\n");
    md.push_str(
        "Column names that appeared in 3+ files but are **not** in `synonyms.toml`.\n",
    );
    md.push_str("These are candidates to add as aliases.\n\n");
    md.push_str("| Column Header | Files Seen In |\n|---|---|\n");
    for (header, count) in &gaps {
        md.push_str(&format!("| `{header}` | {count} |\n"));
    }
    if gaps.is_empty() {
        md.push_str("| *(no gaps found)* | — |\n");
    }

    fs::write(reports_dir.join("synonym-improvements.md"), &md)
        .expect("should write synonym-improvements.md");
}

fn truncate_error(e: &str) -> String {
    // Collapse long error messages to their first sentence / ~80 chars.
    let s = e.split('.').next().unwrap_or(e).trim();
    if s.len() > 80 { s[..80].to_string() } else { s.to_string() }
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
    out.push_str("| filename | confidence | warnings | result |\n");
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
