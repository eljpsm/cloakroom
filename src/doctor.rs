//! Health checks: the configuration, the generated files, the managed
//! include, and the identity git actually resolves. Read-only; it diagnoses
//! and names the fix, it never repairs.
//!
//! Every issue line must name the command that fixes it. A diagnosis the user
//! cannot act on is worse than silence, and repairing on their behalf would
//! make doctor unsafe to run when something is already wrong.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;

use crate::app::RunStatus;
use crate::config::{self, Config};
use crate::git;
use crate::paths::Layout;
use crate::render;
use crate::status::{is_generated_marker, origin_display, same_origin};

pub(crate) struct Report {
    pub lines: Vec<String>,
    pub status: RunStatus,
}

/// Collects report lines and the worst status seen. Checks push findings and
/// carry on, so one run lists everything that is wrong rather than the first
/// thing.
struct Doctor {
    lines: Vec<String>,
    status: RunStatus,
}

impl Doctor {
    fn ok(&mut self, message: impl Display) {
        self.lines.push(format!("ok: {message}"));
    }

    /// Worth knowing, not worth a nonzero exit.
    fn note(&mut self, message: impl Display) {
        self.lines.push(format!("note: {message}"));
    }

    fn issue(&mut self, message: impl Display) {
        self.lines.push(format!("issue: {message}"));
        self.status = self.status.combine(RunStatus::Issues);
    }
}

/// Run every check, in the order a reader would want to fix them: the config,
/// then the files it should have produced, then git's ability to use them,
/// then what git resolves in this repository. Only the generated-file check
/// depends on an earlier one, since comparing files needs a config to compile.
pub(crate) fn report(cwd: &Path, layout: &Layout) -> anyhow::Result<Report> {
    let mut doctor = Doctor {
        lines: Vec::new(),
        status: RunStatus::Clean,
    };

    let config = check_config(&mut doctor, layout);
    if let Some(config) = &config {
        check_generated(&mut doctor, layout, config);
    }
    check_include(&mut doctor, layout)?;
    check_git_version(&mut doctor, config.as_ref())?;
    check_identity(&mut doctor, cwd, layout, config.as_ref())?;

    Ok(Report {
        lines: doctor.lines,
        status: doctor.status,
    })
}

/// Returns the config only when it parses and validates; later checks that
/// depend on a trustworthy config are skipped otherwise.
fn check_config(doctor: &mut Doctor, layout: &Layout) -> Option<Config> {
    let config_file = layout.config_file();
    if !config_file.is_file() {
        doctor.issue(format!(
            "no config at {}; run cloakroom init",
            config_file.display()
        ));
        return None;
    }
    let config = match config::load(&config_file) {
        Ok(config) => config,
        Err(err) => {
            doctor.issue(format!("{err:#}"));
            return None;
        }
    };
    let config = match config::validate(config) {
        Ok(config) => config,
        Err(findings) => {
            for finding in findings {
                doctor.issue(finding);
            }
            return None;
        }
    };
    doctor.ok("config.toml parses and validates");

    for key in config.profiles.keys() {
        if !config.rules.iter().any(|rule| &rule.profile == key) {
            doctor.note(format!(
                "profile {key} has no rules; nothing selects it automatically"
            ));
        }
    }

    // The same condition under different profiles: legal, but only the last
    // include wins, so the earlier rule is dead. Keyed on the compiled
    // condition string, which is what git compares too, so gitdir and
    // gitdir/i over one path count as the two different conditions they are.
    let mut by_condition: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for rule in &config.rules {
        for condition in render::conditions(rule) {
            by_condition
                .entry(condition)
                .or_default()
                .push(&rule.profile);
        }
    }
    for (condition, profiles) in by_condition {
        let mut distinct = profiles.clone();
        distinct.dedup();
        if distinct.len() > 1 {
            doctor.issue(format!(
                "condition {condition:?} selects profiles {}; git applies the last one and the others are dead",
                profiles.join(", ")
            ));
        }
    }

    Some(config)
}

/// Compare the tree on disk against a fresh compile. Byte equality is the
/// whole test, which is the reason compilation is deterministic. It catches a
/// hand edit, a half-finished apply, and a config changed since the last one,
/// without needing to tell them apart.
fn check_generated(doctor: &mut Doctor, layout: &Layout, config: &Config) {
    let compiled = render::compile(config);
    // One "ok" line only if nothing was stale, missing, or orphaned.
    let mut fresh = true;
    let mut check_file = |doctor: &mut Doctor, path: &Path, name: String, expected: &str| {
        match std::fs::read(path) {
            Ok(actual) if actual == expected.as_bytes() => {}
            Ok(_) => {
                doctor.issue(format!("{name} is stale; run cloakroom apply"));
                fresh = false;
            }
            Err(_) => {
                doctor.issue(format!("{name} is missing; run cloakroom apply"));
                fresh = false;
            }
        }
    };

    check_file(
        doctor,
        &layout.root_gitconfig(),
        "generated/root.gitconfig".to_owned(),
        &compiled.root,
    );
    for (digest, contents) in &compiled.objects {
        check_file(
            doctor,
            &layout.object_gitconfig(digest),
            format!("generated/objects/{digest}.gitconfig"),
            contents,
        );
    }

    // An object outside the manifest is not merely clutter. It is a
    // gitconfig with a [user] block that some old root may still include, so
    // it is reported rather than ignored. A missing objects/ directory was
    // already covered by the missing-file checks above.
    if let Ok(entries) = std::fs::read_dir(layout.objects_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(digest) = name.strip_suffix(".gitconfig") else {
                continue;
            };
            if !compiled.objects.contains_key(digest) {
                doctor.issue(format!(
                    "generated/objects/{name} is not in the current manifest; run cloakroom apply"
                ));
                fresh = false;
            }
        }
    }

    if fresh {
        doctor.ok("generated files match the configuration");
    }
}

/// Exactly one include, no more and no less. Zero means git never reads any
/// of this. Duplicates are harmless to git but mean the global gitconfig was
/// edited by hand, and the user should decide which line to keep.
fn check_include(doctor: &mut Doctor, layout: &Layout) -> anyhow::Result<()> {
    match git::equivalent_global_include_count(&layout.root_gitconfig())? {
        0 => doctor.issue(format!(
            "global gitconfig does not include {}; run cloakroom init",
            layout.include_path
        )),
        1 => doctor.ok("global gitconfig includes the generated root"),
        count => doctor.issue(format!(
            "global gitconfig includes the generated root {count} times; remove duplicate include.path entries"
        )),
    }
    Ok(())
}

/// Only asked when the config actually uses remote rules. Path rules work on
/// any git with conditional includes, and reporting a version that does not
/// matter would be noise.
fn check_git_version(doctor: &mut Doctor, config: Option<&Config>) -> anyhow::Result<()> {
    let uses_remotes = config.is_some_and(Config::uses_remote_rules);
    if !uses_remotes {
        return Ok(());
    }
    match git::remote_rule_support()? {
        git::RemoteRuleSupport::TooOld(major, minor) => doctor.issue(format!(
            "git {}.{} does not support hasconfig conditions; remote rules need git 2.36 or newer",
            major, minor
        )),
        git::RemoteRuleSupport::Supported => {
            doctor.ok("git is new enough for hasconfig remote rules")
        }
        git::RemoteRuleSupport::Unknown => {
            doctor.issue("could not parse the git version; remote rules need Git 2.36 or newer")
        }
    }
    Ok(())
}

/// The end-to-end check: what git resolves in this repository against what
/// the config says it should be.
///
/// It runs even when the config did not validate, because a broken config
/// does not stop the previously generated files from selecting an identity,
/// and that identity is what the user commits with today. The checks that
/// need the config stop early when there is none.
fn check_identity(
    doctor: &mut Doctor,
    cwd: &Path,
    layout: &Layout,
    config: Option<&Config>,
) -> anyhow::Result<()> {
    if git::repo_toplevel(cwd)?.is_none() {
        doctor.note("not inside a git repository; identity checks skipped");
        return Ok(());
    }

    let identity = git::effective_identity(cwd)?;
    if identity.name.is_none() {
        doctor.issue("no user.name resolves in this repository");
    }
    if identity.email.is_none() {
        doctor.issue("no user.email resolves in this repository; git will refuse to commit");
    }

    let Some(marker) = &identity.profile else {
        if identity.name.is_some() && identity.email.is_some() {
            doctor
                .ok("no cloakroom rule matches this repository; the identity comes from elsewhere");
        }
        return Ok(());
    };
    if !is_generated_marker(marker, layout) {
        doctor.issue(format!(
            "cloakroom profile marker {:?} comes from {} (scope {}), not a generated object",
            marker.value,
            origin_display(marker),
            marker.scope
        ));
        return Ok(());
    }
    // The marker is genuine, but there is no config to compare it against.
    // check_config already said why, so stop rather than repeat it.
    let Some(config) = config else {
        return Ok(());
    };
    let Some(expected) = config.profiles.get(&marker.value) else {
        doctor.issue(format!(
            "cloakroom profile marker {:?} is not in config.toml",
            marker.value
        ));
        return Ok(());
    };

    // Value and origin both have to agree. A repository that sets user.name
    // to the same string the profile does is still an override, and saying so
    // is what makes the next surprise explicable.
    let mut matches = true;
    if let Some(name) = &identity.name
        && (name.value != expected.name || !same_origin(name, marker))
    {
        doctor.issue(format!(
            "user.name is {:?} from {} (scope {}), but cloakroom profile {} sets {:?}",
            name.value,
            origin_display(name),
            name.scope,
            marker.value,
            expected.name
        ));
        matches = false;
    }
    if let Some(email) = &identity.email
        && (email.value != expected.email || !same_origin(email, marker))
    {
        doctor.issue(format!(
            "user.email is {:?} from {} (scope {}), but cloakroom profile {} sets {:?}",
            email.value,
            origin_display(email),
            email.scope,
            marker.value,
            expected.email
        ));
        matches = false;
    }
    if matches && identity.name.is_some() && identity.email.is_some() {
        doctor.ok("the identity in this repository matches cloakroom's rules");
    }
    Ok(())
}
