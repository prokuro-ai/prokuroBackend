//! Shared ops markers for BOM write failure visibility.
//!
//! The marker file under `observability/` is the source of truth for log greps.

/// Log token for BOM storage write failures.
pub const BOM_WRITE_FAILED_MARKER: &str = "bom_write_failed";

#[cfg(test)]
mod tests {
    use super::BOM_WRITE_FAILED_MARKER;

    #[test]
    fn marker_file_matches_constant() {
        let from_file = include_str!("../../observability/bom_write_failed_marker.txt").trim();
        assert_eq!(
            from_file, BOM_WRITE_FAILED_MARKER,
            "marker file and Rust constant must stay identical"
        );
    }
}
