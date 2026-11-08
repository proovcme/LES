//! Compatibility entry point; selection and validation have one Rust owner.
use crate::config::{Failure, Result};
use std::path::{Path, PathBuf};
pub fn select(root: &Path, application: &Path, explicit: Option<PathBuf>) -> Result<PathBuf> {
    les_core::skills::runtime::select(root, application, explicit)
        .map_err(|error| Failure::new(error.code, error.message))
}
