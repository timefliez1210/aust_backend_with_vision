//! SOUL.md loader and section validator.
//!
//! Loads the assistant's persona file at startup and exposes each required section
//! so prompt assembly can pull individual pieces without re-parsing on every turn.
//! The loader errors clearly if a required section is absent.

use crate::error::{AssistantError, Result};

/// The five required top-level sections of SOUL.md.
const REQUIRED_SECTIONS: &[&str] = &[
    "Persona",
    "Hard Rules",
    "Domain Primer",
    "Tone",
    "Escalation",
];

/// Parsed and validated contents of SOUL.md.
#[derive(Debug, Clone)]
pub struct Soul {
    /// Who the assistant is and how it presents itself.
    pub persona: String,
    /// Non-negotiable behavioural constraints.
    pub hard_rules: String,
    /// Background knowledge about the moving-company domain.
    pub domain_primer: String,
    /// Communication style guidelines.
    pub tone: String,
    /// When and how to escalate to the owner.
    pub escalation: String,
}

impl Soul {
    /// Build a single system-prompt string by concatenating all sections.
    pub fn as_system_prompt(&self) -> String {
        format!(
            "# Persona\n{}\n\n# Regeln\n{}\n\n# Domäne\n{}\n\n# Ton\n{}\n\n# Eskalation\n{}",
            self.persona, self.hard_rules, self.domain_primer, self.tone, self.escalation
        )
    }
}

/// Load and validate SOUL.md from the given file path.
///
/// # Errors
/// Returns [`AssistantError::SoulLoad`] when the file cannot be read, and
/// [`AssistantError::SoulMissingSection`] when a required section heading is absent.
pub fn load(path: &std::path::Path) -> Result<Soul> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AssistantError::SoulLoad(format!("{}: {e}", path.display()))
    })?;
    parse(&content)
}

/// Parse SOUL.md content from a string (for testing without filesystem access).
pub fn parse(content: &str) -> Result<Soul> {
    let persona = extract_section(content, "Persona")?;
    let hard_rules = extract_section(content, "Hard Rules")?;
    let domain_primer = extract_section(content, "Domain Primer")?;
    let tone = extract_section(content, "Tone")?;
    let escalation = extract_section(content, "Escalation")?;

    Ok(Soul {
        persona,
        hard_rules,
        domain_primer,
        tone,
        escalation,
    })
}

/// Extract the text body of a `# Heading` section, stopping at the next `#` heading.
fn extract_section(content: &str, heading: &str) -> Result<String> {
    let needle = format!("# {heading}");
    let start = content.find(&needle).ok_or_else(|| {
        AssistantError::SoulMissingSection(heading.to_string())
    })?;

    // Advance past the heading line.
    let after_heading = &content[start + needle.len()..];
    let body = after_heading
        .trim_start_matches('\n')
        .trim_start_matches('\r');

    // Collect until the next top-level heading.
    let end = body
        .find("\n# ")
        .unwrap_or(body.len());

    Ok(body[..end].trim().to_string())
}

/// Validate that the content string contains all required section headings.
///
/// Returns the first missing section name, or `Ok(())` if all are present.
pub fn validate_sections(content: &str) -> Result<()> {
    for section in REQUIRED_SECTIONS {
        let needle = format!("# {section}");
        if !content.contains(&needle) {
            return Err(AssistantError::SoulMissingSection(section.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SOUL: &str = r#"# Persona
Ich bin der Assistent.

# Hard Rules
Regel 1.

# Domain Primer
Umzugsgeschäft.

# Tone
Direkt.

# Escalation
Bei Unklarheit fragen.
"#;

    #[test]
    fn parses_all_sections() {
        let soul = parse(VALID_SOUL).unwrap();
        assert!(soul.persona.contains("Assistent"));
        assert!(soul.hard_rules.contains("Regel"));
        assert!(soul.domain_primer.contains("Umzug"));
        assert!(soul.tone.contains("Direkt"));
        assert!(soul.escalation.contains("Unklarheit"));
    }

    #[test]
    fn missing_section_returns_error() {
        let missing_tone = r#"# Persona
Test

# Hard Rules
Test

# Domain Primer
Test

# Escalation
Test
"#;
        let err = parse(missing_tone).unwrap_err();
        assert!(matches!(err, AssistantError::SoulMissingSection(s) if s == "Tone"));
    }

    #[test]
    fn as_system_prompt_contains_all_sections() {
        let soul = parse(VALID_SOUL).unwrap();
        let prompt = soul.as_system_prompt();
        assert!(prompt.contains("Persona"));
        assert!(prompt.contains("Regeln"));
        assert!(prompt.contains("Eskalation"));
    }
}
