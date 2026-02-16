//! Resolver: in-memory dependency resolution from request + available packages + installed set.
//!
//! Produces a deterministic Plan (ordered install/upgrade steps) or Failure. No I/O; no lockfile
//! or dpkg in this phase. Recommends are left for a later phase.
//! When system packages are provided (Phase 6), implicit deps can be satisfied by system.

use std::collections::{BTreeMap, BTreeSet};

use crate::manifest::Manifest;
use crate::package_name::PackageName;
use crate::source::Source;
use crate::storage::{InstalledDb, Lockfile, LockfilePackage};
use crate::system_packages::SystemPackages;
use crate::version::Version;

/// User intent: install, upgrade, upgrade all, or remove a package by name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Install { name: PackageName },
    Upgrade { name: PackageName },
    /// Upgrade every installed package to latest available (within lockfile if strict).
    UpgradeAll,
    /// Remove a package; fails if dependents exist unless force is true.
    Remove {
        name: PackageName,
        /// If true, remove anyway and leave broken dependents.
        force: bool,
    },
}

/// One step in the execution plan: package identity, revision, source, files, and action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanStep {
    pub name: PackageName,
    pub version: Version,
    pub revision: u32,
    pub source: Source,
    /// Relative paths to install from the unpacked artifact (e.g. `bin/helix`, `share/helix/*`).
    pub files: Vec<String>,
    pub action: PlanAction,
}

/// Action to perform for this package in the plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanAction {
    Install,
    Upgrade,
    Remove,
}

/// Ordered list of package actions (dependencies before dependents).
/// Dependencies satisfied by system packages are listed in `satisfied_by_system`, not in `steps`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    /// Dependencies satisfied by system (e.g. dpkg) packages; never proposed for install.
    pub satisfied_by_system: Vec<(PackageName, Version)>,
}

/// When lockfile is present, whether to allow only locked versions or allow upgrades.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockfileMode {
    /// Only versions (and revisions) present in the lockfile are allowed.
    Strict,
    /// Allow upgrades within the given policy (e.g. version >= locked that satisfies constraint).
    Flexible(UpgradePolicy),
}

/// Policy for flexible lockfile mode: which upgrades are allowed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradePolicy {
    /// Allow any version >= locked version that satisfies the dependency constraint.
    AllowUpgrade,
}

/// Lockfile input to the resolver: optional lockfile plus mode when present.
pub type LockfileInput<'a> = Option<(&'a Lockfile, LockfileMode)>;

/// Index of available packages by package name. All versions per name, sorted for deterministic
/// "latest" (max by Version).
#[derive(Clone, Debug, Default)]
pub struct AvailablePackages {
    by_name: BTreeMap<PackageName, Vec<Manifest>>,
}

impl AvailablePackages {
    /// Builds an index from a list of packages. Caller must ensure packages are validated.
    /// Multiple versions of the same package are stored; duplicates (same name+version) are
    /// coalesced (last wins) for simplicity.
    pub fn from_packages(packages: impl IntoIterator<Item = Manifest>) -> Self {
        let mut by_name: BTreeMap<PackageName, Vec<Manifest>> = BTreeMap::new();
        for m in packages {
            by_name.entry(m.name.clone()).or_default().push(m);
        }
        for vers in by_name.values_mut() {
            vers.sort_by(|a, b| a.version.cmp(&b.version));
        }
        AvailablePackages { by_name }
    }

    /// Returns all known versions for a package name, in ascending version order.
    pub fn get_versions(&self, name: &PackageName) -> Option<&[Manifest]> {
        self.by_name.get(name).map(|v| v.as_slice())
    }

    /// Returns the package with the highest version for the given name.
    pub fn get_latest(&self, name: &PackageName) -> Option<&Manifest> {
        self.by_name.get(name).and_then(|v| v.last())
    }

    /// Returns true if there is a concrete package with this name.
    pub fn has_package(&self, name: &PackageName) -> bool {
        self.by_name.contains_key(name)
    }

    /// Iterator over all package names (for providers lookup).
    pub fn all_names(&self) -> impl Iterator<Item = &PackageName> {
        self.by_name.keys()
    }

    /// Find all packages that provide the given virtual name.
    pub fn providers_of(&self, virtual_name: &str) -> Vec<&Manifest> {
        self.by_name
            .values()
            .flat_map(|v| v.iter())
            .filter(|m| m.provides.iter().any(|p| p == virtual_name))
            .collect()
    }
}

/// Resolves a user request against available packages and installed set. No I/O.
/// When `lockfile` is `Some`, resolution is constrained by the lockfile and mode (strict or flexible).
/// When `system` is `Some`, implicit dependencies can be satisfied by system packages (prefer system).
pub fn resolve<'a>(
    request: Request,
    available: &'a AvailablePackages,
    installed: &InstalledDb,
    lockfile: LockfileInput<'a>,
    system: Option<&SystemPackages>,
    verbose: bool,
) -> Result<Plan, crate::failure::ResolverFailure> {
    if verbose {
        let lock_mode = lockfile
            .as_ref()
            .map(|(_, m)| format!("{:?}", m))
            .unwrap_or_else(|| "none".to_string());
        eprintln!(
            "resolve: request {:?}, lockfile mode {}",
            request, lock_mode
        );
    }
    match &request {
        Request::Remove { name, force } => {
            return resolve_remove(name.clone(), *force, available, installed, verbose);
        }
        Request::UpgradeAll => {
            return resolve_upgrade_all(available, installed, lockfile, system, verbose);
        }
        _ => {}
    }

    let (name, prefer_upgrade) = match &request {
        Request::Install { name } => (name.clone(), false),
        Request::Upgrade { name } => (name.clone(), true),
        _ => unreachable!(),
    };

    let mut selected: BTreeMap<PackageName, (Version, &'a Manifest)> = BTreeMap::new();
    let mut visiting: BTreeSet<PackageName> = BTreeSet::new();
    let mut satisfied_system: BTreeSet<(PackageName, Version)> = BTreeSet::new();

    resolve_one(
        &name,
        None,
        prefer_upgrade,
        true, // is_explicit: root request
        available,
        installed,
        &lockfile,
        system,
        &mut selected,
        &mut visiting,
        &mut satisfied_system,
        verbose,
    )?;

    check_conflicts(&selected)?;

    let steps = topological_plan(&selected, installed);
    let satisfied_by_system: Vec<(PackageName, Version)> = satisfied_system.into_iter().collect();
    if verbose {
        eprintln!(
            "resolve: plan has {} steps, {} satisfied by system",
            steps.len(),
            satisfied_by_system.len()
        );
    }
    Ok(Plan {
        steps,
        satisfied_by_system,
    })
}

/// Resolves "upgrade all": resolve upgrade for each installed package and merge into one plan.
fn resolve_upgrade_all<'a>(
    available: &'a AvailablePackages,
    installed: &InstalledDb,
    lockfile: LockfileInput<'a>,
    system: Option<&SystemPackages>,
    verbose: bool,
) -> Result<Plan, crate::failure::ResolverFailure> {
    let names: Vec<PackageName> = installed.list_all().iter().map(|e| e.name.clone()).collect();
    if names.is_empty() {
        return Ok(Plan::default());
    }
    let mut selected: BTreeMap<PackageName, (Version, &'a Manifest)> = BTreeMap::new();
    let mut visiting: BTreeSet<PackageName> = BTreeSet::new();
    let mut satisfied_system: BTreeSet<(PackageName, Version)> = BTreeSet::new();
    for name in &names {
        visiting.clear();
        resolve_one(
            name,
            None,
            true, // prefer_upgrade
            true, // is_explicit
            available,
            installed,
            &lockfile,
            system,
            &mut selected,
            &mut visiting,
            &mut satisfied_system,
            verbose,
        )?;
    }
    check_conflicts(&selected)?;
    let steps = topological_plan(&selected, installed);
    let satisfied_by_system: Vec<(PackageName, Version)> = satisfied_system.into_iter().collect();
    if verbose {
        eprintln!(
            "resolve: plan has {} steps, {} satisfied by system",
            steps.len(),
            satisfied_by_system.len()
        );
    }
    Ok(Plan {
        steps,
        satisfied_by_system,
    })
}

/// Returns installed packages that depend on the given package name (via manifest depends/depends_any).
fn dependents_of(
    name: &PackageName,
    available: &AvailablePackages,
    installed: &InstalledDb,
) -> Vec<PackageName> {
    let mut out = Vec::new();
    for entry in installed.list_all() {
        let manifest = available
            .get_versions(&entry.name)
            .and_then(|vers| vers.iter().find(|m| m.version == entry.version));
        let Some(m) = manifest else { continue };
        let depends_on_name = m.depends.iter().any(|d| &d.name == name)
            || m.depends_any
                .iter()
                .any(|group| group.iter().any(|d| &d.name == name));
        if depends_on_name {
            out.push(entry.name.clone());
        }
    }
    out
}

/// Resolves "remove name": check no dependents (unless force), then plan a single Remove step.
pub fn resolve_remove(
    name: PackageName,
    force: bool,
    available: &AvailablePackages,
    installed: &InstalledDb,
    verbose: bool,
) -> Result<Plan, crate::failure::ResolverFailure> {
    if verbose {
        eprintln!("resolve: remove {} (force={})", name.as_str(), force);
    }
    let entry = installed.get_by_name(&name).ok_or_else(|| {
        crate::failure::ResolverFailure::other(
            Some(name.as_str()),
            Some("package not installed"),
        )
    })?;
    let dependents = dependents_of(&name, available, installed);
    if !force && !dependents.is_empty() {
        let names: Vec<String> = dependents.iter().map(|n| n.as_str().to_string()).collect();
        return Err(crate::failure::ResolverFailure::has_dependents(
            name.as_str(),
            names,
        ));
    }
    let step = PlanStep {
        name: entry.name.clone(),
        version: entry.version.clone(),
        revision: entry.revision,
        source: crate::source::Source {
            type_name: "none".to_string(),
            url: String::new(),
            hash: crate::source::SourceHash::parse("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap(),
        },
        files: vec![],
        action: PlanAction::Remove,
    };
    Ok(Plan {
        steps: vec![step],
        satisfied_by_system: vec![],
    })
}

/// Resolve a single dependency: by name (and optional version constraint). If the name is a
/// virtual (no concrete package), look up providers.
/// When !is_explicit and system is Some, prefer satisfying from system before sic.
#[allow(clippy::too_many_arguments)]
fn resolve_one<'a>(
    name: &PackageName,
    constraint: Option<&crate::dep_constraint::DepConstraint>,
    prefer_upgrade: bool,
    is_explicit: bool,
    available: &'a AvailablePackages,
    installed: &InstalledDb,
    lockfile: &LockfileInput<'a>,
    system: Option<&SystemPackages>,
    selected: &mut BTreeMap<PackageName, (Version, &'a Manifest)>,
    visiting: &mut BTreeSet<PackageName>,
    satisfied_system: &mut BTreeSet<(PackageName, Version)>,
    verbose: bool,
) -> Result<(), crate::failure::ResolverFailure> {
    // Cycle detection first: if we are currently expanding this name, we have a cycle.
    if visiting.contains(name) {
        return Err(crate::failure::ResolverFailure::cycle(name.as_str()));
    }
    // If we already selected a package for this name, ensure it satisfies the constraint if any.
    if let Some((ver, _manifest)) = selected.get(name) {
        if let Some(c) = constraint {
            if c.name != *name {
                return Ok(());
            }
            if !c.satisfies(ver) {
                return Err(crate::failure::ResolverFailure::unsatisfiable(
                    name.as_str(),
                    Some(format!("{}", c)),
                    Some(vec![ver.clone()]),
                    None,
                ));
            }
        }
        return Ok(());
    }
    // Implicit dep: prefer system if it satisfies the constraint. Do not add to selected.
    if !is_explicit {
        if let Some(sys) = system {
            if let Some(sys_ver) = sys.get(name) {
                if constraint
                    .map(|c| c.name == *name && c.satisfies(&sys_ver))
                    .unwrap_or(true)
                {
                    if verbose {
                        eprintln!(
                            "resolve: satisfied by system {} {}",
                            name.as_str(),
                            sys_ver.as_str()
                        );
                    }
                    satisfied_system.insert((name.clone(), sys_ver));
                    return Ok(());
                }
            }
        }
    }
    visiting.insert(name.clone());

    let manifest = match choose_manifest(
        name,
        constraint,
        prefer_upgrade,
        available,
        installed,
        lockfile,
    ) {
        Ok(m) => m,
        Err(e) => {
            visiting.remove(name);
            return Err(e);
        }
    };
    let version = manifest.version.clone();
    if verbose {
        eprintln!(
            "resolve: selected {} {}",
            name.as_str(),
            version.as_str()
        );
    }
    selected.insert(name.clone(), (version.clone(), manifest));

    // Resolve depends
    for dep in &manifest.depends {
        if let Err(e) = resolve_one(
            &dep.name,
            Some(dep),
            prefer_upgrade,
            false, // implicit
            available,
            installed,
            lockfile,
            system,
            selected,
            visiting,
            satisfied_system,
            verbose,
        ) {
            visiting.remove(name);
            return Err(e);
        }
    }

    // Resolve depends_any: first satisfiable alternative (all constraints in that alternative).
    // On failure of an alternative, roll back any selected entries and satisfied_system entries
    // added during that alternative.
    let mut any_satisfied = false;
    for group in &manifest.depends_any {
        let keys_before_alt: BTreeSet<PackageName> = selected.keys().cloned().collect();
        let satisfied_system_before: BTreeSet<(PackageName, Version)> =
            satisfied_system.iter().cloned().collect();
        let mut this_alt_ok = true;
        for c in group {
            let name_for_dep = &c.name;
            if let Some((ver, _)) = selected.get(name_for_dep) {
                if !c.satisfies(ver) {
                    this_alt_ok = false;
                    break;
                }
            } else if resolve_one(
                name_for_dep,
                Some(c),
                prefer_upgrade,
                false, // implicit
                available,
                installed,
                lockfile,
                system,
                selected,
                visiting,
                satisfied_system,
                verbose,
            )
            .is_err()
            {
                this_alt_ok = false;
                break;
            }
        }
        if !this_alt_ok {
            let to_remove: Vec<PackageName> = selected
                .keys()
                .filter(|&k| !keys_before_alt.contains(k))
                .cloned()
                .collect();
            for k in &to_remove {
                selected.remove(k);
                visiting.remove(k);
            }
            satisfied_system.retain(|p| satisfied_system_before.contains(p));
            continue;
        }
        any_satisfied = true;
        break;
    }
    if !any_satisfied && !manifest.depends_any.is_empty() {
        visiting.remove(name);
        return Err(crate::failure::ResolverFailure::unsatisfiable(
            manifest.name.as_str(),
            None::<&str>,
            None,
            Some(vec!["no alternative in depends_any could be satisfied".to_string()]),
        ));
    }

    visiting.remove(name);
    Ok(())
}

/// Choose which manifest to use for a name: locked-first (prefer installed) unless prefer_upgrade
/// or not installed; then latest in available. When lockfile is present, filter to locked versions (strict) or allow upgrades (flexible). For virtuals, pick a provider.
fn choose_manifest<'a>(
    name: &PackageName,
    constraint: Option<&crate::dep_constraint::DepConstraint>,
    prefer_upgrade: bool,
    available: &'a AvailablePackages,
    installed: &InstalledDb,
    lockfile: &LockfileInput<'a>,
) -> Result<&'a Manifest, crate::failure::ResolverFailure> {
    let versions = available.get_versions(name);

    if let Some(vers) = versions {
        let mut candidates: Vec<&Manifest> = vers
            .iter()
            .filter(|m| constraint.map(|c| c.satisfies(&m.version)).unwrap_or(true))
            .collect();
        if let Some((lf, mode)) = lockfile.as_ref() {
            match mode {
                LockfileMode::Strict => {
                    let locked = lf.packages_for_name(name);
                    if locked.is_empty() {
                        return Err(crate::failure::ResolverFailure::not_in_lockfile(name.as_str()));
                    }
                    let allowed_version_revision: std::collections::BTreeSet<(Version, u32)> =
                        locked
                            .iter()
                            .map(|p| (p.version.clone(), p.revision))
                            .collect();
                    candidates.retain(|m| {
                        allowed_version_revision.contains(&(m.version.clone(), m.revision))
                    });
                    if candidates.is_empty() {
                        let locked_vers: Vec<Version> =
                            locked.iter().map(|p| p.version.clone()).collect();
                        return Err(crate::failure::ResolverFailure::unsatisfiable(
                            name.as_str(),
                            constraint.map(|c| format!("{}", c)),
                            Some(locked_vers),
                            Some(vec!["no locked (version, revision) satisfies constraint (strict mode)".to_string()]),
                        ));
                    }
                }
                LockfileMode::Flexible(UpgradePolicy::AllowUpgrade) => {
                    let locked = lf.packages_for_name(name);
                    if !locked.is_empty() {
                        let max_locked =
                            locked.iter().map(|p| &p.version).max_by(|a, b| a.cmp(b));
                        if let Some(min_ver) = max_locked {
                            candidates.retain(|m| m.version >= *min_ver);
                            if candidates.is_empty() {
                                let locked_vers: Vec<Version> =
                                    locked.iter().map(|p| p.version.clone()).collect();
                                return Err(crate::failure::ResolverFailure::unsatisfiable(
                                    name.as_str(),
                                    constraint.map(|c| format!("{}", c)),
                                    Some(locked_vers),
                                    Some(vec![
                                        "no version >= locked satisfies constraint (flexible mode)".to_string(),
                                    ]),
                                ));
                            }
                        }
                    }
                }
            }
        }
        let installed_ver = installed.get_by_name(name);
        if !prefer_upgrade {
            if let Some(entry) = installed_ver {
                if let Some(m) = candidates.iter().find(|m| {
                    m.version == entry.version && m.revision == entry.revision
                }) {
                    return Ok(*m);
                }
            }
        }
        return candidates.last().copied().ok_or_else(|| {
            let available_vers: Vec<Version> = vers.iter().map(|m| m.version.clone()).collect();
            crate::failure::ResolverFailure::unsatisfiable(
                name.as_str(),
                constraint.map(|c| format!("{}", c)),
                Some(available_vers),
                None,
            )
        });
    }

    // Virtual: resolve by provider
    let virtual_name = name.as_str();
    let mut providers: Vec<&Manifest> = available.providers_of(virtual_name);
    if let Some((lf, LockfileMode::Strict)) = lockfile.as_ref() {
        let allowed: std::collections::BTreeSet<(PackageName, Version, u32)> = lf
            .packages
            .iter()
            .map(|p| (p.name.clone(), p.version.clone(), p.revision))
            .collect();
        providers.retain(|m| {
            allowed.contains(&(m.name.clone(), m.version.clone(), m.revision))
        });
        if providers.is_empty() {
            return Err(crate::failure::ResolverFailure::not_in_lockfile(virtual_name));
        }
    } else if let Some((lf, LockfileMode::Flexible(UpgradePolicy::AllowUpgrade))) = lockfile.as_ref() {
        providers.retain(|m| {
            let locked = lf.packages_for_name(&m.name);
            if locked.is_empty() {
                return true;
            }
            let max_locked = locked.iter().map(|p| &p.version).max_by(|a, b| a.cmp(b));
            match max_locked {
                Some(min_ver) => m.version >= *min_ver,
                None => true,
            }
        });
    }
    let installed_ver = installed.get_by_name(name);
    for m in providers.iter().rev() {
        if !constraint.map(|c| c.satisfies(&m.version)).unwrap_or(true) {
            continue;
        }
        if !prefer_upgrade {
            if let Some(entry) = installed_ver {
                if entry.version == m.version && entry.revision == m.revision {
                    return Ok(*m);
                }
            }
        }
        return Ok(*m);
    }
    for m in providers.iter() {
        if constraint.map(|c| c.satisfies(&m.version)).unwrap_or(true) {
            return Ok(*m);
        }
    }

    Err(crate::failure::ResolverFailure::unsatisfiable(
        virtual_name,
        None::<&str>,
        None,
        Some(vec!["package not found".to_string()]),
    ))
}

fn check_conflicts(
    selected: &BTreeMap<PackageName, (Version, &Manifest)>,
) -> Result<(), crate::failure::ResolverFailure> {
    for (name_a, (_, manifest_a)) in selected {
        for conflict_name in &manifest_a.conflicts {
            if selected.contains_key(conflict_name) {
                return Err(crate::failure::ResolverFailure::conflict(
                    name_a.as_str(),
                    Some(vec![conflict_name.as_str().to_string()]),
                ));
            }
        }
    }
    Ok(())
}

/// Build DAG: (name, version) -> list of (name, version) that this package depends on.
/// Then topological sort (dependencies first).
fn topological_plan(
    selected: &BTreeMap<PackageName, (Version, &Manifest)>,
    installed: &InstalledDb,
) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    let mut done: BTreeSet<(PackageName, Version)> = BTreeSet::new();

    fn visit(
        _key_name: &PackageName,
        version: &Version,
        manifest: &Manifest,
        selected: &BTreeMap<PackageName, (Version, &Manifest)>,
        installed: &InstalledDb,
        done: &mut BTreeSet<(PackageName, Version)>,
        steps: &mut Vec<PlanStep>,
    ) {
        let key = (manifest.name.clone(), version.clone());
        if done.contains(&key) {
            return;
        }
        done.insert(key);

        for dep in &manifest.depends {
            if let Some((v, m)) = selected.get(&dep.name) {
                visit(&dep.name, v, m, selected, installed, done, steps);
            }
        }
        for group in &manifest.depends_any {
            for c in group {
                if let Some((v, m)) = selected.get(&c.name) {
                    visit(&c.name, v, m, selected, installed, done, steps);
                    break;
                }
            }
        }

        let concrete_name = &manifest.name;
        let action = if installed.get_by_name(concrete_name).is_some() {
            PlanAction::Upgrade
        } else {
            PlanAction::Install
        };
        steps.push(PlanStep {
            name: concrete_name.clone(),
            version: version.clone(),
            revision: manifest.revision,
            source: manifest.source.clone(),
            files: manifest.files.clone(),
            action,
        });
    }

    for (name, (version, manifest)) in selected {
        visit(
            name, version, manifest, selected, installed, &mut done, &mut steps,
        );
    }
    steps
}

/// Builds a lockfile from a resolved plan (for writing sic.lock).
pub fn plan_to_lockfile(plan: &Plan) -> Lockfile {
    Lockfile {
        packages: plan
            .steps
            .iter()
            .map(|s| LockfilePackage {
                name: s.name.clone(),
                version: s.version.clone(),
                revision: s.revision,
                source: s.source.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep_constraint::DepConstraint;
    use crate::manifest::Manifest;
    use crate::source::SourceHash;
    use crate::storage::{Lockfile, LockfilePackage};
    use crate::system_packages::SystemPackages;
    use crate::version::Version;

    fn manifest_no_deps(name: &str, version: &str) -> Manifest {
        let pkg_name = PackageName::new(name).unwrap();
        let ver = Version::new(version).unwrap();
        Manifest {
            name: pkg_name.clone(),
            version: ver.clone(),
            revision: 0,
            source: crate::source::Source {
                type_name: "tarball".to_string(),
                url: format!("https://example.com/{}.tar.gz", name),
                hash: SourceHash::parse("sha256:deadbeef").unwrap(),
            },
            depends: vec![],
            depends_any: vec![],
            recommends: vec![],
            conflicts: vec![],
            provides: vec![],
            files: vec![],
            commands: vec![],
        }
    }

    #[test]
    fn single_package_no_deps() {
        let m = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![m.clone()]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("foo").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "foo");
        assert_eq!(plan.steps[0].version.as_str(), "1.0");
        assert_eq!(plan.steps[0].action, PlanAction::Install);
    }

    #[test]
    fn two_packages_a_depends_b() {
        let mb = manifest_no_deps("b", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 1.0").unwrap()];
        let available = AvailablePackages::from_packages(vec![ma.clone(), mb.clone()]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].name.as_str(), "b");
        assert_eq!(plan.steps[1].name.as_str(), "a");
    }

    #[test]
    fn conflict_a_conflicts_b_request_both() {
        let mut ma = manifest_no_deps("a", "1.0");
        ma.conflicts = vec![PackageName::new("b").unwrap()];
        let mb = manifest_no_deps("b", "1.0");
        let mut meta = manifest_no_deps("meta", "1.0");
        meta.depends = vec![
            DepConstraint::parse("a >= 1").unwrap(),
            DepConstraint::parse("b >= 1").unwrap(),
        ];
        let available = AvailablePackages::from_packages(vec![ma, mb, meta]);
        let installed = InstalledDb::default();
        let r = resolve(
            Request::Install {
                name: PackageName::new("meta").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        );
        assert!(r.is_err());
        if let Err(ref e) = r {
            assert_eq!(e.kind, crate::failure::FailureKind::Conflict);
        } else {
            panic!("expected Conflict failure, got {:?}", r);
        }
    }

    #[test]
    fn depends_any_only_b_available() {
        let mb = manifest_no_deps("b", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends_any = vec![
            vec![DepConstraint::parse("b >= 1").unwrap()],
            vec![DepConstraint::parse("c >= 1").unwrap()],
        ];
        let available = AvailablePackages::from_packages(vec![ma, mb]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"b"));
        assert!(names.contains(&"a"));
    }

    #[test]
    fn depends_any_first_alternative_fails_rollback_second_in_plan() {
        // depends_any [[b, d], [e]]: first alternative needs b and d; second needs e only.
        // Only b and e available (no d). First alternative fails partway (b succeeds, d fails);
        // roll back must remove b so second alternative yields plan with e only (not b).
        let mb = manifest_no_deps("b", "1.0");
        let me = manifest_no_deps("e", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends_any = vec![
            vec![
                DepConstraint::parse("b >= 1").unwrap(),
                DepConstraint::parse("d >= 1").unwrap(),
            ],
            vec![DepConstraint::parse("e >= 1").unwrap()],
        ];
        let available = AvailablePackages::from_packages(vec![ma, mb, me]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(plan.steps.len(), 2);
        assert!(names.contains(&"e"));
        assert!(names.contains(&"a"));
        assert!(!names.contains(&"b"));
    }

    #[test]
    fn provides_a_depends_editor_c_provides_editor() {
        let mut mc = manifest_no_deps("c", "1.0");
        mc.provides = vec!["editor".to_string()];
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("editor >= 0").unwrap()];
        let available = AvailablePackages::from_packages(vec![ma, mc]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"c"));
        assert!(names.contains(&"a"));
    }

    #[test]
    fn locked_first_b_installed_12_available_12_and_13_install_b_chooses_12() {
        let b12 = manifest_no_deps("b", "12.0");
        let b13 = manifest_no_deps("b", "13.0");
        let available = AvailablePackages::from_packages(vec![b12.clone(), b13]);
        let installed = InstalledDb::from(vec![crate::storage::InstalledEntry {
            name: PackageName::new("b").unwrap(),
            version: Version::new("12.0").unwrap(),
            revision: 0,
            install_path: "pkgs/b-12.0".to_string(),
            files: vec![],
            file_checksums: vec![],
        }]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("b").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].version.as_str(), "12.0");
    }

    #[test]
    fn unsatisfiable_version_constraint_includes_operator() {
        // When no version satisfies the constraint, failure should report constraint with operator.
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 2.0").unwrap()];
        let mb = manifest_no_deps("b", "1.0");
        let available = AvailablePackages::from_packages(vec![ma, mb]);
        let installed = InstalledDb::default();
        let r = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        );
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert_eq!(err.kind, crate::failure::FailureKind::Unsatisfiable);
        assert_eq!(err.version_constraint.as_deref(), Some("b >= 2.0"));
    }

    #[test]
    fn cycle_a_depends_b_b_depends_a_fails() {
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 1").unwrap()];
        let mut mb = manifest_no_deps("b", "1.0");
        mb.depends = vec![DepConstraint::parse("a >= 1").unwrap()];
        let available = AvailablePackages::from_packages(vec![ma, mb]);
        let installed = InstalledDb::default();
        let r = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        );
        assert!(r.is_err());
        if let Err(ref e) = r {
            assert_eq!(e.kind, crate::failure::FailureKind::Cycle);
        } else {
            panic!("expected Cycle failure");
        }
    }

    // --- Phase 5: Lockfile integration tests ---

    #[test]
    fn resolver_with_lockfile_strict_chooses_locked_version() {
        let b12 = manifest_no_deps("b", "12.0");
        let b13 = manifest_no_deps("b", "13.0");
        let available = AvailablePackages::from_packages(vec![b12.clone(), b13]);
        let installed = InstalledDb::default();
        let lockfile = Lockfile {
            packages: vec![LockfilePackage {
                name: PackageName::new("b").unwrap(),
                version: Version::new("12.0").unwrap(),
                revision: 0,
                source: b12.source.clone(),
            }],
        };
        let plan = resolve(
            Request::Install {
                name: PackageName::new("b").unwrap(),
            },
            &available,
            &installed,
            Some((&lockfile, LockfileMode::Strict)),
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "b");
        assert_eq!(plan.steps[0].version.as_str(), "12.0");
    }

    #[test]
    fn strict_version_not_in_lockfile_fails() {
        // a depends on b >= 13; lockfile has only b 12.0; strict mode -> failure (b 13.0 not in lockfile).
        let b12 = manifest_no_deps("b", "12.0");
        let b13 = manifest_no_deps("b", "13.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 13").unwrap()];
        let available =
            AvailablePackages::from_packages(vec![ma, b12.clone(), b13]);
        let installed = InstalledDb::default();
        let lockfile = Lockfile {
            packages: vec![LockfilePackage {
                name: PackageName::new("b").unwrap(),
                version: Version::new("12.0").unwrap(),
                revision: 0,
                source: b12.source.clone(),
            }],
        };
        let r = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            Some((&lockfile, LockfileMode::Strict)),
            None,
            false,
        );
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert!(err.kind == crate::failure::FailureKind::Unsatisfiable
            || err.kind == crate::failure::FailureKind::NotInLockfile);
        assert!(err.package.as_deref() == Some("b") || err.package.as_deref() == Some("a"));
    }

    #[test]
    fn strict_new_package_not_in_lockfile_fails() {
        let foo = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![foo]);
        let installed = InstalledDb::default();
        let lockfile = Lockfile {
            packages: vec![],
        };
        let r = resolve(
            Request::Install {
                name: PackageName::new("foo").unwrap(),
            },
            &available,
            &installed,
            Some((&lockfile, LockfileMode::Strict)),
            None,
            false,
        );
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert_eq!(err.kind, crate::failure::FailureKind::NotInLockfile);
        assert_eq!(err.package.as_deref(), Some("foo"));
    }

    #[test]
    fn flexible_allows_upgrade_plan_has_new_version() {
        let b12 = manifest_no_deps("b", "12.0");
        let b13 = manifest_no_deps("b", "13.0");
        let available = AvailablePackages::from_packages(vec![b12.clone(), b13.clone()]);
        let installed = InstalledDb::default();
        let lockfile = Lockfile {
            packages: vec![LockfilePackage {
                name: PackageName::new("b").unwrap(),
                version: Version::new("12.0").unwrap(),
                revision: 0,
                source: b12.source.clone(),
            }],
        };
        let plan = resolve(
            Request::Upgrade {
                name: PackageName::new("b").unwrap(),
            },
            &available,
            &installed,
            Some((&lockfile, LockfileMode::Flexible(UpgradePolicy::AllowUpgrade))),
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "b");
        assert_eq!(plan.steps[0].version.as_str(), "13.0");
    }

    #[test]
    fn write_lockfile_roundtrip_same_plan() {
        let foo = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![foo.clone()]);
        let installed = InstalledDb::default();
        let plan1 = resolve(
            Request::Install {
                name: PackageName::new("foo").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        let lockfile = plan_to_lockfile(&plan1);
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("sic.lock");
        lockfile.write(&path).unwrap();
        let loaded = Lockfile::load(&path).unwrap().unwrap();
        let plan2 = resolve(
            Request::Install {
                name: PackageName::new("foo").unwrap(),
            },
            &available,
            &installed,
            Some((&loaded, LockfileMode::Strict)),
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan1.steps.len(), plan2.steps.len());
        assert_eq!(plan1.steps[0].name.as_str(), plan2.steps[0].name.as_str());
        assert_eq!(plan1.steps[0].version.as_str(), plan2.steps[0].version.as_str());
    }

    // --- Phase 6: Debian / system packages tests ---

    #[test]
    fn implicit_dep_satisfied_by_system_plan_has_only_requested() {
        // System has ripgrep 13.0; sic has helix depending on ripgrep >= 13; no sic ripgrep.
        let mut helix = manifest_no_deps("helix", "24.03");
        helix.depends = vec![DepConstraint::parse("ripgrep >= 13").unwrap()];
        let available = AvailablePackages::from_packages(vec![helix]);
        let installed = InstalledDb::default();
        let system = SystemPackages::from_map([("ripgrep", "13.0")]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("helix").unwrap(),
            },
            &available,
            &installed,
            None,
            Some(&system),
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "helix");
        let ripgrep = PackageName::new("ripgrep").unwrap();
        assert!(
            plan.satisfied_by_system
                .iter()
                .any(|(n, v)| n == &ripgrep && v.as_str() == "13.0"),
            "ripgrep 13.0 should be in satisfied_by_system"
        );
    }

    #[test]
    fn explicit_request_uses_sic_not_system() {
        // User requests sic install ripgrep. Sic has ripgrep 14.0; system has ripgrep 13.0.
        let ripgrep_sic = manifest_no_deps("ripgrep", "14.0");
        let available = AvailablePackages::from_packages(vec![ripgrep_sic]);
        let installed = InstalledDb::default();
        let system = SystemPackages::from_map([("ripgrep", "13.0")]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("ripgrep").unwrap(),
            },
            &available,
            &installed,
            None,
            Some(&system),
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "ripgrep");
        assert_eq!(plan.steps[0].version.as_str(), "14.0");
        let ripgrep = PackageName::new("ripgrep").unwrap();
        assert!(
            !plan.satisfied_by_system.iter().any(|(n, _)| n == &ripgrep),
            "ripgrep should not be in satisfied_by_system when explicitly requested"
        );
    }

    #[test]
    fn system_none_unchanged_behavior() {
        // resolve(..., system: None) behaves as before: all deps from sic.
        let mb = manifest_no_deps("b", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 1.0").unwrap()];
        let available = AvailablePackages::from_packages(vec![ma.clone(), mb]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.satisfied_by_system.is_empty());
    }

    #[test]
    fn depends_any_one_alternative_satisfied_by_system() {
        // a depends_any [[b], [c]]. System has b 1.0; sic has c 1.0. First alternative (b) satisfied by system -> no b in steps.
        let mc = manifest_no_deps("c", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends_any = vec![
            vec![DepConstraint::parse("b >= 1").unwrap()],
            vec![DepConstraint::parse("c >= 1").unwrap()],
        ];
        let available = AvailablePackages::from_packages(vec![ma, mc]);
        let installed = InstalledDb::default();
        let system = SystemPackages::from_map([("b", "1.0")]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            Some(&system),
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "a");
        let b = PackageName::new("b").unwrap();
        assert!(
            plan.satisfied_by_system.iter().any(|(n, v)| n == &b && v.as_str() == "1.0"),
            "b 1.0 should be in satisfied_by_system"
        );
    }

    #[test]
    fn depends_any_rollback_satisfied_system_when_alternative_fails() {
        // a depends_any [[b, c], [d]]. System has b 1.0; sic has d 1.0 only (no c).
        // First alternative: b satisfied by system, c fails -> roll back. Second: d from sic.
        // satisfied_by_system must not contain b (we rolled back that alternative).
        let md = manifest_no_deps("d", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends_any = vec![
            vec![
                DepConstraint::parse("b >= 1").unwrap(),
                DepConstraint::parse("c >= 1").unwrap(),
            ],
            vec![DepConstraint::parse("d >= 1").unwrap()],
        ];
        let available = AvailablePackages::from_packages(vec![ma, md]);
        let installed = InstalledDb::default();
        let system = SystemPackages::from_map([("b", "1.0")]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            Some(&system),
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"d"));
        assert!(names.contains(&"a"));
        let b = PackageName::new("b").unwrap();
        assert!(
            !plan.satisfied_by_system.iter().any(|(n, _)| n == &b),
            "b must not be in satisfied_by_system after rollback of first alternative"
        );
    }

    #[test]
    fn depends_any_rollback_clears_visiting_so_third_alternative_can_use_same_dep() {
        // a depends_any [[b, c], [d, e], [d]]. First: b from system, c missing -> roll back.
        // Second: d from sic, e missing -> roll back (must clear d from visiting).
        // Third: d from sic -> succeed. Without clearing visiting, resolve_one(d) in third would see d in visiting and return cycle.
        let md = manifest_no_deps("d", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends_any = vec![
            vec![
                DepConstraint::parse("b >= 1").unwrap(),
                DepConstraint::parse("c >= 1").unwrap(),
            ],
            vec![
                DepConstraint::parse("d >= 1").unwrap(),
                DepConstraint::parse("e >= 1").unwrap(),
            ],
            vec![DepConstraint::parse("d >= 1").unwrap()],
        ];
        let available = AvailablePackages::from_packages(vec![ma, md]);
        let installed = InstalledDb::default();
        let system = SystemPackages::from_map([("b", "1.0")]);
        let plan = resolve(
            Request::Install {
                name: PackageName::new("a").unwrap(),
            },
            &available,
            &installed,
            None,
            Some(&system),
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"d"));
        assert!(names.contains(&"a"));
    }

    // --- Phase 10: Upgrade and remove tests ---

    #[test]
    fn remove_when_not_installed_fails() {
        let foo = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![foo]);
        let installed = InstalledDb::default();
        let r = resolve_remove(
            PackageName::new("foo").unwrap(),
            false,
            &available,
            &installed,
            false,
        );
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert_eq!(err.kind, crate::failure::FailureKind::Other);
    }

    #[test]
    fn remove_when_dependents_exist_fails_unless_force() {
        let mb = manifest_no_deps("b", "1.0");
        let mut ma = manifest_no_deps("a", "1.0");
        ma.depends = vec![DepConstraint::parse("b >= 1").unwrap()];
        let available = AvailablePackages::from_packages(vec![ma, mb]);
        let installed = InstalledDb::from(vec![
            crate::storage::InstalledEntry {
                name: PackageName::new("b").unwrap(),
                version: Version::new("1.0").unwrap(),
                revision: 0,
                install_path: "pkgs/b-1.0".to_string(),
                files: vec![],
                file_checksums: vec![],
            },
            crate::storage::InstalledEntry {
                name: PackageName::new("a").unwrap(),
                version: Version::new("1.0").unwrap(),
                revision: 0,
                install_path: "pkgs/a-1.0".to_string(),
                files: vec![],
                file_checksums: vec![],
            },
        ]);
        let r = resolve_remove(
            PackageName::new("b").unwrap(),
            false,
            &available,
            &installed,
            false,
        );
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert_eq!(err.kind, crate::failure::FailureKind::HasDependents);
        assert_eq!(err.package.as_deref(), Some("b"));
        assert!(err.conflicting_packages.as_ref().map(|v| v.contains(&"a".to_string())).unwrap_or(false));

        let plan = resolve_remove(
            PackageName::new("b").unwrap(),
            true, // force
            &available,
            &installed,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "b");
        assert_eq!(plan.steps[0].action, PlanAction::Remove);
    }

    #[test]
    fn remove_when_no_dependents_succeeds() {
        let foo = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![foo.clone()]);
        let installed = InstalledDb::from(vec![crate::storage::InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec![],
            file_checksums: vec![],
        }]);
        let plan = resolve_remove(
            PackageName::new("foo").unwrap(),
            false,
            &available,
            &installed,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "foo");
        assert_eq!(plan.steps[0].version.as_str(), "1.0");
        assert_eq!(plan.steps[0].action, PlanAction::Remove);
    }

    #[test]
    fn upgrade_all_empty_installed_returns_empty_plan() {
        let foo = manifest_no_deps("foo", "1.0");
        let available = AvailablePackages::from_packages(vec![foo]);
        let installed = InstalledDb::default();
        let plan = resolve(
            Request::UpgradeAll,
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn upgrade_all_one_installed_returns_upgrade_steps() {
        let foo1 = manifest_no_deps("foo", "1.0");
        let foo2 = manifest_no_deps("foo", "2.0");
        let available = AvailablePackages::from_packages(vec![foo1, foo2]);
        let installed = InstalledDb::from(vec![crate::storage::InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec![],
            file_checksums: vec![],
        }]);
        let plan = resolve(
            Request::UpgradeAll,
            &available,
            &installed,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name.as_str(), "foo");
        assert_eq!(plan.steps[0].version.as_str(), "2.0");
        assert_eq!(plan.steps[0].action, PlanAction::Upgrade);
    }
}
