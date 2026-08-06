//! Atomic, deterministic writes for the generated directory. Content is
//! written to a temporary file beside the target and renamed into place, so
//! a crash never leaves git reading a half-written include.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

use tempfile::Builder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    /// The contents differed and the file was replaced.
    Wrote,
    /// The file already held these bytes and was left alone.
    Unchanged,
}

/// Skips the write entirely when the bytes already match, so repeated
/// applies leave mtimes alone and can report "unchanged" honestly.
///
/// The temporary file is created in the target's own directory, because
/// rename is only atomic within a filesystem. `tempfile` removes it on any
/// error path, so a failure leaves the old file and nothing else.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> io::Result<WriteOutcome> {
    if std::fs::read(path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(WriteOutcome::Unchanged);
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )
    })?;
    let mut temporary = Builder::new()
        .prefix(".cloakroom-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temporary.as_file_mut().write_all(contents.as_bytes())?;
    temporary.persist(path).map_err(|err| err.error)?;
    Ok(WriteOutcome::Wrote)
}

/// Remove object gitconfigs whose digest is no longer in the manifest. Only
/// `*.gitconfig` files are touched; anything else in the directory is not
/// cloakroom's to delete. Returns the removed file names, sorted.
///
/// Call this only after the new root is in place. Until then the old root is
/// still live and the objects it names must still exist.
pub(crate) fn prune_objects(dir: &Path, keep: &BTreeSet<String>) -> io::Result<Vec<String>> {
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(key) = name.strip_suffix(".gitconfig") else {
            continue;
        };
        if !keep.contains(key) {
            std::fs::remove_file(&path)?;
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_creates_and_then_reports_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root.gitconfig");
        assert_eq!(
            write_atomic(&path, "content\n").unwrap(),
            WriteOutcome::Wrote
        );
        assert_eq!(
            write_atomic(&path, "content\n").unwrap(),
            WriteOutcome::Unchanged
        );
        assert_eq!(
            write_atomic(&path, "changed\n").unwrap(),
            WriteOutcome::Wrote
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed\n");
    }

    /// `prune_objects` only removes `*.gitconfig`, so a leaked temporary
    /// would sit in objects/ forever.
    #[test]
    fn no_temporary_files_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root.gitconfig");
        write_atomic(&path, "content\n").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["root.gitconfig"]);
    }

    #[test]
    fn prune_removes_only_stale_gitconfig_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.gitconfig"), "").unwrap();
        std::fs::write(dir.path().join("stale.gitconfig"), "").unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "").unwrap();
        let keep = BTreeSet::from(["keep".to_owned()]);
        let removed = prune_objects(dir.path(), &keep).unwrap();
        assert_eq!(removed, ["stale.gitconfig"]);
        assert!(dir.path().join("keep.gitconfig").exists());
        assert!(dir.path().join("unrelated.txt").exists());
    }
}
