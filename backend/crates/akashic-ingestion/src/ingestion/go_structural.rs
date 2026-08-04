//! Re-export: Go structural implements moved to `akashic-domain::algos::go_structural` (A1 Task 2).
//!
//! All call sites importing from this module continue to compile unchanged.

pub use akashic_domain::algos::go_structural::{
    go_interface_methods, go_method_receiver, go_structural_implements, go_type_is_interface,
};
