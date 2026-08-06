//! Command dispatch: the only module that prints and the only module that
//! decides exit codes.

use std::collections::BTreeSet;
use std::process::ExitCode;

use crate::cli::{Cli, Command};
use crate::config::{self, Config};
use crate::file_io::{self, WriteOutcome};
use crate::paths::Layout;

/// Overall result of a cloakroom run, ordered from least to most severe.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RunStatus {
    /// Everything checked out and every operation succeeded.
    #[default]
    Clean,
    /// The configuration or the resolved identity needs attention.
    Issues,
    /// An operational failure occurred.
    Failure,
}

impl RunStatus {
    /// Combine independent outcomes, retaining the more severe status.
    pub(crate) const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failure, _) | (_, Self::Failure) => Self::Failure,
            (Self::Issues, _) | (_, Self::Issues) => Self::Issues,
            (Self::Clean, Self::Clean) => Self::Clean,
        }
    }
}

impl From<RunStatus> for ExitCode {
    fn from(status: RunStatus) -> Self {
        // These numbers are the CLI contract. They are documented in the
        // README and scripts branch on them; do not renumber.
        Self::from(match status {
            RunStatus::Clean => 0,
            RunStatus::Issues => 1,
            RunStatus::Failure => 2,
        })
    }
}

/// Dispatch one command. An `Err` reaching here is operational (git missing,
/// an unwritable directory), so it prints once and exits `Failure`. Problems
/// with the user's configuration are not errors; they are reported as lines
/// and an `Issues` or `Failure` status by the command itself.
pub(crate) fn run(cli: Cli) -> RunStatus {
    let result = match cli.command {
        Command::Init => init(),
        Command::Apply => apply(),
        Command::Status => status(),
        Command::Doctor => doctor(),
    };
    result.unwrap_or_else(|err| {
        eprintln!("cloakroom: {err:#}");
        RunStatus::Failure
    })
}

/// Written by `init` when there is no config yet. Every line is a comment, so
/// a fresh install compiles to an empty root.gitconfig and changes nothing
/// about the identity git resolves until the user edits it.
const STARTER_CONFIG: &str = "\
# cloakroom configuration. Edit, then run `cloakroom apply`.
#
# Profiles are identities. Rules select a profile by repository location
# (path, git's gitdir condition) or by remote URL (remotes, git's hasconfig
# condition). When several rules match, the last one wins.
#
# [profiles.personal]
# name = \"Your Name\"
# email = \"you@example.com\"
#
# [[rules]]
# profile = \"personal\"
# path = \"~/src/personal/\"
#
# [[rules]]
# profile = \"personal\"
# remotes = [\"git@github.com:you/**\", \"https://github.com/you/**\"]
";

/// Create the config, compile it, and point the global gitconfig at the
/// result. Idempotent: a second run keeps the existing config and adds no
/// second include, so it is safe to tell a user to run it again.
fn init() -> anyhow::Result<RunStatus> {
    let layout = Layout::from_env()?;
    std::fs::create_dir_all(layout.objects_dir())?;

    let config_file = layout.config_file();
    if config_file.is_file() {
        println!("kept existing {}", config_file.display());
    } else {
        file_io::write_atomic(&config_file, STARTER_CONFIG)?;
        println!("wrote {}", config_file.display());
    }

    // Generate before touching the global gitconfig, so a broken existing
    // config stops init early.
    let status = apply_with(&layout)?;
    if status != RunStatus::Clean {
        return Ok(status);
    }

    let include = &layout.include_path;
    if crate::git::equivalent_global_include_count(&layout.root_gitconfig())? > 0 {
        println!("global gitconfig already includes {include}");
    } else {
        crate::git::add_global_include(include)?;
        println!("added include.path {include} to the global gitconfig");
    }
    Ok(RunStatus::Clean)
}

/// Compile config.toml into the generated tree. Unlike `init` it refuses to
/// create a config, so a wrong HOME or XDG_CONFIG_HOME surfaces as an error
/// instead of a second, empty configuration somewhere else.
fn apply() -> anyhow::Result<RunStatus> {
    let layout = Layout::from_env()?;
    let config_file = layout.config_file();
    if !config_file.is_file() {
        anyhow::bail!("no config at {}; run cloakroom init", config_file.display());
    }
    apply_with(&layout)
}

/// The body shared by `apply` and `init`. Validation findings go to stderr
/// and end the run at `Failure`: the config is the user's to fix, and writing
/// a partial tree from it would leave git reading something nobody asked for.
fn apply_with(layout: &Layout) -> anyhow::Result<RunStatus> {
    let config = config::load(&layout.config_file())?;
    let config = match config::validate(config) {
        Ok(config) => config,
        Err(findings) => {
            for finding in &findings {
                eprintln!("cloakroom: {finding}");
            }
            return Ok(RunStatus::Failure);
        }
    };
    if config.uses_remote_rules() {
        ensure_remote_rule_support(crate::git::remote_rule_support()?)?;
    }
    write_generated(layout, &config)?;
    Ok(RunStatus::Clean)
}

/// Remote rules compile to `hasconfig` conditions. Git treats a condition it
/// does not know as false, so on an old git the generated file would parse
/// cleanly and never match. Refusing is the only way the user finds out.
fn ensure_remote_rule_support(support: crate::git::RemoteRuleSupport) -> anyhow::Result<()> {
    match support {
        crate::git::RemoteRuleSupport::Supported => Ok(()),
        crate::git::RemoteRuleSupport::TooOld(major, minor) => anyhow::bail!(
            "git {major}.{minor} does not support hasconfig conditions; remote rules need git 2.36 or newer"
        ),
        crate::git::RemoteRuleSupport::Unknown => {
            anyhow::bail!("could not parse the git version; remote rules need git 2.36 or newer")
        }
    }
}

/// Publish the compiled tree in an order that keeps it readable throughout.
///
/// Objects first, then the root that points at them, then pruning of what the
/// root no longer names. Git may read these files at any moment, and at every
/// point in this sequence every path the root names exists. A failure part
/// way leaves the previous root intact and some unreferenced objects behind,
/// which the next apply prunes and doctor reports meanwhile.
fn write_generated(layout: &Layout, config: &Config) -> anyhow::Result<()> {
    let compiled = crate::render::compile(config);
    std::fs::create_dir_all(layout.objects_dir())?;
    for (digest, contents) in &compiled.objects {
        report_write(
            file_io::write_atomic(&layout.object_gitconfig(digest), contents)?,
            format!("generated/objects/{digest}.gitconfig"),
        );
    }
    report_write(
        file_io::write_atomic(&layout.root_gitconfig(), &compiled.root)?,
        "generated/root.gitconfig".to_owned(),
    );
    let keep: BTreeSet<String> = compiled.objects.keys().cloned().collect();
    for name in file_io::prune_objects(&layout.objects_dir(), &keep)? {
        println!("removed generated/objects/{name}");
    }
    Ok(())
}

fn report_write(outcome: WriteOutcome, name: String) {
    match outcome {
        WriteOutcome::Wrote => println!("wrote {name}"),
        WriteOutcome::Unchanged => println!("unchanged {name}"),
    }
}

/// Print the identity report for the current directory. The report decides
/// the status; this only prints it.
fn status() -> anyhow::Result<RunStatus> {
    let layout = Layout::from_env()?;
    let cwd = std::env::current_dir()?;
    let report = crate::status::report(&cwd, &layout)?;
    for line in &report.lines {
        println!("{line}");
    }
    Ok(report.status)
}

/// Print the health report. Read-only, so it is safe to run anywhere, and it
/// never repairs; every issue line names the command that fixes it.
fn doctor() -> anyhow::Result<RunStatus> {
    let layout = Layout::from_env()?;
    let cwd = std::env::current_dir()?;
    let report = crate::doctor::report(&cwd, &layout)?;
    for line in &report.lines {
        println!("{line}");
    }
    Ok(report.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_combination_retains_the_highest_severity() {
        assert_eq!(RunStatus::Clean.combine(RunStatus::Clean), RunStatus::Clean);
        assert_eq!(
            RunStatus::Clean.combine(RunStatus::Issues),
            RunStatus::Issues
        );
        assert_eq!(
            RunStatus::Issues.combine(RunStatus::Clean),
            RunStatus::Issues
        );
        assert_eq!(
            RunStatus::Issues.combine(RunStatus::Failure),
            RunStatus::Failure
        );
        assert_eq!(
            RunStatus::Failure.combine(RunStatus::Clean),
            RunStatus::Failure
        );
    }

    #[test]
    fn the_starter_config_parses_as_an_empty_config() {
        // A fresh init must produce a valid, empty generated tree.
        let config: config::RawConfig = toml::from_str(STARTER_CONFIG).unwrap();
        assert!(config.profiles.is_empty());
        assert!(config.rules.is_empty());
    }

    #[test]
    fn remote_rule_preflight_rejects_old_and_unknown_git() {
        assert!(ensure_remote_rule_support(crate::git::RemoteRuleSupport::Supported).is_ok());
        assert!(ensure_remote_rule_support(crate::git::RemoteRuleSupport::TooOld(2, 35)).is_err());
        assert!(ensure_remote_rule_support(crate::git::RemoteRuleSupport::Unknown).is_err());
    }
}
