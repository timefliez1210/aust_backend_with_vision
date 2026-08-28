//! Startup check for the fonts the KVA templates are laid out in.
//!
//! The XLSX template asks for Calibri. When no Calibri-compatible face is installed,
//! fontconfig silently substitutes a wider one, LibreOffice re-wraps the text boxes,
//! and the KVA ships with a broken terms page — a failure that is invisible in the
//! template and only shows up in the customer's PDF (prod, 2026-08-27).
//!
//! `fonts-crosextra-carlito` provides Carlito, which is metric-compatible with
//! Calibri; `docker/Dockerfile.backend` installs it. This module turns a missing
//! font from "spacing looks odd" into a loud log line at boot.

use std::process::Command;

/// Faces that carry Calibri's metrics, so the templates lay out as designed.
const CALIBRI_COMPATIBLE: &[&str] = &["Calibri", "Carlito"];

/// Ask fontconfig which family it resolves `Calibri` to.
///
/// **Caller**: [`check_template_fonts`]
/// **Why**: `fc-match` answers the same question LibreOffice asks when it renders
/// the template, so it is the substitution that actually matters.
///
/// # Returns
/// The resolved family name, or `None` when `fc-match` is missing or fails.
fn resolved_family(requested: &str) -> Option<String> {
    let output = Command::new("fc-match")
        .arg("-f")
        .arg("%{family}")
        .arg(requested)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let family = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if family.is_empty() {
        None
    } else {
        Some(family)
    }
}

/// Verify that the KVA templates will be rendered in Calibri metrics.
///
/// **Caller**: `main`, once at startup.
/// **Why**: see the module docs — a font substitution silently reflows every
/// generated KVA, so it is worth one process-start check.
///
/// # Returns
/// `Ok(family)` with the resolved family when it is Calibri-compatible,
/// `Err(message)` with a ready-to-log explanation otherwise. Never panics and
/// never blocks startup: a wrong font produces ugly PDFs, not a broken service.
pub fn check_template_fonts() -> Result<String, String> {
    match resolved_family("Calibri") {
        Some(family) => {
            // fc-match returns a comma-separated alias list for some faces.
            let primary = family.split(',').next().unwrap_or(&family).trim().to_string();
            if CALIBRI_COMPATIBLE.iter().any(|f| f.eq_ignore_ascii_case(&primary)) {
                Ok(primary)
            } else {
                Err(format!(
                    "Calibri resolves to '{primary}', which has different metrics. \
                     KVA terms pages will re-wrap. Install fonts-crosextra-carlito."
                ))
            }
        }
        None => Err(
            "fc-match is unavailable, so the KVA rendering font cannot be verified. \
             Install fontconfig and fonts-crosextra-carlito."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check must never take the service down, whatever fontconfig reports.
    #[test]
    fn check_returns_a_verdict_without_panicking() {
        match check_template_fonts() {
            Ok(family) => assert!(!family.is_empty()),
            Err(message) => assert!(message.contains("Carlito") || message.contains("carlito")),
        }
    }
}
