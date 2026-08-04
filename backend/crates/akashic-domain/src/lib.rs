pub mod algos; // pure algorithms (filled by A1 Task 2)
pub mod error; // DomainError — typed service-boundary error
pub mod ports; // repo + service port traits (filled by later A1 tasks)
pub mod types; // domain value types ports return

pub use error::{DomainError, DomainResult};
