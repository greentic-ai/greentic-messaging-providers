# Provider Publish Workflows

`ci/provider-matrix.json` is the single checked-in source of truth for:

- provider name to pack mapping
- provider name to buildable component targets
- provider name to Rust manifests used for targeted `fmt` and `clippy`
- provider-scoped paths that can stay on the fast path

`ci/provider_matrix.py` exposes two commands:

- `python3 ci/provider_matrix.py resolve-provider telegram`
- `python3 ci/provider_matrix.py detect-changes --base <sha> --head <sha>`

Build-all fallback is intentionally conservative. The main workflow flips `build_all=true` when shared crates, shared WIT/build tooling, state/template/common packs, CI workflow files, or any unmapped path changes.

To manually run the fast path in GitHub Actions:

1. Open `Publish Provider Fast Path`
2. Enter a provider such as `telegram`
3. Optionally enable `dry_run`

The fast path builds and lints only the mapped provider components, pulls templates from OCI during pack sync, validates only the mapped pack, and publishes only that provider's components and pack when `dry_run` is disabled.
