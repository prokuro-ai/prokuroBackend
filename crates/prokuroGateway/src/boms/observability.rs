//! Shared ops markers for BOM write failure visibility.
//!
//! `BOM_WRITE_FAILED_MARKER` must match CloudWatch's metric filter. The marker file
//! under `observability/` is the cross-repo source of truth (Rust + CDK both read it).

/// Log message / metric-filter token for BOM storage write failures.
pub const BOM_WRITE_FAILED_MARKER: &str = "bom_write_failed";

#[cfg(test)]
mod tests {
    use super::BOM_WRITE_FAILED_MARKER;
    use std::path::PathBuf;

    #[test]
    fn marker_file_matches_constant() {
        let from_file =
            include_str!("../../observability/bom_write_failed_marker.txt").trim();
        assert_eq!(
            from_file, BOM_WRITE_FAILED_MARKER,
            "marker file and Rust constant must stay identical"
        );
    }

    #[test]
    fn cdk_metric_filter_uses_shared_marker_constant() {
        let cdk_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../prokuroInfrastructureCDK/lib/constructs/bom-write-alarms.ts");
        let cdk = std::fs::read_to_string(&cdk_path).unwrap_or_else(|error| {
            panic!(
                "failed to read CDK alarm construct at {}: {error}",
                cdk_path.display()
            )
        });
        assert!(
            cdk.contains("BOM_WRITE_FAILED_MARKER"),
            "CDK must reference BOM_WRITE_FAILED_MARKER so it cannot drift from the Rust/log marker"
        );
        assert!(
            cdk.contains("FilterPattern.literal(BOM_WRITE_FAILED_MARKER)"),
            "CDK metric filter must use FilterPattern.literal(BOM_WRITE_FAILED_MARKER)"
        );

        let observability_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../../prokuroInfrastructureCDK/lib/observability.ts",
        );
        let observability = std::fs::read_to_string(&observability_path).unwrap_or_else(|error| {
            panic!(
                "failed to read CDK observability module at {}: {error}",
                observability_path.display()
            )
        });
        assert!(
            observability.contains("bom_write_failed_marker.txt"),
            "CDK observability module must load the shared marker file"
        );
    }
}
