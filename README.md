# cloakroom

Context-dependent Git identities compiled to native Git conditional includes.

## Install

```bash
# Straight from GitHub:
cargo install --git https://github.com/eljpsm/cloakroom

# With Nix:
nix run github:eljpsm/cloakroom

# From a clone (installs to ~/.cargo/bin):
make install
```

Remote (`remotes`) rules use Git's `hasconfig` condition, which needs Git `2.36`
or newer. Path rules work on any Git with conditional includes.

## Usage

```bash
# Create ~/.config/cloakroom/config.toml and add one include to the global
# gitconfig.
cloakroom init

# Validate the config and regenerate the gitconfig files.
cloakroom apply

# Explain which identity Git resolves here and which profile provided it.
cloakroom status

# Check config, generated files, the managed include, and the identity in
# the current repository.
cloakroom doctor
```

| Exit code | Description                                             |
| --------- | ------------------------------------------------------- |
| `0`       | Clean.                                                  |
| `1`       | Issues found.                                           |
| `2`       | Operational failure (invalid config, git not runnable). |

## Configuration

`~/.config/cloakroom/config.toml` (or `$XDG_CONFIG_HOME/cloakroom/`):

```toml
[profiles.personal]
name = "Your Name"
email = "you@example.com"

[profiles.work]
name = "Your Name"
email = "you@work.example.com"

# Select by repository location. Git expands the leading ~ and a trailing /
# matches every repository underneath. Set case_insensitive = true to match
# case-insensitively (gitdir/i).
[[rules]]
profile = "work"
path = "~/src/work/"

# Select by remote URL, SSH or HTTP(S). Patterns are Git wildmatch globs.
[[rules]]
profile = "work"
remotes = [
  "git@github.com:example/**",
  "https://github.com/example/**",
]
```

- A rule has either `path` or `remotes`, never both.
- When several rules match a repository, the last one in the file wins,
  mirroring Git's own last-include-wins order.
- A profile with no rules is allowed; include its generated file yourself or use
  it as documentation.

```bash
# Remove the global include line added by init, leaving only the conditional includes.
git config --global --unset include.path '~/.config/cloakroom/generated/root.gitconfig'

# Remove the generated config directory.
rm -r ~/.config/cloakroom
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
