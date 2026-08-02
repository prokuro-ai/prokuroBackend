//! Per-line tariff exposure

use chrono::NaiveDate;
use serde::Serialize;

use crate::classify::{classify_component, ClassificationConfidence};
use crate::data::{EntityListEntry, TariffData};
use crate::trade_programs::program_for_country;

const NOTE_MANUAL_REVIEW: &str = "Could not classify — manual HTS review recommended";
pub const DISCLAIMER: &str = "Estimated for planning purposes only. Not a customs broker classification. Verify with a licensed broker before filing.";
const STALE_DISCLAIMER_SUFFIX: &str =
    " Tariff data is due for review and may not reflect the most current rates.";
pub const CSL_DISCLAIMER_SUFFIX: &str =
    " Potential Entity List match from ITA Consolidated Screening List — verify with official BIS publications before proceeding.";

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TariffInput {
    pub mpn: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub country_of_origin: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DataSources {
    pub hts_revision: String,
    pub section_301_retrieved: String,
    pub hts_data_age_days: i64,
    pub section_301_data_age_days: i64,
    pub is_stale: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TariffLineResult {
    pub mpn: String,
    pub hts_code: Option<String>,
    pub classification: Option<String>,
    pub confidence: ClassificationConfidence,
    pub base_duty_pct: Option<f64>,
    pub section_301_pct: Option<f64>,
    pub total_duty_pct: Option<f64>,
    pub rate_basis: String,
    pub estimated: bool,
    pub notes: Option<String>,
    pub entity_list_match: bool,
    pub entity_list_matched_name: Option<String>,
    pub entity_list_notes: Option<String>,
    pub data_sources: DataSources,
    pub disclaimer: String,
}

pub fn assess_lines(data: &TariffData, inputs: &[TariffInput]) -> Vec<TariffLineResult> {
    let today = chrono::Utc::now().date_naive();
    inputs
        .iter()
        .map(|input| assess_one(data, input, today))
        .collect()
}

fn assess_one(data: &TariffData, input: &TariffInput, today: NaiveDate) -> TariffLineResult {
    let data_sources = build_data_sources(data, today);
    let entity_match = entity_list_outcome(data, input.manufacturer.as_deref());
    let entity_list_match = entity_match.is_some();
    let entity_list_matched_name = entity_match.map(|entry| entry.name.clone());
    let entity_list_notes = entity_match.as_ref().map(|entry| entity_list_note(entry));
    let disclaimer = build_disclaimer(data_sources.is_stale, entity_list_match);

    let classification = classify_component(
        input.description.as_deref().unwrap_or(""),
        input.category.as_deref(),
    );

    if classification.confidence == ClassificationConfidence::Unclassified
        || classification.hts_code.is_none()
    {
        return TariffLineResult {
            mpn: input.mpn.clone(),
            hts_code: None,
            classification: None,
            confidence: ClassificationConfidence::Unclassified,
            base_duty_pct: None,
            section_301_pct: None,
            total_duty_pct: None,
            rate_basis: "unknown_origin".into(),
            estimated: true,
            notes: merge_notes(Some(NOTE_MANUAL_REVIEW.into()), entity_list_notes.clone()),
            entity_list_match,
            entity_list_matched_name,
            entity_list_notes,
            data_sources,
            disclaimer,
        };
    }

    let hts_code = classification.hts_code.expect("handled above");
    let general_rate = data
        .find_hts_base(&hts_code)
        .map(|entry| entry.general_duty_rate_pct);
    let (base_duty_pct, rate_basis) = resolve_base_duty(
        data,
        &hts_code,
        input.country_of_origin.as_deref(),
        general_rate,
    );

    let section_301_entry = data.find_section_301_addon(&hts_code);
    let section_232_entry = data.find_section_232_addon(&hts_code);
    let section_232_pct = section_232_entry.map(|entry| entry.additional_rate_pct);
    let section_301_excluded = data.find_exclusion(&hts_code, today).is_some();

    let (section_301_pct, mut note_parts) = section_301_outcome(
        input.country_of_origin.as_deref(),
        section_301_entry,
        section_301_excluded,
    );

    if let Some(entry) = section_232_entry {
        note_parts.push(format!(
            "Section 232 semiconductor tariff +{}% ({})",
            entry.additional_rate_pct, entry.ch99_subheading
        ));
    }
    if let Some(review) = classification.review_note {
        note_parts.push(review);
    }
    if let Some(entry) = entity_match {
        note_parts.push(entity_list_note(entry));
    }

    let origin_known = input
        .country_of_origin
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let total_duty_pct = compute_total(
        base_duty_pct,
        section_301_pct,
        section_232_pct,
        origin_known,
        section_301_entry.is_some(),
        section_301_excluded,
    );

    TariffLineResult {
        mpn: input.mpn.clone(),
        hts_code: Some(hts_code),
        classification: classification.matched_term,
        confidence: classification.confidence,
        base_duty_pct,
        section_301_pct,
        total_duty_pct,
        rate_basis,
        estimated: true,
        notes: join_notes(note_parts),
        entity_list_match,
        entity_list_matched_name,
        entity_list_notes,
        data_sources,
        disclaimer,
    }
}

fn entity_list_outcome<'a>(
    data: &'a TariffData,
    manufacturer: Option<&str>,
) -> Option<&'a EntityListEntry> {
    manufacturer
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|manufacturer| data.find_entity_list_match(manufacturer))
}

fn entity_list_note(entry: &EntityListEntry) -> String {
    let notice = entry
        .federal_register_notice
        .as_deref()
        .unwrap_or("official BIS notice");
    format!("BIS Entity List match: {} ({notice})", entry.name)
}

fn build_disclaimer(is_stale: bool, entity_list_match: bool) -> String {
    let mut disclaimer = if is_stale {
        format!("{DISCLAIMER}{STALE_DISCLAIMER_SUFFIX}")
    } else {
        DISCLAIMER.to_string()
    };
    if entity_list_match {
        disclaimer.push_str(CSL_DISCLAIMER_SUFFIX);
    }
    disclaimer
}

fn merge_notes(primary: Option<String>, secondary: Option<String>) -> Option<String> {
    match (primary, secondary) {
        (Some(left), Some(right)) => Some(format!("{left}; {right}")),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn resolve_base_duty(
    data: &TariffData,
    hts_code: &str,
    country_of_origin: Option<&str>,
    general_rate: Option<f64>,
) -> (Option<f64>, String) {
    let Some(origin) = country_of_origin
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (general_rate, "general".into());
    };

    if let Some(program) = program_for_country(origin) {
        if let Some(rate) = data.find_special_rate(hts_code, program) {
            return (Some(rate), format!("special:{program}"));
        }
    }

    (general_rate, "general".into())
}

fn section_301_outcome(
    origin: Option<&str>,
    entry: Option<&crate::data::Chapter99Addon>,
    excluded: bool,
) -> (Option<f64>, Vec<String>) {
    let Some(entry) = entry else {
        return (None, Vec::new());
    };

    if excluded {
        return (
            None,
            vec!["Active Section 301 exclusion applies — additional 301 duty not estimated".into()],
        );
    }

    match origin.map(str::trim).filter(|value| !value.is_empty()) {
        Some(origin) if is_china_origin(origin) => (
            Some(entry.additional_rate_pct),
            vec![format!(
                "Section 301 {} applies (China origin)",
                entry.list.as_deref().unwrap_or("list")
            )],
        ),
        Some(_) => (None, Vec::new()),
        None => (
            None,
            vec![format!(
                "Country of origin unknown — if China-sourced, +{}% Section 301 exposure",
                entry.additional_rate_pct
            )],
        ),
    }
}

fn compute_total(
    base: Option<f64>,
    section_301: Option<f64>,
    section_232: Option<f64>,
    origin_known: bool,
    has_301_addon: bool,
    excluded: bool,
) -> Option<f64> {
    let base = base?;
    if !origin_known && has_301_addon && !excluded && section_301.is_none() {
        return section_232.map(|s232| base + s232);
    }
    Some(base + section_301.unwrap_or(0.0) + section_232.unwrap_or(0.0))
}

fn build_data_sources(data: &TariffData, today: NaiveDate) -> DataSources {
    DataSources {
        hts_revision: data.hts_revision(),
        section_301_retrieved: data.addons_meta.published_at.clone(),
        hts_data_age_days: dataset_age_days(&data.hts_meta.published_at, today),
        section_301_data_age_days: dataset_age_days(&data.addons_meta.published_at, today),
        is_stale: data.is_stale(today),
    }
}

fn dataset_age_days(published_at: &str, today: NaiveDate) -> i64 {
    NaiveDate::parse_from_str(&published_at[..10.min(published_at.len())], "%Y-%m-%d")
        .map(|published| (today - published).num_days())
        .unwrap_or(0)
}

fn join_notes(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn is_china_origin(country: &str) -> bool {
    matches!(
        country.trim().to_lowercase().as_str(),
        "cn" | "chn" | "china" | "prc"
    )
}

#[cfg(test)]
mod tests {
    use super::{assess_lines, TariffInput};
    use crate::classify::ClassificationConfidence;
    use crate::data::{
        Chapter99Addon, DatasetMeta, EntityListEntry, HtsBaseEntry, Section301Exclusion, TariffData,
    };

    fn tariff_data() -> TariffData {
        use crate::data::SnapshotFile;
        let entity = EntityListEntry {
            entity_id: "el-1".into(),
            name: "Huawei Technologies Co., Ltd.".into(),
            alt_names: vec!["Huawei".into()],
            federal_register_notice: Some("85 FR 00000".into()),
        };
        TariffData::from_parts(
            SnapshotFile {
                meta: meta("hts_base"),
                entries: vec![HtsBaseEntry {
                    hts_code: "8532.24.00".into(),
                    general_duty_rate_pct: 0.0,
                    special_rate_programs: vec![],
                }],
            },
            SnapshotFile {
                meta: meta("chapter99_addons"),
                entries: vec![Chapter99Addon {
                    hts_code: "8532".into(),
                    program: "section_301".into(),
                    additional_rate_pct: 25.0,
                    ch99_subheading: "9903.88.01".into(),
                    list: None,
                }],
            },
            SnapshotFile {
                meta: meta("section301_exclusions"),
                entries: vec![Section301Exclusion {
                    hts_code: "8532.24.00".into(),
                    expires_at: None,
                    status: "active".into(),
                }],
            },
            SnapshotFile {
                meta: meta("entity_list"),
                entries: vec![entity],
            },
        )
        .expect("tariff data")
    }

    fn meta(name: &str) -> DatasetMeta {
        DatasetMeta {
            published_at: "2026-07-10".into(),
            source: name.into(),
            source_revision: None,
            entry_count: 1,
            version: 1,
        }
    }

    #[test]
    fn flags_entity_list_manufacturer_match() {
        let results = assess_lines(
            &tariff_data(),
            &[TariffInput {
                mpn: "PART-1".into(),
                description: Some("CAP CER 0.1UF X7R".into()),
                category: None,
                country_of_origin: Some("CN".into()),
                manufacturer: Some("Huawei Technologies Co., Ltd.".into()),
            }],
        );
        assert!(results[0].entity_list_match);
        assert_eq!(
            results[0].entity_list_matched_name.as_deref(),
            Some("Huawei Technologies Co., Ltd.")
        );
    }

    #[test]
    fn unrelated_manufacturer_does_not_match_entity_list() {
        let results = assess_lines(
            &tariff_data(),
            &[TariffInput {
                mpn: "PART-1".into(),
                description: Some("CAP CER 0.1UF X7R".into()),
                category: None,
                country_of_origin: Some("CN".into()),
                manufacturer: Some("Murata".into()),
            }],
        );
        assert!(!results[0].entity_list_match);
    }

    #[test]
    fn entity_list_match_surfaces_on_unclassified_lines() {
        let results = assess_lines(
            &tariff_data(),
            &[TariffInput {
                mpn: "PART-1".into(),
                description: Some("XQ-99 FLUX WIDGET".into()),
                category: None,
                country_of_origin: None,
                manufacturer: Some("Huawei".into()),
            }],
        );
        assert_eq!(
            results[0].confidence,
            ClassificationConfidence::Unclassified
        );
        assert!(results[0].entity_list_match);
    }
}
