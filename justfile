# sic — build and lint targets
# Usage: just <recipe>

default:
    just --list

build:
    cargo build

test:
    cargo test

lint:
    cargo fmt --check
    cargo clippy -- -D warnings

fmt:
    cargo fmt

# Build debug binary; run with: just debug -- status (or any subcommand)
debug:
    cargo build && ./target/debug/sic {{ _args... }}
