//! Where cloakroom's files live and how the managed include refers to them.

use std::path::PathBuf;

use anyhow::Context;

/// Where cloakroom keeps its files, and the string it writes into the global
/// gitconfig to reach them. Every path cloakroom touches comes from here, so
/// pointing HOME or XDG_CONFIG_HOME elsewhere relocates the whole tree. The
/// tests rely on that.
pub(crate) struct Layout {
    /// Absolute directory holding config.toml and generated/.
    pub config_dir: PathBuf,
    /// The include.path value written to the global gitconfig. A literal
    /// `~/...` when derived from HOME, so the user's gitconfig survives a
    /// home move; absolute when XDG_CONFIG_HOME overrides it, because git
    /// never expands that variable in include paths.
    pub include_path: String,
}

impl Layout {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        // The XDG spec says a relative XDG_CONFIG_HOME must be ignored.
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        Self::new(xdg, std::env::home_dir())
    }

    /// Split out from `from_env` so the environment can be varied in tests
    /// without touching process-wide state.
    fn new(xdg: Option<PathBuf>, home: Option<PathBuf>) -> anyhow::Result<Self> {
        if let Some(xdg) = xdg {
            let config_dir = xdg.join("cloakroom");
            let include_path = format!(
                "{}/generated/root.gitconfig",
                config_dir
                    .to_str()
                    .context("XDG_CONFIG_HOME is not valid UTF-8")?
            );
            return Ok(Layout {
                config_dir,
                include_path,
            });
        }
        let home = home.context("cannot determine the home directory")?;
        Ok(Layout {
            config_dir: home.join(".config/cloakroom"),
            include_path: "~/.config/cloakroom/generated/root.gitconfig".to_owned(),
        })
    }

    // Everything cloakroom owns hangs off config_dir. No other module builds
    // one of these paths by hand.

    pub(crate) fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub(crate) fn generated_dir(&self) -> PathBuf {
        self.config_dir.join("generated")
    }

    pub(crate) fn root_gitconfig(&self) -> PathBuf {
        self.generated_dir().join("root.gitconfig")
    }

    pub(crate) fn objects_dir(&self) -> PathBuf {
        self.generated_dir().join("objects")
    }

    pub(crate) fn object_gitconfig(&self, digest: &str) -> PathBuf {
        self.objects_dir().join(format!("{digest}.gitconfig"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_config_home_overrides_home_and_makes_the_include_absolute() {
        let layout = Layout::new(
            Some(PathBuf::from("/custom/config")),
            Some(PathBuf::from("/home/user")),
        )
        .unwrap();
        assert_eq!(layout.config_dir, PathBuf::from("/custom/config/cloakroom"));
        assert_eq!(
            layout.include_path,
            "/custom/config/cloakroom/generated/root.gitconfig"
        );
    }

    #[test]
    fn home_alone_yields_a_tilde_include_that_git_expands() {
        let layout = Layout::new(None, Some(PathBuf::from("/home/user"))).unwrap();
        assert_eq!(
            layout.config_dir,
            PathBuf::from("/home/user/.config/cloakroom")
        );
        assert_eq!(
            layout.include_path,
            "~/.config/cloakroom/generated/root.gitconfig"
        );
    }

    #[test]
    fn no_home_at_all_is_an_error() {
        assert!(Layout::new(None, None).is_err());
    }

    #[test]
    fn derived_paths_hang_off_the_config_dir() {
        let layout = Layout::new(None, Some(PathBuf::from("/h"))).unwrap();
        assert_eq!(
            layout.config_file(),
            PathBuf::from("/h/.config/cloakroom/config.toml")
        );
        assert_eq!(
            layout.object_gitconfig("abc"),
            PathBuf::from("/h/.config/cloakroom/generated/objects/abc.gitconfig")
        );
    }
}
