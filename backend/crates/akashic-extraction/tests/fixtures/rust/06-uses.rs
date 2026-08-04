use std::collections::HashMap;
use crate::ingestion::analyzer::Language;
use super::helpers;
use serde::{Serialize, Deserialize};
use std::io::Result as IoResult;
use std::fmt::*;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};
