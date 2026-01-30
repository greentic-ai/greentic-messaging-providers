# Messaging Webex Pack

Provider-core Webex messaging pack (messages API).

## Pack ID
- `messaging-webex`

## Providers
- `messaging.webex.bot` (capabilities: messaging; ops: send, reply)

## Components
- `ai.greentic.component-templates`
- `messaging-provider-webex`
- `templates`

## Secrets
- `WEBEX_BOT_TOKEN` (tenant): Webex bot access token used for Messages API calls.

## Flows
- `diagnostics`
- `setup_custom`
- `setup_default`
- `verify_webhooks`

## Setup
Inputs:
- Config required: public_base_url
- Config optional: default_room_id
- Secrets required: WEBEX_BOT_TOKEN
- Secrets optional: none

Writes:
- Config keys: public_base_url, default_room_id
- Secrets: WEBEX_BOT_TOKEN

Webhooks:
- public_base_url (registered for `messages.created`; the provided secret is mirrored in `X-Webex-Signature` so your ingress should validate it, and Webex only delivers events for rooms where the bot is a member)

Subscriptions:
- none

OAuth:
- not required
