# Webex Component

Webex provider component supporting egress, ingress, and formatting.

## Component ID
- `webex`

## Provider types
- `messaging.slack.api`
- `messaging.teams.graph`
- `messaging.telegram.bot`
- `messaging.webchat`
- `messaging.webex.bot`
- `messaging.whatsapp.cloud`

## Runtime config
- Injected as `provider_runtime_config.json` (json, schema v1).

## Secrets
- `WEBEX_BOT_TOKEN` (tenant): Bot token used for Webex API calls.
