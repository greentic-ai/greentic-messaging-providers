# Messaging Slack Pack

Provider-core Slack messaging pack (chat.postMessage).

## Pack ID
- `messaging-slack`

## Providers
- `messaging.slack.api` (capabilities: messaging; ops: send, reply)

## Components
- `ai.greentic.component-templates`
- `messaging-provider-slack`
- `templates`

## Secrets
- `SLACK_BOT_TOKEN` (tenant): Slack bot token used for chat.postMessage calls.
- `SLACK_SIGNING_SECRET` (tenant): Slack signing secret (optional for future webhook validation).

## Flows
- `diagnostics`
- `rotate_credentials`
- `setup_custom`
- `setup_default`
- `verify_webhooks`
