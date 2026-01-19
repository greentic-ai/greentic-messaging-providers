# Messaging Telegram Pack

## Pack identity
- Path: `packs/messaging-telegram`.
- Pack manifest version: 0.4.15 (`packs/messaging-telegram/pack.manifest.json:3`).
- Dist gtpack manifest version: 0.4.13 (`docs/audit/packs/_evidence/manifests/messaging-telegram.manifest.json:1271`).

## Declared extensions
- Egress (provider extension): `greentic.provider-extension.v1` (`packs/messaging-telegram/pack.manifest.json:19`).
- Ingress: `messaging.provider_ingress.v1` (`packs/messaging-telegram/pack.manifest.json:57`).
- Setup hints: `messaging.provider_flow_hints` (`packs/messaging-telegram/pack.manifest.json:45`).

## Entry operations per extension
- Provider ops: `send`, `reply` (`packs/messaging-telegram/pack.manifest.json:29`).
- Provider runtime export: `schema-core-api` (`packs/messaging-telegram/pack.manifest.json:37`).
- Ingress export: `handle-webhook` (`packs/messaging-telegram/pack.manifest.json:68`).

## Inputs/outputs contract
- Provider contract uses `schema-core-api` with JSON byte payloads for `invoke` (`components/messaging-provider-telegram/wit/messaging-provider-telegram/deps/provider-schema-core/package.wit:6`).
- Ingress contract uses `handle-webhook` returning `normalized-payload-json` (`components/messaging-ingress-telegram/wit/messaging-ingress-telegram/deps/provider-common/world.wit:67`).

## Config requirements (greentic-config)
- Required config keys: `bot_token`, `public_base_url` (`packs/messaging-telegram/schemas/messaging/telegram/config.schema.json:26`).
- Config schema reference: `assets/schemas/messaging/telegram/config.schema.json` (`packs/messaging-telegram/pack.manifest.json:33`).

## Secret requirements (greentic-secrets)
- Required secrets: `TELEGRAM_BOT_TOKEN` (`packs/messaging-telegram/pack.manifest.json:73`).

## WASM components (gtpack component sources)
| component | wasm_path | digest |
| --- | --- | --- |
| diagnostics___optional_send | components/diagnostics___optional_send.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___preflight | components/diagnostics___preflight.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___summary | components/diagnostics___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___verify_token | components/diagnostics___verify_token.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
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

Component sources reference: `docs/audit/packs/_evidence/manifests/messaging-telegram.manifest.json:560`.

Provider/ingress wasm paths (no digest in pack manifest or lock):
- `components/messaging-ingress-telegram.wasm` (`packs/messaging-telegram/pack.manifest.json:285`).
- `components/messaging-provider-telegram.wasm` (`packs/messaging-telegram/pack.manifest.json:307`).

## PUBLIC_BASE_URL
- Required config key in `packs/messaging-telegram/schemas/messaging/telegram/config.schema.json:26`.

## Subscriptions lifecycle
- No subscriptions extension declared; only ingress `handle-webhook` and provider ops are listed (`packs/messaging-telegram/pack.manifest.json:19`).

## Offline testability
- None stated in pack README (`packs/messaging-telegram/README.md:1`).

## Status
- PARTIAL: dist manifest version mismatch (0.4.13) vs pack manifest (0.4.15) (`docs/audit/packs/_evidence/manifests/messaging-telegram.manifest.json:1271`, `packs/messaging-telegram/pack.manifest.json:3`).
