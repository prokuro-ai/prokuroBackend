//! BIS Entity List manufacturer name normalization and lookup helpers.

const ENTITY_SUFFIXES: &[&str] = &[
    "incorporated",
    "corporation",
    "company",
    "limited",
    "holding",
    "holdings",
    "group",
    "international",
    "technology",
    "technologies",
    "electronics",
    "industries",
    "industrial",
    "gmbh",
    "corp",
    "inc",
    "ltd",
    "llc",
    "co",
    "plc",
    "nv",
    "sa",
    "ag",
];

pub fn normalize_party_name(name: &str) -> String {
    let lowered = name.trim().to_lowercase();
    let mut cleaned = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
            cleaned.push(ch);
        } else if ch != '.' {
            cleaned.push(' ');
        }
    }
    let mut tokens: Vec<&str> = cleaned.split_whitespace().collect();
    while let Some(last) = tokens.last() {
        if ENTITY_SUFFIXES.contains(last) {
            tokens.pop();
        } else {
            break;
        }
    }
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::normalize_party_name;

    #[test]
    fn strips_punctuation_and_suffixes() {
        assert_eq!(
            normalize_party_name("Huawei Technologies Co., Ltd."),
            "huawei"
        );
    }

    #[test]
    fn murata_does_not_match_huawei() {
        assert_ne!(
            normalize_party_name("Murata"),
            normalize_party_name("Huawei")
        );
    }

    #[test]
    fn alt_name_normalizes_consistently() {
        assert_eq!(
            normalize_party_name("STMicroelectronics N.V."),
            normalize_party_name("STMicroelectronics NV")
        );
    }
}
