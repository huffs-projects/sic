# sic

Userland package manager for ~/.local: installs, upgrades, and removes packages under a user prefix without touching the system. Debian and derivatives are supported: sic reads the dpkg database to satisfy dependencies with system packages when possible. On non-Debian systems or when dpkg is not present, the system package set is treated as empty and resolution uses only sic packages.

## PATH

Add the sic bin directory to your PATH so installed binaries are available:

```bash
export PATH="$HOME/.local/sic/bin:$PATH"
```

Or set `SIC_ROOT` to a custom prefix (e.g. `export SIC_ROOT=$HOME/.local/sic`) and add `$SIC_ROOT/bin` to PATH. User is responsible for PATH order: put the sic bin directory first if you want sic-installed packages to take precedence over system ones.

## Build

```bash
cargo build
```

Or: `just build`

Release build: `cargo build --release`

## Usage

- **install** \<name\> [names...] — Resolve and install package(s). Uses packages from `--packages` (default: \<prefix\>/packages or ./packages).
- **upgrade** [name] — Upgrade one package or all installed (omit name for upgrade-all).
- **remove** \<name\> [names...] — Remove package(s). Fails if dependents exist unless `--force`.
- **status** — List installed packages (human table, or `--output json` / `--output toml`). When a lockfile exists (`--lockfile` or prefix/sic.lock), human output shows locked vs installed; machine output includes `lockfile_status` (match, mismatch, not_in_lockfile) and optional `locked_version`.
- **resolve-only** [name] — Run resolver only and print plan; no fetch or commit.

Global options: `--prefix PATH`, `--packages DIR`, `--lockfile PATH`, `--lockfile-mode strict|flexible`, `--output human|json|toml`, `--dry-run`.

## Lockfile

When `sic.lock` exists (in prefix or via `--lockfile`), resolution can be strict or flexible:

- **strict** — Only versions (and revisions) in the lockfile are allowed. Use for reproducible installs.
- **flexible** — Allow upgrades (e.g. version >= locked) that still satisfy constraints. Use for `upgrade` while respecting the lockfile.

## Failure output

On resolver failure (unsatisfiable, conflict, cycle, not in lockfile, has dependents), sic prints a structured message to stderr. Use `--output json` or `--output toml` for machine-readable output. The message includes a suggested action (e.g. "remove dependents first or use --force" for has-dependents). For scripting, use `--output json` and check the exit code.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | Resolver failure (unsatisfiable, conflict, cycle, not in lockfile, has dependents) |
| 2 | Fetch, stage, or commit failure |
| 3 | Usage error, I/O error, or other |

## Test

```bash
cargo test
```

Or: `just test`

Requires Rust 1.70+.

## Debug

Build and run the debug binary (e.g. to attach a debugger or use `RUST_BACKTRACE=1`):

```bash
cargo build
./target/debug/sic status
```

Or: `just debug -- status` (or `just debug -- install foo`, etc.)

## License

GPL 3.0. See [LICENSE](LICENSE) for details.

## Contributor

- Run tests: `cargo test`. Integration tests in `tests/` run the CLI against fixture packages and tarballs.
