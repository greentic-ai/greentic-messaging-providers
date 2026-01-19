# Messaging Teams Pack

## Pack identity
- Path: `packs/messaging-teams`.
- Pack manifest version: 0.4.15 (`packs/messaging-teams/pack.manifest.json:3`).
- Dist gtpack manifest version: 0.4.13 (`docs/audit/packs/_evidence/manifests/messaging-teams.manifest.json:1302`).

## Declared extensions
- Egress (provider extension): `greentic.provider-extension.v1` (`packs/messaging-teams/pack.manifest.json:19`).
- Ingress: `messaging.provider_ingress.v1` (`packs/messaging-teams/pack.manifest.json:74`).
- Subscriptions: `messaging.subscriptions.v1` (`packs/messaging-teams/pack.manifest.json:89`).
- Setup hints: `messaging.provider_flow_hints` (`packs/messaging-teams/pack.manifest.json:62`).
- OAuth: `messaging.oauth.v1` (`packs/messaging-teams/pack.manifest.json:45`).

## Entry operations per extension
- Provider ops: `send`, `reply` (`packs/messaging-teams/pack.manifest.json:29`).
- Provider runtime export: `schema-core-api` (`packs/messaging-teams/pack.manifest.json:37`).
- Ingress export: `handle-webhook` (`packs/messaging-teams/pack.manifest.json:85`).
- Subscriptions export: `sync-subscriptions` (`packs/messaging-teams/pack.manifest.json:94`).
- OAuth secret keys: `MS_GRAPH_CLIENT_SECRET`, `MS_GRAPH_REFRESH_TOKEN` (`packs/messaging-teams/pack.manifest.json:56`).

## Inputs/outputs contract
- Provider contract uses `schema-core-api` with JSON byte payloads for `invoke` (`components/messaging-provider-teams/wit/messaging-provider-teams/deps/provider-schema-core/package.wit:6`).
- Ingress contract uses `handle-webhook` returning `normalized-payload-json` (`components/messaging-ingress-teams/wit/messaging-ingress-teams/deps/provider-common/world.wit:67`).
- Subscriptions contract uses `sync-subscriptions` with JSON payloads (`components/messaging-ingress-teams/wit/messaging-ingress-teams/deps/provider-common/world.wit:81`).

## Config requirements (greentic-config)
- Required config keys: `tenant_id`, `client_id`, `public_base_url` (`packs/messaging-teams/schemas/messaging/teams/config.schema.json:55`).
- Config schema reference: `assets/schemas/messaging/teams/config.schema.json` (`packs/messaging-teams/pack.manifest.json:33`).
- Subscriptions pattern: `sync` (`packs/messaging-teams/pack.manifest.json:94`).

## Secret requirements (greentic-secrets)
- Required secrets: `MS_GRAPH_CLIENT_SECRET`, `MS_GRAPH_REFRESH_TOKEN` (`packs/messaging-teams/pack.manifest.json:104`).

## WASM components (gtpack component sources)
| component | wasm_path | digest |
| --- | --- | --- |
| diagnostics___channel_check | components/diagnostics___channel_check.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___subscription_check | components/diagnostics___subscription_check.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___summary | components/diagnostics___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___token_check | components/diagnostics___token_check.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| requirements___summary | components/requirements___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___apply | components/setup_custom___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___collect | components/setup_custom___collect.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___summary | components/setup_custom___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___validate | components/setup_custom___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___apply | components/setup_default___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___collect | components/setup_default___collect.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___summary | components/setup_default___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___validate | components/setup_default___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| verify_webhooks___verify | components/verify_webhooks___verify.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |

Component sources reference: `docs/audit/packs/_evidence/manifests/messaging-teams.manifest.json:560`.

Provider/ingress wasm paths (no digest in pack manifest or lock):
- `components/messaging-ingress-teams.wasm` (`packs/messaging-teams/pack.manifest.json:321`).
- `components/messaging-provider-teams.wasm` (`packs/messaging-teams/pack.manifest.json:343`).

## PUBLIC_BASE_URL
- Required config key in `packs/messaging-teams/schemas/messaging/teams/config.schema.json:55`.

## Subscriptions lifecycle
- Pattern `sync` with `sync-subscriptions`; expects state `webhook_url` + `desired_subscriptions` and renews before expiry within `renewal_window_hours` (`packs/messaging-teams/pack.manifest.json:94`).

## Offline testability
- None stated in pack README (`packs/messaging-teams/README.md:1`).

## Status
- PARTIAL: dist manifest version mismatch (0.4.13) vs pack manifest (0.4.15) (`docs/audit/packs/_evidence/manifests/messaging-teams.manifest.json:1302`, `packs/messaging-teams/pack.manifest.json:3`).
