//! cloakroom: context-dependent Git identities compiled to native Git
//! conditional includes.
//!
//! Pipeline: `cli` parses arguments and `app::run` drives the rest. `config`
//! loads and validates the TOML, `render` turns it into gitconfig text,
//! `file_io` writes it atomically, and `git` shells out to the installed git
//! for everything cloakroom refuses to reimplement (`status`, `doctor`).
//!
//! The tree cloakroom owns, under the config directory `paths` resolves:
//!
//! ```text
//! config.toml                             hand edited, the only source of truth
//! generated/root.gitconfig                includeIf conditions, nothing else
//! generated/objects/<sha256>.gitconfig    one [user] block per profile
//! ```
//!
//! Only root.gitconfig is named in the user's global gitconfig, and the
//! includes inside it are relative. So the whole mechanism hangs off one
//! `include.path` line, and removing that line removes cloakroom from git.
//!
//! Nothing runs at commit time. Once `apply` has written the tree, git does
//! all the matching by itself; cloakroom is only ever a compiler and a
//! reporter.
//!
//! Exit codes are `app::RunStatus`: 0 clean, 1 issues, 2 failure.

use std::process::ExitCode;

use clap::Parser;

mod app;
mod cli;
mod config;
mod doctor;
mod file_io;
mod git;
mod paths;
mod render;
mod status;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    app::run(cli).into()
}
