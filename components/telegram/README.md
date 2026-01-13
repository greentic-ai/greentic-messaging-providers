# Telegram Component

Telegram provider component supporting egress, ingress, and formatting.

## Component ID
- `telegram`

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
- `TELEGRAM_BOT_TOKEN` (tenant): Telegram bot token used for sendMessage requests.
