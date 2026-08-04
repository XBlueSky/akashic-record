//! D6 alerting subsystem.
//!
//! Reads metric values from the C5 Prometheus exposition every
//! `cfg.alerts.tick_secs`, evaluates the 5 canonical rules (R1-R5),
//! applies per-rule cooldown, and dispatches matched alerts to the
//! configured sink (incoming webhook or log fallback).

pub mod parser;
pub mod rules;
pub mod sink;
pub mod state;

pub mod evaluator;

pub use evaluator::start_evaluator;
