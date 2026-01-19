# Messaging Provider Bundle Pack

## Pack identity
- Path: `packs/messaging-provider-bundle`.
- Pack manifest version: 0.4.15 (`packs/messaging-provider-bundle/pack.manifest.json:3`).
- Dist gtpack manifest version: 0.4.13 (`docs/audit/packs/_evidence/manifests/messaging-provider-bundle.manifest.json:1389`).

## Declared extensions
- Egress (provider extension): `greentic.provider-extension.v1` (`packs/messaging-provider-bundle/pack.manifest.json:161`).
- Setup hints: `messaging.provider_flow_hints` (`packs/messaging-provider-bundle/pack.manifest.json:277`).

## Entry operations per extension
| provider_type | ops | evidence |
| --- | --- | --- |
| messaging.slack.api | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:167`, `packs/messaging-provider-bundle/pack.manifest.json:171` |
| messaging.teams.graph | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:185`, `packs/messaging-provider-bundle/pack.manifest.json:189` |
| messaging.telegram.bot | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:203`, `packs/messaging-provider-bundle/pack.manifest.json:207` |
| messaging.webchat | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:221`, `packs/messaging-provider-bundle/pack.manifest.json:225` |
| messaging.webex.bot | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:239`, `packs/messaging-provider-bundle/pack.manifest.json:243` |
| messaging.whatsapp.cloud | send, reply | `packs/messaging-provider-bundle/pack.manifest.json:257`, `packs/messaging-provider-bundle/pack.manifest.json:261` |

Provider runtime export: `schema-core-api` (`packs/messaging-provider-bundle/pack.manifest.json:179`).

## Inputs/outputs contract
- Provider contract uses `schema-core-api` with JSON byte payloads for `invoke` (`components/messaging-provider-slack/wit/messaging-provider-slack/deps/provider-schema-core/package.wit:6`).

## Config requirements (greentic-config)
- Bundle runtime config injection: `provider_runtime_config` (schema_version 1, injected as `provider_runtime_config.json`) (`packs/messaging-provider-bundle/pack.manifest.json:8`).
| provider | config_schema_ref | evidence |
| --- | --- | --- |
| Slack | assets/schemas/messaging/slack/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:175` |
| Teams | assets/schemas/messaging/teams/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:193` |
| Telegram | assets/schemas/messaging/telegram/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:211` |
| WebChat | assets/schemas/messaging/webchat/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:229` |
| Webex | assets/schemas/messaging/webex/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:247` |
| WhatsApp | assets/schemas/messaging/whatsapp/config.schema.json | `packs/messaging-provider-bundle/pack.manifest.json:265` |

## Secret requirements (greentic-secrets)
- Required secrets: `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, `MS_GRAPH_TENANT_ID`, `MS_GRAPH_CLIENT_ID`, `MS_GRAPH_CLIENT_SECRET`, `TELEGRAM_BOT_TOKEN`, `WEBEX_BOT_TOKEN`, `WHATSAPP_TOKEN`, `WHATSAPP_PHONE_NUMBER_ID`, `WHATSAPP_VERIFY_TOKEN` (`packs/messaging-provider-bundle/pack.manifest.json:71`).

## WASM components (gtpack component sources)
| component | wasm_path | digest |
| --- | --- | --- |
| diagnostics___checks | components/diagnostics___checks.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| main___start | components/main___start.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| requirements___summary | components/requirements___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___apply | components/setup_custom___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___overview | components/setup_custom___overview.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___summary | components/setup_custom___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_custom___validate | components/setup_custom___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___apply | components/setup_default___apply.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___overview | components/setup_default___overview.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___summary | components/setup_default___summary.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| setup_default___validate | components/setup_default___validate.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |
| verify_webhooks___steps | components/verify_webhooks___steps.wasm | sha256:5ec85da53ba2087a2990ffe996ee27702cab8123eed368f842907d112e643d00 |

Component sources reference: `docs/audit/packs/_evidence/manifests/messaging-provider-bundle.manifest.json:624`.

Provider wasm paths (no digest in pack manifest or lock):
- `components/slack.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:667`).
- `components/teams.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:689`).
- `components/telegram.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:711`).
- `components/webchat.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:755`).
- `components/webex.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:777`).
- `components/whatsapp.wasm` (`packs/messaging-provider-bundle/pack.manifest.json:799`).

## PUBLIC_BASE_URL
- Required in config schemas for Slack, Teams, Telegram, WebChat, and WhatsApp.

## Subscriptions lifecycle
- No subscriptions extension declared; only provider ops are listed (`packs/messaging-provider-bundle/pack.manifest.json:161`).

## Offline testability
- None stated in pack README (`packs/messaging-provider-bundle/README.md:1`).

## Status
- PARTIAL: dist manifest version mismatch (0.4.13) vs pack manifest (0.4.15) (`docs/audit/packs/_evidence/manifests/messaging-provider-bundle.manifest.json:1389`, `packs/messaging-provider-bundle/pack.manifest.json:3`).
