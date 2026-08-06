//! The argument surface. Doc comments on `Command` are `--help` text, so they
//! are written for users; maintainer notes belong in `app`, which implements
//! each one.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "cloakroom",
    version,
    about = "Context-dependent Git identities compiled to native Git conditional includes."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the configuration and add the managed include to the global gitconfig
    Init,
    /// Validate the TOML and generate native Git configuration
    Apply,
    /// Explain the effective identity in the current repository
    Status,
    /// Detect invalid configuration, conflicts, and unexpected identities
    Doctor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_subcommand_parses() {
        // Compared by discriminant because Command carries no data and
        // deriving PartialEq on it would only exist for this assertion.
        for (argument, expected) in [
            ("init", Command::Init),
            ("apply", Command::Apply),
            ("status", Command::Status),
            ("doctor", Command::Doctor),
        ] {
            let cli = Cli::try_parse_from(["cloakroom", argument]).unwrap();
            assert!(
                std::mem::discriminant(&cli.command) == std::mem::discriminant(&expected),
                "{argument} parsed to the wrong command"
            );
        }
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["cloakroom", "unknown"]).is_err());
    }
}
