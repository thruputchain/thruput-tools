//! Embedded, release-matched instructions for coding agents.

const CLI_VERSION_PLACEHOLDER: &str = "{{GROUNDHOG_CLI_VERSION}}";
const SKILL_TEMPLATE: &str = include_str!("../assets/groundhog/knowledge/generated/SKILL.md");

/// Render the skill bundled with this CLI build.
#[must_use]
pub fn render() -> String {
    SKILL_TEMPLATE.replace(CLI_VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_skill_has_valid_identity_and_bounded_size() {
        let skill = render();

        assert!(skill.starts_with("---\nname: groundhog\n"));
        assert!(skill.contains(&format!(
            "<!-- groundhog-cli-version: {} -->",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!skill.contains(CLI_VERSION_PLACEHOLDER));
        assert!(skill.ends_with('\n'));
        assert!(!skill.ends_with("\n\n"));
        assert!(skill.len() <= 20 * 1024, "skill is {} bytes", skill.len());
    }

    #[test]
    fn rendered_skill_preserves_high_risk_guidance() {
        let skill = render();

        assert!(skill.contains("`Balance` is the practical starting point"));
        assert!(skill.contains("Do not use raw `NNInt` for a balance"));
        assert!(skill.contains("`Balance` is the ordinary type for balances"));
        assert!(skill.contains("`NNInt` is the lower-level integer type for counters"));
        assert!(skill.contains("`RCQ<T>` is the SDK's retro-causal queue"));
        assert!(skill.contains("self.value.try_get().none_if_missing().unwrap_or(0)"));
        assert!(skill.contains("A generated TypeScript binding describes a contract"));
        assert!(skill.contains("Generated event descriptors such as `PingEvent`"));
        assert!(skill.contains("Inspect\n   `node_modules`, Cargo registry source"));
        assert!(skill.contains("Never print, log, store, or paste private signing material"));
    }
}
