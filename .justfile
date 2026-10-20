set quiet := true

default:
    just --list

patch:
    test "$(git branch --show-current)" = "main"
    test -z "$(git status --porcelain)"
    cargo release version patch --execute
    cargo release replace --execute --no-confirm
    cargo release commit --execute --no-confirm
    cargo release tag --execute --no-confirm
    cargo release push --execute

ci:
    cargo fmt --all -- --check
    cargo check --workspace --all-targets --all-features
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo nextest run --workspace --all-targets --all-features --no-fail-fast
