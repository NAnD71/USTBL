//! Lightweight, append-only history for the actual game's `config/` directory.
//!
//! The scanner deliberately does not model an attacker. It does not detect a path
//! being replaced between metadata and read, an ancestor replacement, or an
//! equal-sized/equal-mtime replacement. The directory belongs to the player, so
//! identity checks, canonicalization, WinAPI, directory signatures, and cross-process
//! locks would add complexity without protecting the intended use case.

#![allow(dead_code, unused_imports)]

mod model;
mod scan;
mod semantic;
mod store;

pub use model::*;
pub use semantic::{diff_trees, file_diff, semantics};
pub use store::ConfigHistory;
