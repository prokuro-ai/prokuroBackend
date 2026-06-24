#[tokio::test]
async fn minimal_csv_parses_correctly() {
    let bytes = include_bytes!("fixtures/minimal.csv");
    let result = prokuro_parser::pipeline::parse_file(bytes, "minimal.csv").await.unwrap();
    assert_eq!(result.stats.parsed_rows, 2);
    assert!(result.mapping_confidence >= 0.7);
    assert!(result.lines.iter().all(|l| l.mpn.is_some()));
}

#[tokio::test]
async fn minimal_xlsx_selects_bom_sheet() {
    let bytes = include_bytes!("fixtures/minimal.xlsx");
    let result = prokuro_parser::pipeline::parse_file(bytes, "minimal.xlsx").await.unwrap();
    assert_eq!(result.sheet_name.as_deref(), Some("BOM"));
    assert_eq!(result.stats.parsed_rows, 2);
}
