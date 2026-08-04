//! Re-export: RRF algorithms moved to `akashic-domain::algos::rrf` (A1 Task 2).
//!
//! All call sites importing from this module continue to compile unchanged.

pub use akashic_domain::algos::rrf::{
    FusedItem, RankedItem, RankedItemMeta, cross_space_merge, multi_source_rrf, weighted_rrf,
};
