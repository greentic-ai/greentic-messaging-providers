# greentic-messaging-providers

Workspace for building messaging provider components that can be packaged and distributed independently. The repository hosts shared Rust crates and provider-specific WebAssembly components; higher-level packs and orchestration live elsewhere.

Current layout:
- `crates/`: shared libraries for message types and provider utilities.
- `crates/component_questions`: CLI-first questions component (WASM) for emitting/validating setup questions.
- `crates/questions-cli`: helper CLI to render QuestionSpec JSON and collect answers.
- `crates/messaging-cardkit`: lightweight CardKit renderer + profile source helpers built on `gsm-core` for rendering MessageCard fixtures without GSM gateway/egress plumbing.
- `crates/messaging-cardkit-bin`: CLI/server companion that renders MessageCard fixtures via CardKit, exposes `render` and `serve` subcommands, and ships the same fixtures used by the golden tests.
- `crates/greentic-messaging-cardkit`: installable wrapper that delegates to `messaging-cardkit-bin`, making the CLI available via `cargo binstall greentic-messaging-cardkit`.
- `crates/messaging-universal-dto`: shared JSON DTOs for the universal operator-provider protocol (`HttpInV1`, `HttpOutV1`, `RenderPlanInV1`, `EncodeInV1`, `ProviderPayloadV1`, `SendPayloadInV1`/`SendPayloadResultV1`).
- `components/`: provider WASM components. Includes `secrets-probe`, `slack`, `teams`, `telegram`, `webchat`, `webex`, `whatsapp`, and the provider-core `messaging-provider-dummy`.
- `components/provision`: provisioning apply component used by setup flows to write config/secrets.
- `schemas/`: JSON Schemas for provider configuration (e.g., `schemas/messaging/dummy/public.config.schema.json`).
- `tools/`: build/publishing helpers (e.g., `tools/build_components.sh`).
- To resync pack metadata/schemas and stage fresh artifacts locally, run `./tools/sync_packs.sh` (uses workspace version by default, or `PACK_VERSION` override).
- `packs/`: pack sources (bundled providers and fixtures such as `messaging-dummy`).
- Each pack ships a setup spec at `packs/<pack>/assets/setup.yaml` for CLI-driven setup questions.
- Setup flows (`setup_default`/`setup_custom`) start with `*_emit_questions` and expect both `answers` (object) and `answers_json` (JSON string) in the flow input; pass `dry_run` as `false` for real apply or `true` for a no-op preview.
- `.github/workflows/`: CI pipelines (build/test + component artifacts).

## Generated flows (authoritative)
`packs/*/flows/*.ygtc` are generated artifacts. Do not edit them by hand.

To change flows:
1) Update component manifests/schemas under `components/` or provider specs under `specs/providers/`.
2) Regenerate flows: `./ci/gen_flows.sh`.

CI will reject any direct edits to `packs/*/flows/*.ygtc`.

## Building locally
- Ensure Rust is installed with the `wasm32-wasip2` target and `cargo-component`:
  - `rustup target add wasm32-wasip2`
  - `cargo install cargo-component --locked`
- Run the full check/build pipeline: `./ci/local_check.sh` (fmt, tests, and component builds).
- Component artifacts are copied to `target/components/*.wasm`; the build script uses `cargo component build` by default.
- Build the provider WASMs that power the tester harness and other helpers: `scripts/build-provider-wasms.sh` (outputs `<component>.wasm` files under `dist/wasms`).

## CardKit CLI
- `cargo run -p greentic-messaging-cardkit -- render --provider slack --fixture crates/messaging-cardkit/tests/fixtures/cards/basic.json`
- `cargo binstall greentic-messaging-cardkit --no-confirm --locked` to install the CLI globally (runs the same rendering/demo server as `messaging-cardkit-bin`).

## Questions component
- Build the component: `./tools/build_components.sh` (produces `target/components/questions.wasm`).
- Publish to GHCR: `VERSION=<tag> ./tools/publish_questions_oci.sh`.
- Render/collect answers via CLI:
  - `cargo run -p questions-cli -- --spec /path/to/questions.json`

## Publishing (OCI)
- Tag releases (`v*`) trigger the publish workflow, which builds components and pushes them to GHCR under `ghcr.io/<owner>/greentic-messaging-providers/<component>:<tag>`.
- The publish job also writes `components.lock.json` containing the image references and digests and uploads it as a workflow artifact.
- For manual publishing, ensure `OCI_REGISTRY`, `OCI_NAMESPACE`, and `VERSION` are set, then run `./tools/publish_oci.sh` after building.

## Conformance checks
- Workspace tests include a provider conformance test ensuring each component:
  - exposes the expected exports (including `init-runtime-config`) in its WIT world,
  - has a `component.manifest.json` with `secret_requirements`,
  - does not reference environment variables (secrets come from `greentic:secrets-store`).
  - declares `config_schema.provider_runtime_config` (schema v1, JSON) for host injection as `provider_runtime_config.json`.

## Packs (.gtpack) publishing
- Pack sources live under `packs/` (individual provider packs such as `messaging-telegram`, `messaging-slack`, etc.). Packs are built with `packc` from the `greentic-pack` toolchain via `tools/publish_packs_oci.sh`, which emits the current greentic-pack manifest schema (including `meta.messaging.adapters`) into the `.gtpack`.
- Publishing script defaults: `OCI_REGISTRY=ghcr.io`, `OCI_ORG=${GITHUB_REPOSITORY_OWNER}`, `OCI_REPO=greentic-packs`, `PACK_VERSION` from the tag (or override), `PACKS_DIR=packs`, `OUT_DIR=dist/packs`; media type `application/vnd.greentic.gtpack.v1+zip` is used for pushes.
- Release tags (`v*`) run `.github/workflows/publish_packs.yml` to push `.gtpack` artifacts to `ghcr.io/<org>/greentic-packs/<pack>:<version>` (no `latest` tag by default). `PACK_VERSION` is the tag without the leading `v`.
- `DRY_RUN=1 tools/publish_packs_oci.sh` builds packs and writes `packs.lock.json` with digests set to `DRY_RUN` without pushing; the build workflow runs this check on every branch/PR.
- `packs.lock.json` records registry/org/repo, pack file paths, refs, and digests so downstream tools can pin exact OCI blobs.
- `tools/generate_pack_metadata.py` aggregates `secret_requirements` from each referenced component into `pack.manifest.json` before the pack is zipped, so `.gtpack` metadata contains everything `greentic-secrets` needs.
- Manual pack builds must pass the generated secrets file: `packc build --in . --gtpack-out build/<pack>.gtpack --secrets-req .secret_requirements.json` (regenerate with `python3 tools/generate_pack_metadata.py --pack-dir packs/<pack> --components-dir components --secrets-out packs/<pack>/.secret_requirements.json`).
- Pull example: `oras pull ghcr.io/<org>/greentic-packs/messaging-telegram:1.2.3` (use the digest from `packs.lock.json` for pinning in consumers such as `greentic-messaging` or `greentic-distributor-client`).
- Pack builds require `packc >= 0.4.28`; set `PACKC_BUILD_FLAGS="--offline"` if you need an offline build.

## Secrets workflow
- Runtime secrets are resolved only through the `greentic:secrets-store@1.0.0` host bindings; provider code never reads environment variables or filesystem trees.
- Each provider’s `component.manifest.json` declares structured `secret_requirements`, and pack builds merge them into `pack.manifest.json` inside the resulting `.gtpack`.
- Initialize secrets for a built pack with `greentic-secrets init --pack dist/packs/messaging-telegram.gtpack`, then supply values via your preferred `greentic-secrets` set/apply workflow (e.g., `greentic-secrets set TELEGRAM_BOT_TOKEN=...`).
- Pack metadata contains only key names/scopes/descriptions; no secret values are ever baked into `.gtpack` artifacts or logs.

## Slack component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(channel, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(channel, text) -> string`

Secrets (from `greentic:secrets-store@1.0.0`):
- `SLACK_BOT_TOKEN` (required)
- `SLACK_SIGNING_SECRET` (optional; used for webhook signature verification)

## Teams component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(destination_json, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(destination_json, text) -> string`

Secrets (from `greentic:secrets-store@1.0.0`):
- `MS_GRAPH_TENANT_ID`
- `MS_GRAPH_CLIENT_ID`
- `MS_GRAPH_CLIENT_SECRET` (used to mint bearer tokens for Graph API calls)

## Telegram component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(chat_id, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(chat_id, text) -> string`

Secrets (from `greentic:secrets-store@1.0.0`):
- `TELEGRAM_BOT_TOKEN`

## WebChat component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(session_id, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(session_id, text) -> string`

Secrets:
- None required by default; optional `WEBCHAT_BEARER_TOKEN` is used if provisioned.

## Webex component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(room_id, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(room_id, text) -> string`

Secrets (from `greentic:secrets-store@1.0.0`):
- `WEBEX_BOT_TOKEN`

## WhatsApp component
Exports:
- `init-runtime-config(config_json) -> result<_, provider-error>`
- `send_message(destination_json, text) -> result<string, provider-error>`
- `handle_webhook(headers_json, body_json) -> result<string, provider-error>`
- `refresh() -> result<string, provider-error>` (no-op)
- `format_message(destination_json, text) -> string`

Secrets (from `greentic:secrets-store@1.0.0`):
- `WHATSAPP_TOKEN`
- `WHATSAPP_PHONE_NUMBER_ID`
- `WHATSAPP_VERIFY_TOKEN` (webhook validation)

## Dummy provider-core component
Exports (world `greentic:provider-schema-core/schema-core@1.0.0`):
- `describe() -> bytes` (JSON `ProviderManifest` with `provider_type` = `messaging.dummy`)
- `validate-config(config_json: bytes) -> bytes` (accepts any JSON, returns `{ok:true}` + echo)
- `healthcheck() -> bytes` (returns `{status:"ok"}`)
- `invoke(op, input_json) -> bytes`:
  - `send`/`reply` return deterministic payload with `message_id` derived from the input hash, `provider_message_id = "dummy:<hash>"`, and `status = "sent"` (or `replied`).

Pack fixture:
- `packs/messaging-dummy`: provider-core pack with inline `greentic.provider-extension.v1` extension, config schema at `schemas/messaging/dummy/public.config.schema.json`, and the built `messaging-provider-dummy.wasm` artifact.
