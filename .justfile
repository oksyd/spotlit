set quiet := true

default:
    just --list

ci:
    cargo fmt --all -- --check
    cargo check --workspace --all-targets --all-features
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo nextest run --workspace --all-targets --all-features --no-fail-fast
