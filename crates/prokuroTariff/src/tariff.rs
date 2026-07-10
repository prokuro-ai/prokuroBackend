//! Build per-line tariff exposure responses from classification + official data.

use serde::Serialize;

use crate::classify::{ClassificationConfidence, classify_component};
use crate::data::TariffData;
use crate::trade_programs::program_for_country;

const NOTE_MANUAL_REVIEW: &str = "Could not classify — manual HTS review recommended";
const DISCLAIMER: &str = "Estimated for planning purposes only. Not a customs broker classification. Verify with a licensed broker before filing.";
const CHINA_ALIASES: &[&str] = &["cn", "chn", "china", "prc"];
const RATE_BASIS_GENERAL: &str = "general";
const RATE_BASIS_UNKNOWN_ORIGIN: &str = "unknown_origin";

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TariffInput {
    pub mpn: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub country_of_origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DataSources {
    pub hts_revision: String,
    pub section_301_retrieved: String,
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
    /// Which Column 1 rate was used: `general`, `special:<program>`, or `unknown_origin`.
    pub rate_basis: String,
    pub estimated: bool,
    pub notes: Option<String>,
    pub data_sources: DataSources,
    pub disclaimer: String,
}

pub fn is_china_origin(country: &str) -> bool {
    let normalized = country.trim().to_lowercase();
    CHINA_ALIASES.iter().any(|alias| *alias == normalized)
}

fn resolve_base_duty(
    data: &TariffData,
    hts_code: &str,
    country_of_origin: Option<&str>,
    general_rate: Option<f64>,
) -> (Option<f64>, String) {
    let Some(origin) = country_of_origin.map(str::trim).filter(|value| !value.is_empty()) else {
        return (general_rate, RATE_BASIS_GENERAL.to_string());
    };

    if let Some(program) = program_for_country(origin) {
        if let Some(rate) = data.find_special_rate(hts_code, program) {
            return (Some(rate), format!("special:{program}"));
        }
    }

    (general_rate, RATE_BASIS_GENERAL.to_string())
}

pub fn assess_line(data: &TariffData, input: &TariffInput) -> TariffLineResult {
    let description = input.description.as_deref().unwrap_or("");
    let classification = classify_component(description, input.category.as_deref());
    let data_sources = DataSources {
        hts_revision: data.hts_revision.clone(),
        section_301_retrieved: data.section_301_retrieved.clone(),
    };

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
            rate_basis: RATE_BASIS_UNKNOWN_ORIGIN.to_string(),
            estimated: true,
            notes: Some(NOTE_MANUAL_REVIEW.to_string()),
            data_sources,
            disclaimer: DISCLAIMER.to_string(),
        };
    }

    let hts_code = classification
        .hts_code
        .expect("unclassified path returned above");
    let hts_entry = data.find_hts(&hts_code);
    let general_rate = hts_entry.map(|entry| entry.general_duty_rate_pct);
    let (base_duty_pct, rate_basis) = resolve_base_duty(
        data,
        &hts_code,
        input.country_of_origin.as_deref(),
        general_rate,
    );
    let section_301_entry = data.find_section_301(&hts_code);

    let (section_301_pct, section_notes) = match (
        input.country_of_origin.as_deref(),
        section_301_entry,
    ) {
        (Some(origin), Some(entry)) if is_china_origin(origin) => (
            Some(entry.additional_rate_pct),
            Some(format!(
                "Section 301 {} applies (China origin)",
                entry.list
            )),
        ),
        (Some(_origin), _) => (None, None),
        (None, Some(entry)) => (
            None,
            Some(format!(
                "Country of origin unknown — if China-sourced, +{}% Section 301 exposure",
                entry.additional_rate_pct
            )),
        ),
        (None, None) => (None, None),
    };

    let notes = match (section_notes, classification.review_note) {
        (Some(section), Some(review)) => Some(format!("{section}; {review}")),
        (Some(section), None) => Some(section),
        (None, Some(review)) => Some(review),
        (None, None) => None,
    };

    let total_duty_pct = match (base_duty_pct, section_301_pct) {
        (Some(base), Some(section)) => Some(base + section),
        (Some(base), None) if input.country_of_origin.is_some() => Some(base),
        (Some(_base), None) if input.country_of_origin.is_none() && section_301_entry.is_some() => {
            // Origin missing on 301-covered part: do not present a total that implies zero 301 risk.
            None
        }
        (Some(base), None) => Some(base),
        _ => None,
    };

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
        notes,
        data_sources,
        disclaimer: DISCLAIMER.to_string(),
    }
}

pub fn assess_lines(data: &TariffData, inputs: &[TariffInput]) -> Vec<TariffLineResult> {
    inputs.iter().map(|input| assess_line(data, input)).collect()
}

#[cfg(test)]
mod tests {
    use super::{DISCLAIMER, TariffInput, assess_line, is_china_origin};
    use crate::classify::ClassificationConfidence;
    use crate::data::TariffData;

    fn data() -> TariffData {
        TariffData::load().expect("official data files must parse")
    }

    #[test]
    fn every_response_includes_disclaimer_including_unclassified() {
        let classified = assess_line(
            &data(),
            &TariffInput {
                mpn: "C0402".into(),
                description: Some("CAP CER 0.1UF X7R".into()),
                category: None,
                country_of_origin: Some("CN".into()),
            },
        );
        let unclassified = assess_line(
            &data(),
            &TariffInput {
                mpn: "XQ-99".into(),
                description: Some("XQ-99 FLUX WIDGET".into()),
                category: None,
                country_of_origin: None,
            },
        );
        assert_eq!(classified.disclaimer, DISCLAIMER);
        assert_eq!(unclassified.disclaimer, DISCLAIMER);
        assert_eq!(unclassified.confidence, ClassificationConfidence::Unclassified);
    }

    #[test]
    fn china_aliases_normalize() {
        assert!(is_china_origin("CN"));
        assert!(is_china_origin("PRC"));
        assert!(is_china_origin("china"));
        assert!(is_china_origin("CHN"));
        assert!(!is_china_origin("DE"));
        assert!(!is_china_origin("Germany"));
    }

    #[test]
    fn china_ceramic_capacitor_applies_section_301_and_names_list() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "C0402".into(),
                description: Some("CAP CER 0.1UF 50V X7R 0402".into()),
                category: None,
                country_of_origin: Some("CN".into()),
            },
        );

        assert_eq!(result.hts_code.as_deref(), Some("8532.24.00"));
        assert_eq!(result.base_duty_pct, Some(0.0));
        assert_eq!(result.section_301_pct, Some(25.0));
        assert_eq!(result.total_duty_pct, Some(25.0));
        assert_eq!(result.rate_basis, "general");
        assert!(result.estimated);
        let notes = result.notes.expect("notes should name the list");
        assert!(notes.contains("Section 301"));
        assert!(notes.contains("List 1"));
        assert!(notes.contains("China"));
    }

    #[test]
    fn prc_and_lowercase_china_both_trigger_section_301() {
        for origin in ["PRC", "china"] {
            let result = assess_line(
                &data(),
                &TariffInput {
                    mpn: "C0402".into(),
                    description: Some("CAP CER 0.1UF X7R".into()),
                    category: None,
                    country_of_origin: Some(origin.into()),
                },
            );
            assert_eq!(result.section_301_pct, Some(25.0), "origin={origin}");
        }
    }

    #[test]
    fn germany_origin_has_no_section_301_total_is_base_only() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "C0402".into(),
                description: Some("CAP CER 0.1UF X7R".into()),
                category: None,
                country_of_origin: Some("DE".into()),
            },
        );

        assert_eq!(result.section_301_pct, None);
        assert_eq!(result.base_duty_pct, Some(0.0));
        assert_eq!(result.total_duty_pct, Some(0.0));
        assert_eq!(result.rate_basis, "general");
        assert!(result.notes.is_none());
    }

    #[test]
    fn missing_origin_on_301_covered_part_warns_without_setting_section_301_pct() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "C0402".into(),
                description: Some("CAP CER 0.1UF X7R".into()),
                category: None,
                country_of_origin: None,
            },
        );

        assert_eq!(result.section_301_pct, None);
        assert_eq!(result.total_duty_pct, None);
        assert_eq!(result.rate_basis, "general");
        let notes = result.notes.expect("conditional exposure warning");
        assert!(notes.contains("Country of origin unknown"));
        assert!(notes.contains("Section 301"));
        assert!(notes.contains("25"));
    }

    #[test]
    fn unclassified_recommends_manual_review() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "XQ-99".into(),
                description: Some("XQ-99 FLUX WIDGET".into()),
                category: None,
                country_of_origin: Some("CN".into()),
            },
        );

        assert_eq!(result.hts_code, None);
        assert_eq!(result.confidence, ClassificationConfidence::Unclassified);
        assert_eq!(result.base_duty_pct, None);
        assert_eq!(result.section_301_pct, None);
        assert_eq!(result.total_duty_pct, None);
        assert_eq!(result.rate_basis, "unknown_origin");
        assert_eq!(
            result.notes.as_deref(),
            Some("Could not classify — manual HTS review recommended")
        );
    }

    #[test]
    fn mexico_origin_uses_special_free_rate_for_li_ion_battery() {
        // 8507.60.00: General 3.4%, Special Free including USMCA "S" (USITC 2025 Rev 32).
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "INR18650".into(),
                description: Some("18650 li-ion battery cell".into()),
                category: None,
                country_of_origin: Some("MX".into()),
            },
        );

        assert_eq!(result.hts_code.as_deref(), Some("8507.60.00"));
        assert_eq!(result.base_duty_pct, Some(0.0));
        assert_eq!(result.rate_basis, "special:S");
        assert_eq!(result.section_301_pct, None);
        assert_eq!(result.total_duty_pct, Some(0.0));
    }

    #[test]
    fn li_ion_battery_without_origin_falls_back_to_general_rate() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "INR18650".into(),
                description: Some("18650 li-ion battery cell".into()),
                category: None,
                country_of_origin: None,
            },
        );

        assert_eq!(result.hts_code.as_deref(), Some("8507.60.00"));
        assert_eq!(result.base_duty_pct, Some(3.4));
        assert_eq!(result.rate_basis, "general");
    }

    #[test]
    fn china_origin_on_li_ion_uses_general_not_special() {
        let result = assess_line(
            &data(),
            &TariffInput {
                mpn: "INR18650".into(),
                description: Some("18650 li-ion battery cell".into()),
                category: None,
                country_of_origin: Some("CN".into()),
            },
        );

        assert_eq!(result.hts_code.as_deref(), Some("8507.60.00"));
        assert_eq!(result.base_duty_pct, Some(3.4));
        assert_eq!(result.rate_basis, "general");
        assert_eq!(result.section_301_pct, Some(25.0));
        assert_eq!(result.total_duty_pct, Some(28.4));
    }
}
