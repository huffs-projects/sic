//! sic — userland package manager library.

use std::io::IsTerminal;

pub mod cli;
pub mod dep_constraint;
pub mod doctor;
pub mod fetch;
pub mod failure;
pub mod term;
pub mod manifest;
pub mod package_name;
pub mod prefix;
pub mod resolver;
pub mod source;
pub mod stage;
pub mod storage;
pub mod system_packages;
pub mod transaction;
pub mod version;

pub use dep_constraint::{DepConstraint, DepOp, InvalidDepConstraint};
pub use manifest::{
    load_packages_from_dir, parse, parse_and_validate, parse_path, parse_path_and_validate,
    validate, Manifest, ParseError, ParseOrValidationError, ValidationError,
};
pub use package_name::{InvalidPackageName, PackageName};
pub use prefix::{check_layout, ensure_layout, resolve_root};
pub use failure::{
    default_suggested_action, emit_failure, format_human, to_json, to_toml, FailureKind,
    FailureSerializeError, OutputFormat, ResolverFailure,
};
pub use resolver::{
    plan_to_lockfile, resolve, resolve_remove, AvailablePackages, LockfileInput, LockfileMode,
    Plan, PlanAction,
    PlanStep, Request, UpgradePolicy,
};
pub use source::{InvalidSourceHash, Source, SourceHash};
pub use system_packages::{SystemPackages, DPKG_STATUS_PATH};
pub use fetch::{compute_file_hash, fetch_to_cache, verify_file_hash, FetchError};
pub use stage::{stage_plan, StageError};
pub use storage::{
    cache_path, InstalledDb, InstalledEntry, InstalledLoadError, InstalledWriteError, Lockfile,
    LockfileLoadError, LockfilePackage, LockfileWriteError,
};
pub use transaction::{
    acquire_lock, backup_dir, installed_backup_path, staging_path, transaction_log_path,
    LockGuard, Transaction, TransactionError, TransactionState, TransactionType,
};
pub use version::{EmptyVersion, Version};

/// Runs the CLI with the given args; parses, dispatches to subcommand, returns exit code.
pub fn run_with_args(args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>) -> i32 {
    use clap::Parser;
    let args: Vec<_> = args.into_iter().map(Into::into).collect();
    let cli = match cli::Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let use_color = std::env::var_os("NO_COLOR").is_none()
                && std::io::stderr().is_terminal();
            let _ = crate::term::write_error(
                &mut std::io::stderr(),
                use_color,
                &e.to_string(),
            );
            return cli::EXIT_OTHER;
        }
    };
    let prefix = cli::resolve_prefix(&cli.global);
    match &cli.command {
        cli::Command::Install { packages } => cli::run_install(&cli.global, &prefix, packages),
        cli::Command::Upgrade { name } => cli::run_upgrade(&cli.global, &prefix, name.as_deref()),
        cli::Command::Remove { packages, force } => cli::run_remove(&cli.global, &prefix, packages, *force),
        cli::Command::Status => cli::run_status(&cli.global, &prefix),
        cli::Command::ResolveOnly { name } => cli::run_resolve_only(&cli.global, &prefix, name.as_deref()),
        cli::Command::Doctor => cli::run_doctor(&cli.global, &prefix),
        cli::Command::Search { pattern } => cli::run_search(&cli.global, &prefix, pattern.as_deref()),
    }
}

/// Runs the CLI with std::env::args(); entry point for the binary.
pub fn run() -> i32 {
    run_with_args(std::env::args_os().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("SIC_ROOT", tmp.path());
        let code = run_with_args(["sic", "status"]);
        std::env::remove_var("SIC_ROOT");
        assert_eq!(code, 0);
    }
}
