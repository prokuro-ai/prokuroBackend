//! v1.1 plan entitlements (Prokuro-Subscription-Pricing-v1.1).
//! Refresh cadence and Bedrock model are metadata for clients; enforcement is numeric caps.

use prokuro_types::purchasing::{BillingPlan, PlanLimits, PlanUsage, RefreshCadence};

pub fn limits_for(plan: BillingPlan) -> PlanLimits {
    match plan {
        BillingPlan::Free => PlanLimits {
            seats: 1,
            active_boms: 1,
            max_lines_per_bom: 100,
            lines_per_month: 300,
            analyses_per_month: 3,
            purchasing_actions_per_month: 5,
            orders_per_month: 2,
            concurrent_analyses: 1,
            unique_mpn_lookups_per_day: 200,
            refresh: RefreshCadence::Weekly,
            bedrock: "haiku_capped".into(),
        },
        BillingPlan::Growth => PlanLimits {
            seats: 2,
            active_boms: 10,
            max_lines_per_bom: 500,
            lines_per_month: 2_500,
            analyses_per_month: 20,
            purchasing_actions_per_month: 40,
            orders_per_month: 10,
            concurrent_analyses: 1,
            unique_mpn_lookups_per_day: 1_000,
            refresh: RefreshCadence::Daily,
            bedrock: "on".into(),
        },
        BillingPlan::Scale => PlanLimits {
            seats: 5,
            active_boms: 50,
            max_lines_per_bom: 2_000,
            lines_per_month: 15_000,
            analyses_per_month: 100,
            purchasing_actions_per_month: 200,
            orders_per_month: 50,
            concurrent_analyses: 3,
            unique_mpn_lookups_per_day: 5_000,
            refresh: RefreshCadence::Daily,
            bedrock: "on".into(),
        },
    }
}

pub fn empty_usage() -> PlanUsage {
    PlanUsage {
        analyses_count: 0,
        lines_count: 0,
        purchasing_actions_count: 0,
        orders_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_allows_small_purchasing_pool() {
        let free = limits_for(BillingPlan::Free);
        assert_eq!(free.purchasing_actions_per_month, 5);
        assert_eq!(free.orders_per_month, 2);
        assert_eq!(free.active_boms, 1);
        assert!(matches!(free.refresh, RefreshCadence::Weekly));
    }
}
