# Messaging WebChat Pack

## Pack identity
- Path: `packs/messaging-webchat`.
- Pack manifest version: 0.4.15 (`packs/messaging-webchat/pack.manifest.json:3`).
- Dist gtpack manifest version: 0.4.13 (`docs/audit/packs/_evidence/manifests/messaging-webchat.manifest.json:1153`).

## Declared extensions
- Egress (provider extension): `greentic.provider-extension.v1` (`packs/messaging-webchat/pack.manifest.json:18`).
- Setup hints: `messaging.provider_flow_hints` (`packs/messaging-webchat/pack.manifest.json:44`).

## Entry operations per extension
- Provider ops: `send`, `ingest` (`packs/messaging-webchat/pack.manifest.json:28`).
- Provider runtime export: `schema-core-api` (`packs/messaging-webchat/pack.manifest.json:36`).

## Inputs/outputs contract
- Provider contract uses `schema-core-api` with JSON byte payloads for `invoke` (`components/messaging-provider-webchat/wit/messaging-provider-webchat/deps/provider-schema-core/package.wit:6`).

## Config requirements (greentic-config)
- Required config keys: `mode`, `public_base_url` (`packs/messaging-webchat/schemas/messaging/webchat/config.schema.json:30`).
- Config schema reference: `assets/schemas/messaging/webchat/config.schema.json` (`packs/messaging-webchat/pack.manifest.json:32`).

## Secret requirements (greentic-secrets)
- No declared secrets (`packs/messaging-webchat/pack.manifest.json:57`).

## WASM components (gtpack component sources)
| component | wasm_path | digest |
| --- | --- | --- |
| diagnostics___check_secret | components/diagnostics___check_secret.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___optional_loopback | components/diagnostics___optional_loopback.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___summary | components/diagnostics___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| requirements___summary | components/requirements___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___apply | components/setup_custom___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___collect | components/setup_custom___collect.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___summary | components/setup_custom___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___validate | components/setup_custom___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___apply | components/setup_default___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___collect | components/setup_default___collect.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___summary | components/setup_default___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___validate | components/setup_default___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| verify_webhooks___steps | components/verify_webhooks___steps.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |

Component sources reference: `docs/audit/packs/_evidence/manifests/messaging-webchat.manifest.json:496`.

Provider wasm path (no digest in pack manifest or lock):
- `components/messaging-provider-webchat.wasm` (`packs/messaging-webchat/pack.manifest.json:241`).

## PUBLIC_BASE_URL
- Required config key in `packs/messaging-webchat/schemas/messaging/webchat/config.schema.json:30`.

## Subscriptions lifecycle
- No subscriptions extension declared; only provider ops are listed (`packs/messaging-webchat/pack.manifest.json:18`).

## Offline testability
- None stated in pack README (`packs/messaging-webchat/README.md:1`).

## Status
- PARTIAL: dist manifest version mismatch (0.4.13) vs pack manifest (0.4.15) (`docs/audit/packs/_evidence/manifests/messaging-webchat.manifest.json:1153`, `packs/messaging-webchat/pack.manifest.json:3`).
