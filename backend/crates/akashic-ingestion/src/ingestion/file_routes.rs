//! Re-export: file-based routing moved to `akashic-domain::algos::file_routes` (A1 Task 2).
//!
//! All call sites importing from this module continue to compile unchanged.

pub use akashic_domain::algos::file_routes::{FileRoute, file_based_route};
