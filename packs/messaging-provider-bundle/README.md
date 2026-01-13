# Messaging Provider Bundle Pack

Bundle of messaging provider components.

## Pack ID
- `messaging-provider-bundle`

## Providers
- `messaging.slack.api` (capabilities: messaging; ops: send, reply)
- `messaging.teams.graph` (capabilities: messaging; ops: send, reply)
- `messaging.telegram.bot` (capabilities: messaging; ops: send, reply)
- `messaging.webchat` (capabilities: messaging; ops: send, reply)
- `messaging.webex.bot` (capabilities: messaging; ops: send, reply)
- `messaging.whatsapp.cloud` (capabilities: messaging; ops: send, reply)

## Components
- `ai.greentic.component-templates`
- `slack`
- `teams`
- `telegram`
- `templates`
- `webchat`
- `webex`
- `whatsapp`

## Secrets
- `SLACK_BOT_TOKEN` (tenant): Slack bot token used for chat.postMessage.
- `SLACK_SIGNING_SECRET` (tenant): Optional signing secret for webhook verification.
- `MS_GRAPH_TENANT_ID` (tenant): Tenant ID for Microsoft Graph access.
- `MS_GRAPH_CLIENT_ID` (tenant): Client ID for Microsoft Graph access.
- `MS_GRAPH_CLIENT_SECRET` (tenant): Client secret used to obtain Graph access tokens.
- `TELEGRAM_BOT_TOKEN` (tenant): Telegram bot token used for sendMessage requests.
- `WEBEX_BOT_TOKEN` (tenant): Bot token used for Webex API calls.
- `WHATSAPP_TOKEN` (tenant): Access token used for WhatsApp Graph API calls.
- `WHATSAPP_PHONE_NUMBER_ID` (tenant): Phone number ID associated with the WhatsApp sender.
- `WHATSAPP_VERIFY_TOKEN` (tenant): Verify token used for webhook validation (if configured).

## Flows
- `diagnostics`
- `main`
- `setup_custom`
- `setup_default`
- `verify_webhooks`
