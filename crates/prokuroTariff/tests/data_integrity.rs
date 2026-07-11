use prokuro_tariff::classify::keyword_hts_codes;
use prokuro_tariff::data::TariffData;

#[test]
fn every_keyword_hts_code_exists_in_hts_electronics_json() {
    let data = TariffData::load().expect("data must load");
    for code in keyword_hts_codes() {
        assert!(
            data.find_hts(code).is_some(),
            "keyword table references {code} which is missing from hts_electronics.json"
        );
    }
}

#[test]
fn every_hts_entry_has_duty_rate_and_revision() {
    let data = TariffData::load().expect("data must load");
    for entry in &data.hts {
        assert!(
            entry.general_duty_rate_pct.is_finite(),
            "{} has non-finite duty rate",
            entry.hts_code
        );
        // f64 is always "present"; ensure revision is non-empty (rate may legitimately be 0.0 / Free)
        assert!(
            !entry.hts_revision.trim().is_empty(),
            "{} missing hts_revision",
            entry.hts_code
        );
        assert_eq!(entry.source, "USITC HTS");
    }
}

#[test]
fn section_301_prefixes_are_in_chapter_84_85_or_90() {
    let data = TariffData::load().expect("data must load");
    for entry in &data.section_301 {
        let digits: String = entry
            .hts_prefix
            .chars()
            .filter(|c| c.is_ascii_digit())
            .take(2)
            .collect();
        assert!(
            matches!(digits.as_str(), "84" | "85" | "90"),
            "prefix {} is outside chapters 84/85/90",
            entry.hts_prefix
        );
        assert!(entry.hts_prefix.len() >= 4, "prefix too short: {}", entry.hts_prefix);
        assert_eq!(entry.source, "USTR");
        assert!(!entry.retrieved.is_empty());
    }
}
