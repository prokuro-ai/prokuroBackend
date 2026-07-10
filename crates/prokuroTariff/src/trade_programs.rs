//! Country → HTS Column 1 Special program-letter mapping (USITC General Notes).
//!
//! Only programs that appear in our curated `special_rate_programs` entries are mapped.
//! Broad preference schemes (GSP `A`/`A*`, APTA `B`, etc.) are omitted — they cover many
//! countries and need case-by-case eligibility checks we do not attempt here.

/// Returns the HTS Special program code for a country of origin, if we map one.
///
/// Accepts ISO-ish codes and a few common names. Matching is case-insensitive.
pub fn program_for_country(country: &str) -> Option<&'static str> {
    let key = country.trim().to_uppercase();
    match key.as_str() {
        // USMCA — HTS Special letter "S"
        "MX" | "MEX" | "MEXICO" | "CA" | "CAN" | "CANADA" => Some("S"),
        // Bilateral / regional FTAs whose program letter matches the country code
        "AU" | "AUS" | "AUSTRALIA" => Some("AU"),
        "BH" | "BHR" | "BAHRAIN" => Some("BH"),
        "CL" | "CHL" | "CHILE" => Some("CL"),
        "CO" | "COL" | "COLOMBIA" => Some("CO"),
        "IL" | "ISR" | "ISRAEL" => Some("IL"),
        "JO" | "JOR" | "JORDAN" => Some("JO"),
        "KR" | "KOR" | "KOREA" | "SOUTH KOREA" | "REPUBLIC OF KOREA" => Some("KR"),
        "MA" | "MAR" | "MOROCCO" => Some("MA"),
        "OM" | "OMN" | "OMAN" => Some("OM"),
        "PA" | "PAN" | "PANAMA" => Some("PA"),
        "PE" | "PER" | "PERU" => Some("PE"),
        "SG" | "SGP" | "SINGAPORE" => Some("SG"),
        // DR-CAFTA — HTS Special letter "P"
        "CR" | "CRI" | "COSTA RICA"
        | "DO" | "DOM" | "DOMINICAN REPUBLIC"
        | "GT" | "GTM" | "GUATEMALA"
        | "HN" | "HND" | "HONDURAS"
        | "NI" | "NIC" | "NICARAGUA"
        | "SV" | "SLV" | "EL SALVADOR" => Some("P"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::program_for_country;

    #[test]
    fn mexico_and_canada_map_to_usmca_s() {
        assert_eq!(program_for_country("MX"), Some("S"));
        assert_eq!(program_for_country("mexico"), Some("S"));
        assert_eq!(program_for_country("CA"), Some("S"));
    }

    #[test]
    fn korea_maps_to_kr() {
        assert_eq!(program_for_country("KR"), Some("KR"));
        assert_eq!(program_for_country("South Korea"), Some("KR"));
    }

    #[test]
    fn china_has_no_special_program_mapping() {
        assert_eq!(program_for_country("CN"), None);
        assert_eq!(program_for_country("China"), None);
    }
}
