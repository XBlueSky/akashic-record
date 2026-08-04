//! Re-export: impact algorithms moved to `akashic-domain::algos::impact` (A1 Task 2).
//!
//! All call sites importing from this module continue to compile unchanged.

pub use akashic_domain::algos::impact::{
    RawImpactEdge, aggregate_impacts, format_impact, path_score, risk_level,
};
