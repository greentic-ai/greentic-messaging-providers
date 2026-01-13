# Slack Component

Slack provider component supporting egress, ingress, and formatting.

## Component ID
- `slack`

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
- `SLACK_BOT_TOKEN` (tenant): Slack bot token used for chat.postMessage.
- `SLACK_SIGNING_SECRET` (tenant): Optional signing secret for webhook verification.
