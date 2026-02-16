//! CLI parsing and command dispatch for sic.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::doctor::{run_doctor_checks, resolve_lockfile_path};
use crate::failure::{emit_failure, OutputFormat};
use crate::prefix::resolve_root;
use crate::term;
use crate::resolver::{LockfileMode, PlanAction, Request, UpgradePolicy};
use crate::storage::{InstalledDb, Lockfile};
use crate::transaction::{Transaction, TransactionType};
use crate::{ensure_layout, load_packages_from_dir, resolve, resolve_remove, stage_plan};
use crate::{AvailablePackages, Plan, SystemPackages};

/// Exit code: success.
pub const EXIT_OK: i32 = 0;
/// Exit code: resolver failure (e.g. unsatisfiable, conflict).
pub const EXIT_RESOLVER: i32 = 1;
/// Exit code: fetch, stage, or commit failure.
pub const EXIT_EXEC: i32 = 2;
/// Exit code: usage error, I/O error, or other.
pub const EXIT_OTHER: i32 = 3;

/// Userland package manager for ~/.local
#[derive(Parser, Debug)]
#[command(name = "sic", version, about)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Args, Debug, Clone)]
pub struct GlobalOpts {
    /// Installation prefix (default: SIC_ROOT or ~/.local/sic)
    #[arg(long, value_name = "PATH")]
    pub prefix: Option<PathBuf>,

    /// Directory containing package.toml files (default: <prefix>/packages or ./packages)
    #[arg(long, value_name = "DIR")]
    pub packages: Option<PathBuf>,

    /// Path to sic.lock for strict/flexible resolution (optional)
    #[arg(long, value_name = "PATH")]
    pub lockfile: Option<PathBuf>,

    /// Lockfile mode when --lockfile is set: strict or flexible
    #[arg(long, default_value = "strict", value_name = "MODE")]
    pub lockfile_mode: LockfileModeArg,

    /// Output format for failures and status: human, json, or toml
    #[arg(long, default_value = "human", value_name = "FMT")]
    pub output: OutputFormatArg,

    /// Only resolve and print plan; do not fetch or commit
    #[arg(long)]
    pub dry_run: bool,

    /// Verbose resolver and fetch logging
    #[arg(short = 'v', long = "verbose", alias = "debug")]
    pub verbose: bool,

    /// Non-interactive: assume yes for any prompt; never block on user input (for scripts/CI).
    /// Future prompts must skip when this is set; when false and not a TTY, fail with "use --yes for non-interactive use" rather than blocking.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// When to use colored output: auto (TTY and no NO_COLOR), never, or always
    #[arg(long, default_value = "auto", value_name = "WHEN")]
    pub color: ColorModeArg,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColorModeArg {
    Auto,
    Never,
    Always,
}

impl std::str::FromStr for ColorModeArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(ColorModeArg::Auto),
            "never" => Ok(ColorModeArg::Never),
            "always" => Ok(ColorModeArg::Always),
            _ => Err(format!("expected 'auto', 'never', or 'always', got '{}'", s)),
        }
    }
}

impl ColorModeArg {
    /// Whether to use color for stderr. For Auto: false if NO_COLOR set, else stderr is TTY.
    pub fn use_color_stderr(&self) -> bool {
        match self {
            ColorModeArg::Never => false,
            ColorModeArg::Always => true,
            ColorModeArg::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
            }
        }
    }

    /// Whether to use color for stdout. For Auto: false if NO_COLOR set, else stdout is TTY.
    pub fn use_color_stdout(&self) -> bool {
        match self {
            ColorModeArg::Never => false,
            ColorModeArg::Always => true,
            ColorModeArg::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockfileModeArg {
    Strict,
    Flexible,
}

impl std::str::FromStr for LockfileModeArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "strict" => Ok(LockfileModeArg::Strict),
            "flexible" => Ok(LockfileModeArg::Flexible),
            _ => Err(format!("expected 'strict' or 'flexible', got '{}'", s)),
        }
    }
}

impl From<LockfileModeArg> for LockfileMode {
    fn from(a: LockfileModeArg) -> Self {
        match a {
            LockfileModeArg::Strict => LockfileMode::Strict,
            LockfileModeArg::Flexible => LockfileMode::Flexible(UpgradePolicy::AllowUpgrade),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputFormatArg {
    Human,
    Json,
    Toml,
}

impl std::str::FromStr for OutputFormatArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "human" => Ok(OutputFormatArg::Human),
            "json" => Ok(OutputFormatArg::Json),
            "toml" => Ok(OutputFormatArg::Toml),
            _ => Err(format!("expected 'human', 'json', or 'toml', got '{}'", s)),
        }
    }
}

impl From<OutputFormatArg> for OutputFormat {
    fn from(a: OutputFormatArg) -> Self {
        match a {
            OutputFormatArg::Human => OutputFormat::Human,
            OutputFormatArg::Json => OutputFormat::Json,
            OutputFormatArg::Toml => OutputFormat::Toml,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Install package(s) by name
    Install {
        /// Package name(s) to install
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// Upgrade package(s) to latest; omit name to upgrade all installed
    Upgrade {
        /// Package name to upgrade (default: all installed)
        #[arg()]
        name: Option<String>,
    },

    /// Remove installed package(s)
    Remove {
        /// Package name(s) to remove
        #[arg(required = true)]
        packages: Vec<String>,

        /// Remove even if dependents exist (leaves broken deps)
        #[arg(short, long)]
        force: bool,
    },

    /// List installed packages with versions
    Status,

    /// Run resolver only and print plan; no fetch or commit
    ResolveOnly {
        /// Package name for install plan (omit for upgrade-all plan)
        #[arg()]
        name: Option<String>,
    },

    /// Check prefix layout, lockfile vs installed, and broken symlinks
    Doctor,

    /// List available packages from packages dir (no install)
    Search {
        /// Optional filter: show only packages whose name contains this (case-insensitive)
        #[arg()]
        pattern: Option<String>,
    },
}

/// Resolves prefix: global --prefix or resolve_root().
pub fn resolve_prefix(global: &GlobalOpts) -> PathBuf {
    global
        .prefix
        .clone()
        .unwrap_or_else(resolve_root)
}

/// Resolves packages dir: global --packages or prefix/packages or ./packages.
pub fn resolve_packages_dir(global: &GlobalOpts, prefix: &Path) -> PathBuf {
    if let Some(ref p) = global.packages {
        return p.clone();
    }
    let prefix_packages = prefix.join("packages");
    if prefix_packages.is_dir() {
        return prefix_packages;
    }
    PathBuf::from("packages")
}

/// Emits a warning to stderr for each sic package in the plan that shadows a system package (same name).
pub fn emit_shadowing_warnings(plan: &Plan, system: &SystemPackages) {
    for step in &plan.steps {
        if step.action == PlanAction::Remove {
            continue;
        }
        if system.get(&step.name).is_some() {
            eprintln!(
                "sic: warning: sic package {} shadows system package {}; may affect ABI",
                step.name.as_str(),
                step.name.as_str(),
            );
        }
    }
}

/// Loads installed DB, lockfile (if any), and packages; runs resolver and optionally executes.
pub fn run_install(
    global: &GlobalOpts,
    prefix: &Path,
    names: &[String],
) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let use_color_stdout = global.color.use_color_stdout();
    let output_fmt: OutputFormat = global.output.clone().into();
    let packages_dir = resolve_packages_dir(global, prefix);
    let packages = match load_packages_from_dir(&packages_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to read packages from {}: {}", packages_dir.display(), e),
            );
            return EXIT_OTHER;
        }
    };
    if packages.is_empty() {
        let _ = term::write_error(
            &mut std::io::stderr(),
            use_color_stderr,
            &format!("sic: no packages found in {}", packages_dir.display()),
        );
        return EXIT_OTHER;
    }
    let available = AvailablePackages::from_packages(packages);
    let mut installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to load installed.toml: {}", e),
            );
            return EXIT_OTHER;
        }
    };
    let lockfile_path: Option<PathBuf> = global.lockfile.clone().or_else(|| {
        let lf = prefix.join("sic.lock");
        if lf.is_file() { Some(lf) } else { None }
    });
    let lockfile_input = match lockfile_path.as_deref() {
        Some(p) => match Lockfile::load(p) {
            Ok(Some(lf)) => Some((lf, global.lockfile_mode.clone().into())),
            Ok(None) => None,
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: failed to load lockfile {}: {}", p.display(), e),
                );
                return EXIT_OTHER;
            }
        },
        None => None,
    };
    let system = SystemPackages::load_default();

    for name in names {
        let pkg_name = match crate::PackageName::new(name) {
            Ok(n) => n,
            Err(_) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: invalid package name: {}", name),
                );
                return EXIT_OTHER;
            }
        };
        let request = Request::Install { name: pkg_name };
        let plan = match resolve(request, &available, &installed, lockfile_input.as_ref().map(|(lf, m)| (lf, *m)), Some(&system), global.verbose) {
            Ok(p) => p,
            Err(failure) => {
                let _ = emit_failure(&failure, output_fmt, &mut std::io::stderr(), use_color_stderr);
                return EXIT_RESOLVER;
            }
        };
        emit_shadowing_warnings(&plan, &system);
        if global.dry_run {
            print_plan(&plan, output_fmt);
            continue;
        }
        if plan.steps.is_empty() {
            continue;
        }
        if let Err(e) = run_plan(prefix, &plan, TransactionType::Install, global.verbose) {
            let _ = term::write_error(&mut std::io::stderr(), use_color_stderr, &format!("sic: {}", e));
            return EXIT_EXEC;
        }
        installed = match InstalledDb::load(prefix) {
            Ok(db) => db,
            Err(_) => installed,
        };
        for step in &plan.steps {
            if step.action != PlanAction::Remove {
                let _ = term::write_success(
                    &mut std::io::stdout(),
                    use_color_stdout,
                    &format!("Installed {} {}", step.name.as_str(), step.version.as_str()),
                );
            }
        }
    }
    EXIT_OK
}

/// Runs upgrade for one name or upgrade-all.
pub fn run_upgrade(global: &GlobalOpts, prefix: &Path, name: Option<&str>) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let use_color_stdout = global.color.use_color_stdout();
    let output_fmt: OutputFormat = global.output.clone().into();
    let packages_dir = resolve_packages_dir(global, prefix);
    let packages = match load_packages_from_dir(&packages_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to read packages from {}: {}", packages_dir.display(), e),
            );
            return EXIT_OTHER;
        }
    };
    if packages.is_empty() {
        let _ = term::write_error(
            &mut std::io::stderr(),
            use_color_stderr,
            &format!("sic: no packages found in {}", packages_dir.display()),
        );
        return EXIT_OTHER;
    }
    let available = AvailablePackages::from_packages(packages);
    let installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to load installed.toml: {}", e),
            );
            return EXIT_OTHER;
        }
    };
    let lockfile_path: Option<PathBuf> = global.lockfile.clone().or_else(|| {
        let lf = prefix.join("sic.lock");
        if lf.is_file() { Some(lf) } else { None }
    });
    let lockfile_input = match lockfile_path.as_deref() {
        Some(p) => match Lockfile::load(p) {
            Ok(Some(lf)) => Some((lf, global.lockfile_mode.clone().into())),
            Ok(None) => None,
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: failed to load lockfile {}: {}", p.display(), e),
                );
                return EXIT_OTHER;
            }
        },
        None => None,
    };
    let system = SystemPackages::load_default();

    let request = match name {
        Some(n) => {
            let pkg_name = match crate::PackageName::new(n) {
                Ok(n) => n,
                Err(_) => {
                    let _ = term::write_error(
                        &mut std::io::stderr(),
                        use_color_stderr,
                        &format!("sic: invalid package name: {}", n),
                    );
                    return EXIT_OTHER;
                }
            };
            Request::Upgrade { name: pkg_name }
        }
        None => Request::UpgradeAll,
    };
    let plan = match resolve(request, &available, &installed, lockfile_input.as_ref().map(|(lf, m)| (lf, *m)), Some(&system), global.verbose) {
        Ok(p) => p,
        Err(failure) => {
            let _ = emit_failure(&failure, output_fmt, &mut std::io::stderr(), use_color_stderr);
            return EXIT_RESOLVER;
        }
    };
    emit_shadowing_warnings(&plan, &system);
    if global.dry_run {
        print_plan(&plan, output_fmt);
        return EXIT_OK;
    }
    if plan.steps.is_empty() {
        println!("Nothing to upgrade.");
        return EXIT_OK;
    }
    if let Err(e) = run_plan(prefix, &plan, TransactionType::Upgrade, global.verbose) {
        let _ = term::write_error(&mut std::io::stderr(), use_color_stderr, &format!("sic: {}", e));
        return EXIT_EXEC;
    }
    for step in &plan.steps {
        if step.action == PlanAction::Upgrade {
            let _ = term::write_success(
                &mut std::io::stdout(),
                use_color_stdout,
                &format!("Upgraded {} to {}", step.name.as_str(), step.version.as_str()),
            );
        }
    }
    EXIT_OK
}

/// Runs remove for given names.
pub fn run_remove(global: &GlobalOpts, prefix: &Path, names: &[String], force: bool) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let use_color_stdout = global.color.use_color_stdout();
    let output_fmt: OutputFormat = global.output.clone().into();
    let packages_dir = resolve_packages_dir(global, prefix);
    let packages = match load_packages_from_dir(&packages_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to read packages from {}: {}", packages_dir.display(), e),
            );
            return EXIT_OTHER;
        }
    };
    let available = AvailablePackages::from_packages(packages);
    let mut installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to load installed.toml: {}", e),
            );
            return EXIT_OTHER;
        }
    };

    for name in names {
        let pkg_name = match crate::PackageName::new(name) {
            Ok(n) => n,
            Err(_) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: invalid package name: {}", name),
                );
                return EXIT_OTHER;
            }
        };
        let plan = match resolve_remove(pkg_name, force, &available, &installed, global.verbose) {
            Ok(p) => p,
            Err(failure) => {
                let _ = emit_failure(&failure, output_fmt, &mut std::io::stderr(), use_color_stderr);
                return EXIT_RESOLVER;
            }
        };
        if global.dry_run {
            print_plan(&plan, output_fmt);
            continue;
        }
        if let Err(e) = run_plan(prefix, &plan, TransactionType::Remove, global.verbose) {
            let _ = term::write_error(&mut std::io::stderr(), use_color_stderr, &format!("sic: {}", e));
            return EXIT_EXEC;
        }
        installed = match InstalledDb::load(prefix) {
            Ok(db) => db,
            Err(_) => installed,
        };
        for step in &plan.steps {
            if step.action == PlanAction::Remove {
                let _ = term::write_success(
                    &mut std::io::stdout(),
                    use_color_stdout,
                    &format!("Removed {} {}", step.name.as_str(), step.version.as_str()),
                );
            }
        }
    }
    EXIT_OK
}

/// Lockfile status for one installed package: match, mismatch (locked vs installed), or not in lockfile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockfileStatus {
    Match,
    Mismatch,
    NotInLockfile,
}

/// Returns (lockfile_status, locked_version) for an installed entry when a lockfile is present.
fn status_lockfile_info<'a>(
    entry: &crate::storage::InstalledEntry,
    lockfile: &'a Lockfile,
) -> (LockfileStatus, Option<&'a crate::version::Version>) {
    let locked = lockfile.packages_for_name(&entry.name);
    match locked.first() {
        None => (LockfileStatus::NotInLockfile, None),
        Some(p) => {
            if p.version == entry.version {
                (LockfileStatus::Match, Some(&p.version))
            } else {
                (LockfileStatus::Mismatch, Some(&p.version))
            }
        }
    }
}

/// Prints status (installed packages). When --lockfile or prefix/sic.lock exists, shows locked vs installed.
pub fn run_status(global: &GlobalOpts, prefix: &Path) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let output_fmt: OutputFormat = global.output.clone().into();
    let installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to load installed.toml: {}", e),
            );
            return EXIT_OTHER;
        }
    };
    let lockfile_path: Option<PathBuf> = global.lockfile.clone().or_else(|| {
        let lf = prefix.join("sic.lock");
        if lf.is_file() {
            Some(lf)
        } else {
            None
        }
    });
    let lockfile_opt = match lockfile_path.as_deref() {
        Some(p) => match Lockfile::load(p) {
            Ok(Some(lf)) => Some(lf),
            Ok(None) => None,
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: failed to load lockfile {}: {}", p.display(), e),
                );
                return EXIT_OTHER;
            }
        },
        None => None,
    };
    let entries = installed.list_all();
    if output_fmt == OutputFormat::Json {
        let list: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "name": e.name.as_str(),
                    "version": e.version.as_str(),
                    "revision": e.revision,
                    "install_path": e.install_path,
                });
                if let Some(ref lf) = lockfile_opt {
                    let (status, locked_ver) = status_lockfile_info(e, lf);
                    obj["lockfile_status"] = serde_json::json!(match status {
                        LockfileStatus::Match => "match",
                        LockfileStatus::Mismatch => "mismatch",
                        LockfileStatus::NotInLockfile => "not_in_lockfile",
                    });
                    if let Some(v) = locked_ver {
                        obj["locked_version"] = serde_json::json!(v.as_str());
                    }
                }
                obj
            })
            .collect();
        match serde_json::to_string_pretty(&list) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else if output_fmt == OutputFormat::Toml {
        let list: Vec<toml::Value> = entries
            .iter()
            .map(|e| {
                let mut row: toml::map::Map<String, toml::Value> = [
                    ("name".to_string(), toml::Value::String(e.name.as_str().to_string())),
                    ("version".to_string(), toml::Value::String(e.version.as_str().to_string())),
                    ("revision".to_string(), toml::Value::Integer(e.revision as i64)),
                    ("install_path".to_string(), toml::Value::String(e.install_path.clone())),
                ]
                .into_iter()
                .collect();
                if let Some(ref lf) = lockfile_opt {
                    let (status, locked_ver) = status_lockfile_info(e, lf);
                    row.insert(
                        "lockfile_status".to_string(),
                        toml::Value::String(
                            match status {
                                LockfileStatus::Match => "match",
                                LockfileStatus::Mismatch => "mismatch",
                                LockfileStatus::NotInLockfile => "not_in_lockfile",
                            }
                            .to_string(),
                        ),
                    );
                    if let Some(v) = locked_ver {
                        row.insert(
                            "locked_version".to_string(),
                            toml::Value::String(v.as_str().to_string()),
                        );
                    }
                }
                toml::Value::Table(row)
            })
            .collect();
        let root = toml::map::Map::from_iter([("packages".to_string(), toml::Value::Array(list))]);
        match toml::to_string_pretty(&toml::Value::Table(root)) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else {
        for e in entries {
            if let Some(ref lf) = lockfile_opt {
                let (status, locked_ver) = status_lockfile_info(e, lf);
                let suffix = match status {
                    LockfileStatus::Match => format!("\t(locked {})", locked_ver.unwrap().as_str()),
                    LockfileStatus::Mismatch => format!(
                        "\tlocked {}, installed {}",
                        locked_ver.unwrap().as_str(),
                        e.version.as_str()
                    ),
                    LockfileStatus::NotInLockfile => "\t(not in lockfile)".to_string(),
                };
                println!("{}\t{}{}", e.name.as_str(), e.version.as_str(), suffix);
            } else {
                println!("{}\t{}", e.name.as_str(), e.version.as_str());
            }
        }
    }
    EXIT_OK
}

/// Lists available packages from packages dir. No fetch or install.
pub fn run_search(
    global: &GlobalOpts,
    prefix: &Path,
    pattern: Option<&str>,
) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let output_fmt: OutputFormat = global.output.clone().into();
    let packages_dir = resolve_packages_dir(global, prefix);
    let packages = match load_packages_from_dir(&packages_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to read packages from {}: {}", packages_dir.display(), e),
            );
            return EXIT_OTHER;
        }
    };
    if packages.is_empty() {
        let _ = term::write_error(
            &mut std::io::stderr(),
            use_color_stderr,
            &format!("sic: no packages found in {}", packages_dir.display()),
        );
        return EXIT_OTHER;
    }
    let available = AvailablePackages::from_packages(packages);
    let pattern_lower = pattern.map(|p| p.to_lowercase());
    let mut results: Vec<_> = available
        .all_names()
        .filter_map(|name| {
            let latest = available.get_latest(name)?;
            if let Some(ref pat) = pattern_lower {
                if !name.as_str().to_lowercase().contains(pat) {
                    return None;
                }
            }
            Some((name.clone(), latest.version.clone()))
        })
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    if output_fmt == OutputFormat::Json {
        let list: Vec<serde_json::Value> = results
            .iter()
            .map(|(name, version)| {
                serde_json::json!({
                    "name": name.as_str(),
                    "version": version.as_str(),
                })
            })
            .collect();
        let root = serde_json::json!({ "packages": list });
        match serde_json::to_string_pretty(&root) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else if output_fmt == OutputFormat::Toml {
        let list: Vec<toml::Value> = results
            .iter()
            .map(|(name, version)| {
                toml::Value::Table(
                    [
                        ("name".to_string(), toml::Value::String(name.as_str().to_string())),
                        ("version".to_string(), toml::Value::String(version.as_str().to_string())),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .collect();
        let root = toml::map::Map::from_iter([("packages".to_string(), toml::Value::Array(list))]);
        match toml::to_string_pretty(&toml::Value::Table(root)) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else {
        for (name, version) in &results {
            println!("{}\t{}", name.as_str(), version.as_str());
        }
    }
    EXIT_OK
}

/// Runs doctor checks (layout, lockfile vs installed, broken symlinks) and prints result.
pub fn run_doctor(global: &GlobalOpts, prefix: &Path) -> i32 {
    let output_fmt: OutputFormat = global.output.clone().into();
    let lockfile_path = resolve_lockfile_path(prefix, global.lockfile.as_ref());
    let result = run_doctor_checks(prefix, lockfile_path);
    if output_fmt == OutputFormat::Json {
        match serde_json::to_string_pretty(&result) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let use_color = global.color.use_color_stderr();
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else if output_fmt == OutputFormat::Toml {
        let root = toml::map::Map::from_iter([
            (
                "layout".to_string(),
                toml::Value::Table(
                    [
                        ("ok".to_string(), toml::Value::Boolean(result.layout.ok)),
                        (
                            "issues".to_string(),
                            toml::Value::Array(
                                result
                                    .layout
                                    .issues
                                    .iter()
                                    .map(|s| toml::Value::String(s.clone()))
                                    .collect(),
                            ),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                "lockfile".to_string(),
                toml::Value::Table(
                    [
                        ("ok".to_string(), toml::Value::Boolean(result.lockfile.ok)),
                        (
                            "issues".to_string(),
                            toml::Value::Array(
                                result
                                    .lockfile
                                    .issues
                                    .iter()
                                    .map(|s| toml::Value::String(s.clone()))
                                    .collect(),
                            ),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                "symlinks".to_string(),
                toml::Value::Table(
                    [
                        ("ok".to_string(), toml::Value::Boolean(result.symlinks.ok)),
                        (
                            "issues".to_string(),
                            toml::Value::Array(
                                result
                                    .symlinks
                                    .issues
                                    .iter()
                                    .map(|s| toml::Value::String(s.clone()))
                                    .collect(),
                            ),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ),
        ]);
        match toml::to_string_pretty(&toml::Value::Table(root)) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                let use_color = global.color.use_color_stderr();
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color,
                    &format!("sic: {}", e),
                );
                return EXIT_OTHER;
            }
        }
    } else {
        if result.layout.ok {
            println!("Layout: OK");
        } else {
            println!(
                "Layout: {} issue(s):",
                result.layout.issues.len()
            );
            for i in &result.layout.issues {
                println!("  {}", i);
            }
        }
        if result.lockfile.ok {
            println!("Lockfile vs installed: OK");
        } else {
            println!(
                "Lockfile vs installed: {} issue(s):",
                result.lockfile.issues.len()
            );
            for i in &result.lockfile.issues {
                println!("  {}", i);
            }
        }
        if result.symlinks.ok {
            println!("Symlinks: OK");
        } else {
            println!(
                "Symlinks: {} issue(s):",
                result.symlinks.issues.len()
            );
            for i in &result.symlinks.issues {
                println!("  {}", i);
            }
        }
    }
    if result.overall_ok() {
        EXIT_OK
    } else {
        EXIT_OTHER
    }
}

/// Resolve only; print plan and exit.
pub fn run_resolve_only(global: &GlobalOpts, prefix: &Path, name: Option<&str>) -> i32 {
    let use_color_stderr = global.color.use_color_stderr();
    let output_fmt: OutputFormat = global.output.clone().into();
    let packages_dir = resolve_packages_dir(global, prefix);
    let packages = match load_packages_from_dir(&packages_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to read packages from {}: {}", packages_dir.display(), e),
            );
            return EXIT_OTHER;
        }
    };
    if packages.is_empty() {
        let _ = term::write_error(
            &mut std::io::stderr(),
            use_color_stderr,
            &format!("sic: no packages found in {}", packages_dir.display()),
        );
        return EXIT_OTHER;
    }
    let available = AvailablePackages::from_packages(packages);
    let installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            let _ = term::write_error(
                &mut std::io::stderr(),
                use_color_stderr,
                &format!("sic: failed to load installed.toml: {}", e),
            );
            return EXIT_OTHER;
        }
    };
    let lockfile_path: Option<PathBuf> = global.lockfile.clone().or_else(|| {
        let lf = prefix.join("sic.lock");
        if lf.is_file() { Some(lf) } else { None }
    });
    let lockfile_input = match lockfile_path.as_deref() {
        Some(p) => match Lockfile::load(p) {
            Ok(Some(lf)) => Some((lf, global.lockfile_mode.clone().into())),
            Ok(None) => None,
            Err(e) => {
                let _ = term::write_error(
                    &mut std::io::stderr(),
                    use_color_stderr,
                    &format!("sic: failed to load lockfile {}: {}", p.display(), e),
                );
                return EXIT_OTHER;
            }
        },
        None => None,
    };
    let system = SystemPackages::load_default();

    let request = match name {
        Some(n) => {
            let pkg_name = match crate::PackageName::new(n) {
                Ok(n) => n,
                Err(_) => {
                    let _ = term::write_error(
                        &mut std::io::stderr(),
                        use_color_stderr,
                        &format!("sic: invalid package name: {}", n),
                    );
                    return EXIT_OTHER;
                }
            };
            Request::Install { name: pkg_name }
        }
        None => Request::UpgradeAll,
    };
    let plan = match resolve(request, &available, &installed, lockfile_input.as_ref().map(|(lf, m)| (lf, *m)), Some(&system), global.verbose) {
        Ok(p) => p,
        Err(failure) => {
            let _ = emit_failure(&failure, output_fmt, &mut std::io::stderr(), use_color_stderr);
            return EXIT_RESOLVER;
        }
    };
    print_plan(&plan, output_fmt);
    EXIT_OK
}

fn print_plan(plan: &Plan, output_fmt: OutputFormat) {
    if output_fmt == OutputFormat::Json {
        let steps: Vec<serde_json::Value> = plan
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name.as_str(),
                    "version": s.version.as_str(),
                    "revision": s.revision,
                    "action": format!("{:?}", s.action),
                })
            })
            .collect();
        let out = serde_json::json!({ "steps": steps, "satisfied_by_system": plan.satisfied_by_system.iter().map(|(n, v)| serde_json::json!({ "name": n.as_str(), "version": v.as_str() })).collect::<Vec<_>>() });
        if let Ok(s) = serde_json::to_string_pretty(&out) {
            println!("{}", s);
        }
    } else if output_fmt == OutputFormat::Toml {
        let steps: Vec<toml::Value> = plan
            .steps
            .iter()
            .map(|s| {
                toml::Value::Table(
                    [
                        ("name".to_string(), toml::Value::String(s.name.as_str().to_string())),
                        ("version".to_string(), toml::Value::String(s.version.as_str().to_string())),
                        ("revision".to_string(), toml::Value::Integer(s.revision as i64)),
                        ("action".to_string(), toml::Value::String(format!("{:?}", s.action))),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .collect();
        let root = toml::map::Map::from_iter([
            ("steps".to_string(), toml::Value::Array(steps)),
            (
                "satisfied_by_system".to_string(),
                toml::Value::Array(
                    plan.satisfied_by_system
                        .iter()
                        .map(|(n, v)| {
                            toml::Value::Table(
                                [
                                    ("name".to_string(), toml::Value::String(n.as_str().to_string())),
                                    ("version".to_string(), toml::Value::String(v.as_str().to_string())),
                                ]
                                .into_iter()
                                .collect(),
                            )
                        })
                        .collect(),
                ),
            ),
        ]);
        if let Ok(s) = toml::to_string_pretty(&toml::Value::Table(root)) {
            println!("{}", s);
        }
    } else {
        for step in &plan.steps {
            println!("  {} {} ({:?})", step.name.as_str(), step.version.as_str(), step.action);
        }
        if !plan.satisfied_by_system.is_empty() {
            println!("  satisfied by system: {:?}", plan.satisfied_by_system);
        }
    }
}

/// Runs a plan: ensure layout, create transaction, stage, commit.
fn run_plan(
    prefix: &Path,
    plan: &Plan,
    tx_type: TransactionType,
    verbose: bool,
) -> Result<(), crate::TransactionError> {
    ensure_layout(prefix).map_err(|e| crate::TransactionError::msg(format!("layout: {}", e)))?;
    let mut tx = Transaction::new(tx_type, plan.clone(), prefix)?;
    let show_progress = std::io::stderr().is_terminal();
    stage_plan(prefix, tx.id, plan, verbose, show_progress)
        .map_err(|e| crate::TransactionError::msg(e.to_string()))?;
    tx.commit(prefix)?;
    Ok(())
}
