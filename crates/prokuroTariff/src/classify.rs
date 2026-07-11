//! Keyword-based HTS classification with honest confidence levels.
//!
//! Never invents an HTS code. Unmatched inputs return [`ClassificationConfidence::Unclassified`].
//! Matching is whole-token (word-boundary), not raw substring search.

use serde::Serialize;

const LOW_CONFIDENCE_SURROUNDINGS_NOTE: &str =
    "Low confidence: surrounding text does not resemble a standard component description — manual review recommended.";

/// Tokens that look like electronics BOM vocabulary (units, packages, etc.).
/// Used only for the surroundings sanity check — not for classification.
const ELECTRONICS_DOMAIN_HINTS: &[&str] = &[
    "ohm", "ohms", "farad", "farads", "henry", "henries", "volt", "volts", "amp", "amps",
    "ampere", "amperes", "watt", "watts", "hz", "mhz", "ghz", "uf", "nf", "pf", "mv", "kv",
    "ma", "ua", "smd", "tht", "smt", "dip", "sip", "sot", "soic", "qfn", "qfp", "bga", "tssop",
    "msop", "dfn", "lga", "0402", "0603", "0805", "1206", "1210", "2512", "mfr", "mpn", "sku",
    "qty", "refdes", "bom", "pcb", "ic", "led", "rgb", "arm", "cortex", "cell", "pack", "mm",
    "awg", "vdc", "vac", "rms", "tol", "tolerance", "temp", "nch", "pch", "npn", "pnp",
];

/// Ordered most-specific-first. First match wins.
const KEYWORD_RULES: &[KeywordRule] = &[
    // Batteries — lithium-ion before generic lithium/battery
    KeywordRule {
        terms: &["li-ion", "li ion", "lithium-ion", "lithium ion", "lipo", "li-po"],
        hts_code: "8507.60.00",
        label: "lithium-ion battery",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["lithium"],
        hts_code: "8506.50.00",
        label: "lithium primary cell",
        confidence: ClassificationConfidence::High,
    },
    // Semiconductors — specific before generic
    KeywordRule {
        terms: &["mosfet", "n-ch", "p-ch", "n-channel", "p-channel"],
        hts_code: "8541.21.00",
        label: "MOSFET transistor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &[
            "microcontroller",
            "mcu",
            "processor",
            "cortex",
            "stm32",
            "cpu",
            "soc",
        ],
        hts_code: "8542.31.00",
        label: "processor / microcontroller IC",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &[
            "dram",
            "sdram",
            "ddr",
            "flash",
            "eeprom",
            "sram",
            "memory",
            "nand",
            "nor flash",
        ],
        hts_code: "8542.32.00",
        label: "memory IC",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["op-amp", "opamp", "amplifier ic", "audio amp ic"],
        hts_code: "8542.33.00",
        label: "amplifier IC",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["led"],
        hts_code: "8541.41.00",
        label: "LED",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["photovoltaic", "solar cell", "solar panel"],
        hts_code: "8541.42.00",
        label: "photovoltaic cell",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["thyristor", "triac", "diac"],
        hts_code: "8541.30.00",
        label: "thyristor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["diode", "schottky", "zener", "rectifier"],
        hts_code: "8541.10.00",
        label: "diode",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["transistor", "bjt"],
        hts_code: "8541.29.00",
        label: "transistor",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["crystal", "xtal", "oscillator", "piezo"],
        hts_code: "8541.60.00",
        label: "mounted piezoelectric crystal",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["integrated circuit", "ic", "asic", "fpga"],
        hts_code: "8542.39.00",
        label: "other integrated circuit",
        confidence: ClassificationConfidence::Medium,
    },
    // Capacitors — dielectric-specific before generic
    KeywordRule {
        terms: &["tantalum"],
        hts_code: "8532.21.00",
        label: "tantalum capacitor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &[
            "electrolytic",
            "aluminum electrolytic",
            "aluminium electrolytic",
        ],
        hts_code: "8532.22.00",
        label: "aluminum electrolytic capacitor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["ceramic", "x7r", "x5r", "c0g", "np0", "mlcc", "cap cer"],
        hts_code: "8532.24.00",
        label: "ceramic capacitor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &[
            "film capacitor",
            "polyester capacitor",
            "polypropylene capacitor",
        ],
        hts_code: "8532.25.00",
        label: "film capacitor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["capacitor", "cap"],
        hts_code: "8532.24.00",
        label: "capacitor",
        confidence: ClassificationConfidence::Medium,
    },
    // Resistors
    KeywordRule {
        terms: &["carbon resistor"],
        hts_code: "8533.10.00",
        label: "carbon resistor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["wirewound", "wire-wound"],
        hts_code: "8533.31.00",
        label: "wirewound resistor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["potentiometer", "rheostat", "varistor", "ntc", "ptc"],
        hts_code: "8533.40.00",
        label: "variable resistor",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["resistor", "res"],
        hts_code: "8533.21.00",
        label: "resistor",
        confidence: ClassificationConfidence::Medium,
    },
    // Magnetics / electromechanical
    KeywordRule {
        terms: &["transformer"],
        hts_code: "8504.31.40",
        label: "transformer",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["inductor", "choke", "ferrite bead"],
        hts_code: "8504.50.80",
        label: "inductor",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["relay"],
        hts_code: "8536.41.00",
        label: "relay",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["switch"],
        hts_code: "8536.50.90",
        label: "switch",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["connector", "header", "socket", "plug", "jack"],
        hts_code: "8536.69.80",
        label: "connector",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["pcb", "printed circuit", "circuit board"],
        hts_code: "8534.00.00",
        label: "printed circuit board",
        confidence: ClassificationConfidence::High,
    },
    KeywordRule {
        terms: &["cable", "wire harness", "ribbon cable"],
        hts_code: "8544.42.90",
        label: "cable with connectors",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["battery"],
        hts_code: "8506.50.00",
        label: "battery",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["loudspeaker", "speaker", "microphone"],
        hts_code: "8518.21.00",
        label: "audio transducer",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["headphone", "earphone", "headset"],
        hts_code: "8518.30.20",
        label: "headphones",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["antenna", "aerial"],
        hts_code: "8517.71.00",
        label: "antenna",
        confidence: ClassificationConfidence::Medium,
    },
    KeywordRule {
        terms: &["thermostat", "controller", "regulator"],
        hts_code: "9032.89.60",
        label: "control instrument",
        confidence: ClassificationConfidence::Low,
    },
];

struct KeywordRule {
    terms: &'static [&'static str],
    hts_code: &'static str,
    label: &'static str,
    confidence: ClassificationConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassificationConfidence {
    High,
    Medium,
    Low,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub hts_code: Option<String>,
    pub matched_term: Option<String>,
    pub confidence: ClassificationConfidence,
    pub review_note: Option<String>,
}

/// Split on non-alphanumeric characters and lowercase.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn tokens_contain_phrase(tokens: &[String], phrase: &str) -> bool {
    let phrase_tokens = tokenize(phrase);
    if phrase_tokens.is_empty() || phrase_tokens.len() > tokens.len() {
        return false;
    }
    tokens
        .windows(phrase_tokens.len())
        .any(|window| window == phrase_tokens.as_slice())
}

fn is_manufacturer_style_code(token: &str) -> bool {
    let has_alpha = token.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    has_alpha && has_digit
}

fn is_value_with_unit_suffix(token: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "v", "mv", "kv", "a", "ma", "ua", "w", "mw", "ohm", "uf", "nf", "pf", "hz", "mhz", "ghz",
        "mm", "awg",
    ];
    SUFFIXES.iter().any(|suffix| {
        token.len() > suffix.len()
            && token.ends_with(suffix)
            && token[..token.len() - suffix.len()]
                .chars()
                .all(|c| c.is_ascii_digit())
    })
}

fn is_recognized_token(token: &str) -> bool {
    if ELECTRONICS_DOMAIN_HINTS.contains(&token) {
        return true;
    }
    if is_manufacturer_style_code(token) || is_value_with_unit_suffix(token) {
        return true;
    }
    KEYWORD_RULES.iter().any(|rule| {
        rule.terms
            .iter()
            .any(|term| tokenize(term).iter().any(|part| part == token))
    })
}

fn surroundings_look_non_electronic(tokens: &[String]) -> bool {
    // Need enough tokens to judge (e.g. lone "capacitor" must stay Medium).
    if tokens.len() < 4 {
        return false;
    }
    let unrecognized = tokens
        .iter()
        .filter(|token| !is_recognized_token(token))
        .count();
    unrecognized >= 3
}

/// Classify a component from free-text description and optional category.
///
/// Matching is case-insensitive whole-token matching against `description` + optional `category`.
/// Returns [`ClassificationConfidence::Unclassified`] with `hts_code = None` when no rule matches.
pub fn classify_component(description: &str, category: Option<&str>) -> Classification {
    let haystack = match category {
        Some(category) if !category.trim().is_empty() => {
            format!("{} {}", description, category)
        }
        _ => description.to_string(),
    };
    let tokens = tokenize(&haystack);

    let description_empty = description.trim().is_empty();
    let category_only = description_empty && category.is_some_and(|c| !c.trim().is_empty());

    for rule in KEYWORD_RULES {
        if !rule
            .terms
            .iter()
            .any(|term| tokens_contain_phrase(&tokens, term))
        {
            continue;
        }

        let mut confidence = if category_only {
            ClassificationConfidence::Low
        } else {
            rule.confidence
        };
        let mut review_note = None;

        if surroundings_look_non_electronic(&tokens) {
            confidence = ClassificationConfidence::Low;
            review_note = Some(LOW_CONFIDENCE_SURROUNDINGS_NOTE.to_string());
        }

        return Classification {
            hts_code: Some(rule.hts_code.to_string()),
            matched_term: Some(rule.label.to_string()),
            confidence,
            review_note,
        };
    }

    Classification {
        hts_code: None,
        matched_term: None,
        confidence: ClassificationConfidence::Unclassified,
        review_note: None,
    }
}

/// HTS codes referenced by the keyword table (for data-integrity tests).
pub fn keyword_hts_codes() -> Vec<&'static str> {
    KEYWORD_RULES
        .iter()
        .map(|rule| rule.hts_code)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ClassificationConfidence, LOW_CONFIDENCE_SURROUNDINGS_NOTE, classify_component,
    };

    #[test]
    fn ceramic_capacitor_description_classifies_high() {
        let result = classify_component("CAP CER 0.1UF 50V X7R 0402", None);
        assert_eq!(result.hts_code.as_deref(), Some("8532.24.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
        assert_eq!(result.matched_term.as_deref(), Some("ceramic capacitor"));
        assert_eq!(result.review_note, None);
    }

    #[test]
    fn tantalum_capacitor_classifies_high() {
        let result = classify_component("Tantalum capacitor 10uF", None);
        assert_eq!(result.hts_code.as_deref(), Some("8532.21.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
    }

    #[test]
    fn generic_capacitor_classifies_medium() {
        let result = classify_component("capacitor", None);
        assert_eq!(result.hts_code.as_deref(), Some("8532.24.00"));
        assert_eq!(result.confidence, ClassificationConfidence::Medium);
        assert_eq!(result.matched_term.as_deref(), Some("capacitor"));
        assert_eq!(result.review_note, None);
    }

    #[test]
    fn mosfet_classifies_as_specific_transistor_not_generic() {
        let result = classify_component("MOSFET N-CH 20V 4.1A SOT-23", None);
        assert_eq!(result.hts_code.as_deref(), Some("8541.21.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
        assert_ne!(result.hts_code.as_deref(), Some("8541.29.00"));
    }

    #[test]
    fn mcu_classifies_as_processor_ic() {
        let result = classify_component("STM32F407 MCU ARM Cortex-M4", None);
        assert_eq!(result.hts_code.as_deref(), Some("8542.31.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
    }

    #[test]
    fn dram_classifies_as_memory_ic() {
        let result = classify_component("DDR4 SDRAM 8Gb", None);
        assert_eq!(result.hts_code.as_deref(), Some("8542.32.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
    }

    #[test]
    fn led_classifies_high() {
        let result = classify_component("LED RGB CLEAR 4SMD", None);
        assert_eq!(result.hts_code.as_deref(), Some("8541.41.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
    }

    #[test]
    fn li_ion_battery_classifies_as_accumulator_not_primary_cell() {
        let result = classify_component("18650 li-ion battery cell", None);
        assert_eq!(result.hts_code.as_deref(), Some("8507.60.00"));
        assert_eq!(result.confidence, ClassificationConfidence::High);
        assert_ne!(result.hts_code.as_deref(), Some("8506.50.00"));
    }

    #[test]
    fn connector_header_classifies() {
        let result = classify_component("connector header 2.54mm", None);
        assert_eq!(result.hts_code.as_deref(), Some("8536.69.80"));
        assert!(matches!(
            result.confidence,
            ClassificationConfidence::High | ClassificationConfidence::Medium
        ));
    }

    #[test]
    fn gibberish_is_unclassified_with_no_hts_code() {
        let result = classify_component("XQ-99 FLUX WIDGET", None);
        assert_eq!(result.hts_code, None);
        assert_eq!(result.matched_term, None);
        assert_eq!(result.confidence, ClassificationConfidence::Unclassified);
    }

    #[test]
    fn flux_capacitor_phrase_matches_but_downgrades_to_low() {
        let result = classify_component("flux capacitor widget quantum", None);
        assert_eq!(result.hts_code.as_deref(), Some("8532.24.00"));
        assert_eq!(result.confidence, ClassificationConfidence::Low);
        assert_eq!(
            result.review_note.as_deref(),
            Some(LOW_CONFIDENCE_SURROUNDINGS_NOTE)
        );
    }

    #[test]
    fn capacitor_alone_stays_medium_without_downgrade() {
        let result = classify_component("capacitor", None);
        assert_eq!(result.confidence, ClassificationConfidence::Medium);
        assert_eq!(result.review_note, None);
    }
}
