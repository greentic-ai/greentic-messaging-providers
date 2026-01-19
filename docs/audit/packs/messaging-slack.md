# Messaging Slack Pack

## Pack identity
- Path: `packs/messaging-slack`.
- Pack manifest version: 0.4.15 (`packs/messaging-slack/pack.manifest.json:3`).
- Dist gtpack manifest version: 0.4.13 (`docs/audit/packs/_evidence/manifests/messaging-slack.manifest.json:1590`).

## Declared extensions
- Egress (provider extension): `greentic.provider-extension.v1` (`packs/messaging-slack/pack.manifest.json:19`).
- Ingress: `messaging.provider_ingress.v1` (`packs/messaging-slack/pack.manifest.json:77`).
- Setup hints: `messaging.provider_flow_hints` (`packs/messaging-slack/pack.manifest.json:64`).
- OAuth: `messaging.oauth.v1` (`packs/messaging-slack/pack.manifest.json:45`).

## Entry operations per extension
- Provider ops: `send`, `reply` (`packs/messaging-slack/pack.manifest.json:29`).
- Provider runtime export: `schema-core-api` (`packs/messaging-slack/pack.manifest.json:37`).
- Ingress export: `handle-webhook` (`packs/messaging-slack/pack.manifest.json:88`).
- Setup flow hints: `setup_custom`, `setup_default`, plus `diagnostics`, `rotate_credentials`, `verify_webhooks` (`packs/messaging-slack/pack.manifest.json:64`).
- OAuth secret keys: `SLACK_BOT_TOKEN` (`packs/messaging-slack/pack.manifest.json:58`).

## Inputs/outputs contract
- Provider contract uses `schema-core-api` with JSON byte payloads for `invoke` (`components/messaging-provider-slack/wit/messaging-provider-slack/deps/provider-schema-core/package.wit:6`).
- Ingress contract uses `handle-webhook` returning `normalized-payload-json` (`components/messaging-ingress-slack/wit/messaging-ingress-slack/deps/provider-common/world.wit:67`).

## Config requirements (greentic-config)
- Required config keys: `bot_token`, `public_base_url` (`packs/messaging-slack/schemas/messaging/slack/config.schema.json:31`).
- Config schema reference: `assets/schemas/messaging/slack/config.schema.json` (`packs/messaging-slack/pack.manifest.json:33`).

## Secret requirements (greentic-secrets)
- Required secrets: `SLACK_SIGNING_SECRET`, `SLACK_BOT_TOKEN` (`packs/messaging-slack/pack.manifest.json:93`).

## WASM components (gtpack component sources)
| component | wasm_path | digest |
| --- | --- | --- |
| diagnostics___auth_test | components/diagnostics___auth_test.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___optional_send | components/diagnostics___optional_send.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___preflight | components/diagnostics___preflight.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| diagnostics___summary | components/diagnostics___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| requirements___summary | components/requirements___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| rotate_credentials___apply | components/rotate_credentials___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| rotate_credentials___collect | components/rotate_credentials___collect.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| rotate_credentials___summary | components/rotate_credentials___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| rotate_credentials___validate | components/rotate_credentials___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___apply | components/setup_custom___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___collect_inputs | components/setup_custom___collect_inputs.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___summary | components/setup_custom___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___validate | components/setup_custom___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___apply_config | components/setup_default___apply_config.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___collect_inputs | components/setup_default___collect_inputs.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___summary | components/setup_default___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___validate_token | components/setup_default___validate_token.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| verify_webhooks___describe | components/verify_webhooks___describe.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |

Component sources reference: `docs/audit/packs/_evidence/manifests/messaging-slack.manifest.json:688`.

Provider/ingress wasm paths (no digest in pack manifest or lock):
- `components/messaging-ingress-slack.wasm` (`packs/messaging-slack/pack.manifest.json:320`).
- `components/messaging-provider-slack.wasm` (`packs/messaging-slack/pack.manifest.json:342`).

## PUBLIC_BASE_URL
- Required config key in `packs/messaging-slack/schemas/messaging/slack/config.schema.json:31`.

## Subscriptions lifecycle
- No subscriptions extension declared; only ingress `handle-webhook` and provider ops are listed (`packs/messaging-slack/pack.manifest.json:19`).

## Offline testability
- None stated in pack README (`packs/messaging-slack/README.md:1`).

## Status
- PARTIAL: dist manifest version mismatch (0.4.13) vs pack manifest (0.4.15) (`docs/audit/packs/_evidence/manifests/messaging-slack.manifest.json:1590`, `packs/messaging-slack/pack.manifest.json:3`).
