//! `astrarium manifest`.

use std::path::Path;

use astrarium_core::Manifest;
use miette::{IntoDiagnostic, Result};

/// Validates a manifest and all paths it references.
pub fn validate(path: &Path) -> Result<()> {
    let manifest = Manifest::load(path).into_diagnostic()?;
    manifest.validate_files().into_diagnostic()?;
    println!(
        "valid: {} ({} apps, {} projects, {} namespaces)",
        path.display(),
        manifest.apps.len(),
        manifest.projects.len(),
        manifest.namespaces.len()
    );
    Ok(())
}

/// Prints the JSON schema for `astrarium.toml`.
pub fn schema() -> Result<()> {
    let schema = schemars::schema_for!(Manifest);
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).into_diagnostic()?
    );
    Ok(())
}
