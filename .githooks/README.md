# Git hooks

Local hooks enforcing the pre-commit checks Maarten asked for in the
WhatsApp group on 2026-04-17: `cargo fmt` + `cargo clippy` before every
commit that touches Rust code.

## One-time setup

```bash
git config core.hooksPath .githooks
```

This directs git to look here for hooks instead of `.git/hooks/` (the
default, which isn't versioned). Every contributor runs this once per
clone.

## What's enforced

### `pre-commit`

Runs only when the commit stages Rust sources (`*.rs`, `Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`). Steps:

1. `rustfmt --check` on each staged `.rs` file (fast, no full workspace
   build).
2. `cargo clippy --workspace --all-targets -- -D warnings` (uses the
   shared build cache — slow on a cold clone, fast on subsequent
   commits).

Clippy on a cold cache is slow. Skip with `git commit --no-verify` when
iterating; CI will re-run the full check on your PR anyway.

## Why hooks and not `cargo-husky`?

This repo is a messaging-provider workspace, not an application.
Adding `cargo-husky` as a dev-dep would pull extra crates into
`Cargo.lock` and bind the hook install to a `cargo build`. Plain
bash hooks + one `git config` line achieves the same result without
widening dependencies.
