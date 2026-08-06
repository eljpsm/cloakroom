//! Explain the identity Git resolves and whether cloakroom supplied it.
//!
//! Answers one question: what will git put on the next commit here, and did
//! cloakroom put it there. Everything is read back through git, so the answer
//! is what git will actually do, not what the config says it should do.
//!
//! `doctor` reuses the three helpers at the bottom, so both commands agree on
//! what counts as cloakroom's own file and on when two values share a source.

use std::path::Path;

use crate::app::RunStatus;
use crate::git::{self, ConfigEntry};
use crate::paths::Layout;

/// Lines to print and the status they add up to. Building both here keeps
/// `app` free of any judgement about what is worth a nonzero exit.
pub(crate) struct Report {
    pub lines: Vec<String>,
    pub status: RunStatus,
}

/// Issues, not failures: an incomplete or overridden identity is a true
/// report of a real state, so it exits 1 rather than erroring out.
pub(crate) fn report(cwd: &Path, layout: &Layout) -> anyhow::Result<Report> {
    let mut lines = Vec::new();

    let toplevel = git::repo_toplevel(cwd)?;
    match &toplevel {
        Some(root) => lines.push(format!("repository: {}", root.display())),
        None => lines.push(
            "repository: none (not inside a git repository; showing the fall-through identity)"
                .to_owned(),
        ),
    }

    let identity = git::effective_identity(cwd)?;
    lines.push(match (&identity.name, &identity.email) {
        (Some(name), Some(email)) => format!("identity:   {} <{}>", name.value, email.value),
        (Some(name), None) => format!("identity:   {} (user.email is not set)", name.value),
        (None, Some(email)) => format!("identity:   <{}> (user.name is not set)", email.value),
        (None, None) => "identity:   none (git resolves no user.name or user.email)".to_owned(),
    });

    let mut status = if identity.name.is_none() || identity.email.is_none() {
        RunStatus::Issues
    } else {
        RunStatus::Clean
    };

    match &identity.profile {
        None => {
            let source = identity.email.as_ref().or(identity.name.as_ref());
            match source {
                Some(entry) => lines.push(format!(
                    "profile:    none (identity from {}, scope {})",
                    origin_display(entry),
                    entry.scope
                )),
                None => lines.push("profile:    none".to_owned()),
            }
        }
        Some(profile) => {
            // A marker alone proves nothing. The values must come from the
            // same generated file the marker did: git resolves each key
            // independently, so a later scope can replace user.name while
            // leaving the marker behind, and the identity would then be
            // reported as cloakroom's when it is not.
            let marker_is_generated = is_generated_marker(profile, layout);
            let name_matches = identity
                .name
                .as_ref()
                .is_some_and(|name| marker_is_generated && same_origin(name, profile));
            let email_matches = identity
                .email
                .as_ref()
                .is_some_and(|email| marker_is_generated && same_origin(email, profile));
            if name_matches && email_matches {
                lines.push(format!(
                    "profile:    {} (selected by cloakroom)",
                    profile.value
                ));
            } else {
                lines.push(format!(
                    "profile:    {} selected, but the identity is overridden",
                    profile.value
                ));
                if let Some(name) = &identity.name
                    && !name_matches
                {
                    lines.push(format!(
                        "name from:  {} (scope {})",
                        origin_display(name),
                        name.scope
                    ));
                }
                if let Some(email) = &identity.email
                    && !email_matches
                {
                    lines.push(format!(
                        "email from: {} (scope {})",
                        origin_display(email),
                        email.scope
                    ));
                }
                status = RunStatus::Issues;
            }
        }
    }

    Ok(Report { lines, status })
}

/// Whether two resolved values came from the same file. Scope alone is not
/// enough: several global-scope files are in play once includes are involved.
pub(crate) fn same_origin(left: &ConfigEntry, right: &ConfigEntry) -> bool {
    left.scope == right.scope && left.origin == right.origin
}

/// Whether this value came from a file cloakroom generated: directly inside
/// objects/, named `<64 hex>.gitconfig`.
///
/// `cloakroom.profile` is an ordinary config key that anyone can set. Trusting
/// it on sight would let a hand-written gitconfig claim to be a profile, so
/// reports check where it came from instead of what it says. Both paths are
/// canonicalized, so a symlinked config directory still matches.
pub(crate) fn is_generated_marker(entry: &ConfigEntry, layout: &Layout) -> bool {
    let Some(origin) = entry.origin.as_deref() else {
        return false;
    };
    let Ok(origin) = std::fs::canonicalize(origin) else {
        return false;
    };
    let Ok(objects) = std::fs::canonicalize(layout.objects_dir()) else {
        return false;
    };
    origin.parent() == Some(objects.as_path())
        && origin
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".gitconfig"))
            .is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

/// A printable source. Values set on the command line or read from a blob
/// have no file, and a report has to say something about them anyway.
pub(crate) fn origin_display(entry: &ConfigEntry) -> String {
    entry.origin.as_deref().map_or_else(
        || "a non-file origin".to_owned(),
        |path| path.display().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn entry(scope: &str, origin: Option<&str>) -> ConfigEntry {
        ConfigEntry {
            scope: scope.to_owned(),
            origin: origin.map(PathBuf::from),
            value: "value".to_owned(),
        }
    }

    #[test]
    fn matching_scope_and_origin_identify_one_source() {
        assert!(same_origin(
            &entry("global", Some("profile")),
            &entry("global", Some("profile"))
        ));
        assert!(!same_origin(
            &entry("local", Some("repo")),
            &entry("global", Some("profile"))
        ));
    }

    /// Paths that do not exist cannot be canonicalized, which is the same
    /// answer wanted here: no origin, no proof.
    #[test]
    fn markers_need_a_real_generated_object_origin() {
        let layout = Layout {
            config_dir: PathBuf::from("/cloakroom-test/missing-config"),
            include_path: "unused".to_owned(),
        };
        assert!(!is_generated_marker(&entry("command", None), &layout));
        assert!(!is_generated_marker(
            &entry("global", Some("/cloakroom-test/missing-object")),
            &layout
        ));
    }

    #[test]
    fn non_file_origins_have_a_readable_label() {
        assert_eq!(origin_display(&entry("command", None)), "a non-file origin");
    }
}
