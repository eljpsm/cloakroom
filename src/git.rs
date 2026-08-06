//! Every invocation of the installed git. Cloakroom never parses gitconfig
//! or matches gitdir/hasconfig patterns itself; git answers, cloakroom
//! reports.
//!
//! That is the point of the whole design. A reimplementation of git's
//! matching would drift from the git the user actually commits with, and the
//! reports would be confidently wrong. Everything here asks the real git,
//! from the current directory, with the user's real environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Context;

/// Run git and hand back the raw `Output`. Only failure to spawn is an error
/// here: git's exit codes carry meaning (see `global_include_paths`) and each
/// caller decides what its own nonzero status means.
fn run(configure: impl FnOnce(&mut Command)) -> anyhow::Result<Output> {
    let mut command = Command::new("git");
    configure(&mut command);
    command
        .output()
        .context("failed to run git; is it installed?")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

/// (major, minor) of the installed git, or None when the version string is
/// not a shape we recognize.
pub(crate) fn version() -> anyhow::Result<Option<(u32, u32)>> {
    let output = run(|command| {
        command.arg("version");
    })?;
    if !output.status.success() {
        anyhow::bail!("git version failed: {}", stderr_of(&output));
    }
    Ok(parse_version(&String::from_utf8_lossy(&output.stdout)))
}

/// Whether this git understands the `hasconfig` includeIf condition that
/// remote rules compile to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteRuleSupport {
    /// git 2.36 or newer.
    Supported,
    /// Older git. It parses `hasconfig` as a condition it does not know and
    /// treats it as false, so remote rules would silently never match.
    TooOld(u32, u32),
    /// The version string did not parse. Treated as unsupported, because the
    /// failure mode of guessing wrong is silent.
    Unknown,
}

pub(crate) fn remote_rule_support() -> anyhow::Result<RemoteRuleSupport> {
    Ok(remote_rule_support_for(version()?))
}

fn remote_rule_support_for(version: Option<(u32, u32)>) -> RemoteRuleSupport {
    match version {
        Some(version) if version < (2, 36) => RemoteRuleSupport::TooOld(version.0, version.1),
        Some(_) => RemoteRuleSupport::Supported,
        None => RemoteRuleSupport::Unknown,
    }
}

/// "git version 2.43.0" -> (2, 43). Platform suffixes fall away.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let numbers = text
        .trim()
        .strip_prefix("git version ")?
        .split(' ')
        .next()?;
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Every include.path visible in the global scope, in order.
///
/// `--path` makes git expand a leading `~`, so the results are comparable to
/// real paths. `--null` keeps paths with spaces or newlines intact.
/// `--get-all` exits 1 with empty output when the key is unset; that is
/// "none", not a failure, and only a different failure is an error.
pub(crate) fn global_include_paths() -> anyhow::Result<Vec<String>> {
    let output = run(|command| {
        command.args([
            "config",
            "--global",
            "--path",
            "--null",
            "--get-all",
            "include.path",
        ]);
    })?;
    if output.status.success() {
        return Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(|value| String::from_utf8_lossy(value).into_owned())
            .collect());
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    anyhow::bail!(
        "git config --get-all include.path failed: {}",
        stderr_of(&output)
    )
}

/// How many global include.path entries lead to `target`. Zero means init has
/// not run; more than one means git reads the generated root twice, which is
/// harmless but a sign the gitconfig was edited by hand.
pub(crate) fn equivalent_global_include_count(target: &Path) -> anyhow::Result<usize> {
    Ok(global_include_paths()?
        .iter()
        .filter(|path| same_target(Path::new(path), target))
        .count())
}

/// Compare by what the paths resolve to, so a symlink or a different
/// spelling of the same file does not read as a second, unrelated include.
/// Paths that cannot be resolved fall back to string equality, which is the
/// conservative answer: two files that do not exist are not known to be one.
fn same_target(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Append one include.path to the global gitconfig. `--add`, never
/// `--replace-all`: any other include.path there belongs to the user. This is
/// the only write cloakroom makes outside its own directory.
pub(crate) fn add_global_include(path: &str) -> anyhow::Result<()> {
    let output = run(|command| {
        command.args(["config", "--global", "--add", "include.path", path]);
    })?;
    if !output.status.success() {
        anyhow::bail!(
            "git config --add include.path failed: {}",
            stderr_of(&output)
        );
    }
    Ok(())
}

/// The working tree root containing `cwd`, or None when there is none.
/// Only "not a git repository" becomes None; any other failure is an error,
/// so an unreadable repository is never reported as no repository.
pub(crate) fn repo_toplevel(cwd: &Path) -> anyhow::Result<Option<PathBuf>> {
    let output = run(|command| {
        command
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--show-toplevel"]);
    })?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        return Ok(Some(PathBuf::from(text.trim_end())));
    }
    if stderr_of(&output).contains("not a git repository") {
        return Ok(None);
    }
    anyhow::bail!("git rev-parse failed: {}", stderr_of(&output))
}

/// One resolved config value with where git found it.
#[derive(Debug)]
pub(crate) struct ConfigEntry {
    pub scope: String,
    /// Present for `file:` origins; command line and blob origins are not files.
    pub origin: Option<PathBuf>,
    pub value: String,
}

/// The three values cloakroom cares about, as git resolved them here. Each
/// keeps its own origin, so a report can say which file won and whether two
/// of them came from the same one.
#[derive(Debug, Default)]
pub(crate) struct EffectiveIdentity {
    pub profile: Option<ConfigEntry>,
    pub name: Option<ConfigEntry>,
    pub email: Option<ConfigEntry>,
}

/// Resolve the selected profile, name, and email from one Git config
/// snapshot.
///
/// One listing rather than three `--get` calls, so the three values are
/// guaranteed to come from the same resolution. Reading them separately
/// could interleave with an edit and produce a report of a state that never
/// existed.
pub(crate) fn effective_identity(cwd: &Path) -> anyhow::Result<EffectiveIdentity> {
    let output = run(|command| {
        command.arg("-C").arg(cwd).args([
            "config",
            "--null",
            "--list",
            "--show-scope",
            "--show-origin",
        ]);
    })?;
    if !output.status.success() {
        anyhow::bail!("git config --list failed: {}", stderr_of(&output));
    }
    parse_identity(&output.stdout)
}

/// Split the `--null --list --show-scope --show-origin` stream.
///
/// One record per entry, three NUL terminated fields:
///
/// ```text
/// scope NUL origin NUL key LF value NUL
/// ```
///
/// The last record for a key overwrites the earlier ones, which is git's own
/// resolution order, so what comes out is what git would use. Origins are
/// prefixed `file:`; command line and blob origins are not paths and are
/// kept as None.
///
/// A key written with no value at all (`[pull]` then a bare `rebase`) is
/// legal, and its record has no LF. Such a key reads as unset, which is what
/// it is to anything wanting a string: git itself fails with "missing value"
/// the moment it needs one.
fn parse_identity(bytes: &[u8]) -> anyhow::Result<EffectiveIdentity> {
    let mut identity = EffectiveIdentity::default();
    let mut fields = bytes.split(|byte| *byte == 0);
    while let Some(scope) = fields.next() {
        if scope.is_empty() {
            break;
        }
        let origin = fields.next().context("git config output has no origin")?;
        let pair = fields
            .next()
            .context("git config output has no key and value")?;
        // Git never emits an empty key, so this is a truncated record and
        // not the valueless key handled below.
        if pair.is_empty() {
            anyhow::bail!("git config output has an empty key");
        }
        let pair = String::from_utf8_lossy(pair);
        // No LF means a valueless key. It still overwrites what an earlier
        // scope set, because that is what git resolves to here.
        let (key, value) = match pair.split_once('\n') {
            Some((key, value)) => (key, Some(value)),
            None => (pair.as_ref(), None),
        };
        let entry = value.map(|value| ConfigEntry {
            scope: String::from_utf8_lossy(scope).into_owned(),
            origin: String::from_utf8_lossy(origin)
                .strip_prefix("file:")
                .map(PathBuf::from),
            value: value.to_owned(),
        });
        match key {
            "cloakroom.profile" => identity.profile = entry,
            "user.name" => identity.name = entry,
            "user.email" => identity.email = entry,
            _ => {}
        }
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_strings_parse_including_platform_suffixes() {
        assert_eq!(parse_version("git version 2.43.0"), Some((2, 43)));
        assert_eq!(parse_version("git version 2.36.1.windows.1"), Some((2, 36)));
        assert_eq!(
            parse_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39))
        );
        assert_eq!(parse_version("nonsense"), None);
    }

    #[test]
    fn remote_rule_support_is_conservative() {
        assert_eq!(
            remote_rule_support_for(Some((2, 35))),
            RemoteRuleSupport::TooOld(2, 35)
        );
        assert_eq!(
            remote_rule_support_for(Some((2, 36))),
            RemoteRuleSupport::Supported
        );
        assert_eq!(remote_rule_support_for(None), RemoteRuleSupport::Unknown);
    }

    /// user.name appears twice, global then local, as it would for a
    /// repository that overrides the identity cloakroom selected. Reporting
    /// the local one is what lets doctor call that out.
    #[test]
    fn identity_parser_keeps_last_values_and_file_origins() {
        let bytes = b"global\0file:/profiles/a.gitconfig\0cloakroom.profile\na\0\
                      global\0file:/profiles/a.gitconfig\0user.name\nFirst\0\
                      local\0file:.git/config\0user.name\nOverride\0\
                      global\0file:/profiles/a.gitconfig\0user.email\na@b\0";
        let identity = parse_identity(bytes).unwrap();
        assert_eq!(identity.profile.unwrap().value, "a");
        let name = identity.name.unwrap();
        assert_eq!(name.value, "Override");
        assert_eq!(name.origin.as_deref(), Some(Path::new(".git/config")));
        assert_eq!(identity.email.unwrap().value, "a@b");
    }

    #[test]
    fn identity_parser_rejects_incomplete_records() {
        assert!(parse_identity(b"global\0").is_err());
        assert!(parse_identity(b"global\0file:/config\0").is_err());
    }

    /// A valueless key is legal in a gitconfig and reaches the listing with
    /// no value field. One of them must not take the whole listing down: the
    /// keys around it are still what git resolves.
    #[test]
    fn a_valueless_key_does_not_abort_the_listing() {
        let bytes = b"global\0file:/config\0pull.rebase\0\
                      global\0file:/profiles/a.gitconfig\0cloakroom.profile\na\0\
                      global\0file:/profiles/a.gitconfig\0user.name\nPat\0\
                      global\0file:/profiles/a.gitconfig\0user.email\na@b\0";
        let identity = parse_identity(bytes).unwrap();
        assert_eq!(identity.profile.unwrap().value, "a");
        assert_eq!(identity.name.unwrap().value, "Pat");
        assert_eq!(identity.email.unwrap().value, "a@b");
    }

    /// A tracked key written with no value reads as unset, and overwrites an
    /// earlier scope like any other last value. Reporting the earlier one
    /// would name an identity git will not use; it fails with "missing
    /// value" as soon as it needs that key.
    #[test]
    fn a_valueless_tracked_key_reads_as_unset() {
        let bytes = b"global\0file:/profiles/a.gitconfig\0user.name\nPat\0\
                      local\0file:.git/config\0user.name\0";
        assert!(parse_identity(bytes).unwrap().name.is_none());
    }

    /// Neither path can be canonicalized, so the comparison falls back to
    /// the strings and must not collapse two different includes into one.
    #[test]
    fn different_missing_paths_are_not_the_same_target() {
        assert!(!same_target(
            Path::new("/cloakroom-test/missing-a"),
            Path::new("/cloakroom-test/missing-b")
        ));
    }
}
