//! The declarative TOML schema and its validation.
//!
//! Two shapes of the same data. `RawConfig` mirrors the file, with every
//! combination of fields the user could write. `Config` is what survives
//! `validate`: every rule names a profile that exists and carries exactly one
//! condition. `render` takes only the second, so compilation cannot fail.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// config.toml as written. Field names are the TOML keys, and
/// `deny_unknown_fields` turns a misspelled key into a parse error rather
/// than a setting that is silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    /// BTreeMap keeps validation and profile compilation deterministic.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Vec preserves TOML order; git applies the last matching include, so
    /// order is the user's override mechanism.
    #[serde(default)]
    pub rules: Vec<RawRule>,
}

/// One identity. Compiles to one `[user]` block in one generated object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub name: String,
    pub email: String,
}

/// One selection rule as written. `path` and `remotes` are alternatives;
/// validation rejects a rule with both or with neither. `PartialEq` is here
/// for the duplicate-rule check.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRule {
    /// Key into `RawConfig::profiles`.
    pub profile: String,
    /// Repository location, passed through to git's `gitdir` condition
    /// verbatim. Leading `~` and trailing `/` mean what git says they mean;
    /// cloakroom deliberately does no expansion or matching of its own.
    pub path: Option<String>,
    /// Remote URL patterns for git's `hasconfig` condition. Any one matching
    /// selects the profile, so a repository reachable over SSH and HTTPS
    /// needs both spellings.
    #[serde(default)]
    pub remotes: Vec<String>,
    /// Match `path` case-insensitively (`gitdir/i`). Git has no
    /// case-insensitive `hasconfig`, so this is rejected on remote rules.
    #[serde(default)]
    pub case_insensitive: bool,
}

/// A validated config. Holding one means every rule resolves and every
/// condition is well formed.
#[derive(Debug)]
pub(crate) struct Config {
    pub profiles: BTreeMap<String, Profile>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub profile: String,
    pub condition: RuleCondition,
}

/// A rule's condition, already narrowed to the one git keyword it compiles
/// to. `Remotes` holds several patterns because one rule emits one includeIf
/// per pattern.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuleCondition {
    Path {
        path: String,
        case_insensitive: bool,
    },
    Remotes {
        patterns: Vec<String>,
    },
}

impl Config {
    /// Whether compiling will emit `hasconfig` conditions, which need git
    /// 2.36. Path rules work on any git that has conditional includes.
    pub(crate) fn uses_remote_rules(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| matches!(rule.condition, RuleCondition::Remotes { .. }))
    }
}

/// Read and parse. Anything that is a matter of meaning rather than syntax is
/// left to `validate`, so one run can report every problem at once.
pub(crate) fn load(path: &Path) -> anyhow::Result<RawConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
}

/// Collect every problem instead of stopping at the first, so one run shows
/// the whole repair list.
///
/// Findings name rules by their 1-based position, because a rule has no name
/// of its own and TOML gives no line numbers here. A rule that fails still
/// gets checked for its other problems; only its condition is dropped.
pub(crate) fn validate(config: RawConfig) -> Result<Config, Vec<String>> {
    let mut findings = Vec::new();
    let mut rules = Vec::new();

    for (key, profile) in &config.profiles {
        if !is_valid_key(key) {
            findings.push(format!(
                "profile key {key:?} may only contain letters, digits, '.', '_', and '-'"
            ));
        }
        if profile.name.is_empty() {
            findings.push(format!("profile {key}: name is empty"));
        }
        if profile.email.is_empty() {
            findings.push(format!("profile {key}: email is empty"));
        } else if !profile.email.contains('@') {
            findings.push(format!(
                "profile {key}: email {:?} has no '@'",
                profile.email
            ));
        }
        for (field, value) in [("name", &profile.name), ("email", &profile.email)] {
            if has_control_character(value) {
                findings.push(format!(
                    "profile {key}: {field} contains a control character; that would corrupt the generated gitconfig"
                ));
            }
        }
    }

    for (index, rule) in config.rules.iter().enumerate() {
        let ordinal = index + 1;
        if !config.profiles.contains_key(&rule.profile) {
            findings.push(format!(
                "rule {ordinal}: unknown profile {:?}",
                rule.profile
            ));
        }
        let condition = match (&rule.path, rule.remotes.is_empty()) {
            (None, true) => {
                findings.push(format!(
                    "rule {ordinal}: needs a path or remotes; it can never match"
                ));
                None
            }
            (Some(_), false) => {
                findings.push(format!(
                    "rule {ordinal}: has both path and remotes; write two rules instead"
                ));
                None
            }
            (Some(path), true) => Some(RuleCondition::Path {
                path: path.clone(),
                case_insensitive: rule.case_insensitive,
            }),
            (None, false) => Some(RuleCondition::Remotes {
                patterns: rule.remotes.clone(),
            }),
        };
        if rule.case_insensitive && rule.path.is_none() {
            findings.push(format!(
                "rule {ordinal}: case_insensitive only applies to path rules (git has gitdir/i but no case-insensitive hasconfig)"
            ));
        }
        if let Some(path) = &rule.path {
            if path.is_empty() {
                findings.push(format!("rule {ordinal}: path is empty"));
            }
            if has_control_character(path) {
                findings.push(format!("rule {ordinal}: path contains a control character"));
            }
        }
        for pattern in &rule.remotes {
            if pattern.is_empty() {
                findings.push(format!("rule {ordinal}: a remote pattern is empty"));
            }
            if has_control_character(pattern) {
                findings.push(format!(
                    "rule {ordinal}: remote pattern {pattern:?} contains a control character"
                ));
            }
        }
        // Quadratic, over a list a human typed. Comparing whole rules means
        // the same condition under a different profile is not a duplicate;
        // that is deliberate override, and doctor reports it separately.
        if config.rules[..index].contains(rule) {
            findings.push(format!("rule {ordinal}: duplicate of an earlier rule"));
        }
        if let Some(condition) = condition {
            rules.push(Rule {
                profile: rule.profile.clone(),
                condition,
            });
        }
    }

    if findings.is_empty() {
        Ok(Config {
            profiles: config.profiles,
            rules,
        })
    } else {
        Err(findings)
    }
}

/// Profile keys end up quoted inside generated gitconfig and unquoted in
/// every report. Restricting them to plain ASCII keeps both readable and
/// keeps comparison free of case and encoding questions.
fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A newline in a value would end the gitconfig line and let the rest inject
/// arbitrary configuration; other control characters are never legitimate in
/// a name, email, path, or URL pattern either.
fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config the rest of the suite reasons about: two profiles, a path
    /// rule, and a remote rule with two spellings of the same host.
    pub(crate) const SAMPLE: &str = r#"
[profiles.personal]
name = "Pat Personal"
email = "pat@example.com"

[profiles.work]
name = "Pat Work"
email = "pat@work.example"

[[rules]]
profile = "work"
path = "~/src/work/"

[[rules]]
profile = "work"
remotes = [
  "git@example.com:work/**",
  "https://example.com/work/**",
]
"#;

    fn parse(text: &str) -> RawConfig {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn the_sample_config_parses_and_validates() {
        let config = validate(parse(SAMPLE)).unwrap();
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(config.rules.len(), 2);
    }

    #[test]
    fn rule_order_is_preserved_from_the_toml() {
        let config = validate(parse(SAMPLE)).unwrap();
        assert!(matches!(
            &config.rules[0].condition,
            RuleCondition::Path { path, .. } if path == "~/src/work/"
        ));
        assert!(matches!(
            &config.rules[1].condition,
            RuleCondition::Remotes { patterns } if patterns.len() == 2
        ));
    }

    #[test]
    fn an_empty_config_is_valid() {
        let config = validate(parse("")).unwrap();
        assert!(config.profiles.is_empty());
        assert!(config.rules.is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected_at_parse_time() {
        // A typo must be an error, not silence.
        assert!(
            toml::from_str::<RawConfig>("[profiles.a]\nname = \"A\"\nemial = \"a@b\"\n").is_err()
        );
        assert!(
            toml::from_str::<RawConfig>("[[rules]]\nprofile = \"a\"\npaths = \"x\"\n").is_err()
        );
    }

    /// Assert that validation rejects `text` for the stated reason. Matching
    /// a substring keeps the wording of findings free to change; matching
    /// against all findings keeps a test honest when the fixture trips more
    /// than one rule.
    fn single_finding(text: &str, needle: &str) {
        let findings = validate(parse(text)).unwrap_err();
        assert!(
            findings.iter().any(|finding| finding.contains(needle)),
            "expected a finding containing {needle:?}, got {findings:?}"
        );
    }

    #[test]
    fn a_rule_must_reference_a_known_profile() {
        single_finding(
            "[[rules]]\nprofile = \"ghost\"\npath = \"~/x/\"\n",
            "unknown profile",
        );
    }

    #[test]
    fn a_rule_needs_a_condition() {
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\n",
            "needs a path or remotes",
        );
    }

    #[test]
    fn a_rule_may_not_mix_path_and_remotes() {
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\npath = \"~/x/\"\nremotes = [\"git@h:o/**\"]\n",
            "both path and remotes",
        );
    }

    #[test]
    fn case_insensitive_is_rejected_on_remote_rules() {
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\nremotes = [\"git@h:o/**\"]\ncase_insensitive = true\n",
            "case_insensitive only applies to path rules",
        );
    }

    #[test]
    fn profile_keys_are_restricted_to_filename_safe_characters() {
        single_finding(
            "[profiles.\"a/b\"]\nname = \"A\"\nemail = \"a@b\"\n",
            "may only contain",
        );
    }

    #[test]
    fn empty_and_at_less_identity_fields_are_rejected() {
        single_finding(
            "[profiles.a]\nname = \"\"\nemail = \"a@b\"\n",
            "name is empty",
        );
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"\"\n",
            "email is empty",
        );
        single_finding("[profiles.a]\nname = \"A\"\nemail = \"nope\"\n", "no '@'");
    }

    #[test]
    fn control_characters_are_rejected_as_gitconfig_injection() {
        single_finding(
            "[profiles.a]\nname = \"A\\nB\"\nemail = \"a@b\"\n",
            "control character",
        );
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\npath = \"~/x\\u0007/\"\n",
            "control character",
        );
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\nremotes = [\"git@h:o/\\u0007\"]\n",
            "remote pattern",
        );
    }

    #[test]
    fn empty_rule_values_are_rejected() {
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\npath = \"\"\n",
            "path is empty",
        );
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\nremotes = [\"\"]\n",
            "remote pattern is empty",
        );
    }

    #[test]
    fn exact_duplicate_rules_are_rejected() {
        single_finding(
            "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[[rules]]\nprofile = \"a\"\npath = \"~/x/\"\n[[rules]]\nprofile = \"a\"\npath = \"~/x/\"\n",
            "duplicate",
        );
    }

    #[test]
    fn the_same_condition_under_different_profiles_is_allowed() {
        // Last include wins in git; doctor reports the shadowing, apply
        // accepts it.
        let text = "[profiles.a]\nname = \"A\"\nemail = \"a@b\"\n[profiles.b]\nname = \"B\"\nemail = \"b@b\"\n[[rules]]\nprofile = \"a\"\npath = \"~/x/\"\n[[rules]]\nprofile = \"b\"\npath = \"~/x/\"\n";
        assert!(validate(parse(text)).is_ok());
    }
}
