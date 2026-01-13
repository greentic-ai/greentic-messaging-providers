# Messaging Teams Pack

Provider-core Microsoft Teams messaging pack (Graph send).

## Providers
- `messaging.teams.graph` (capabilities: messaging; ops: send, reply)

## Components
- `ai.greentic.component-templates`
- `messaging-provider-teams`
- `templates`

## Secrets
- `MS_GRAPH_CLIENT_SECRET` (tenant): Client secret used for client_credentials or refresh flows.
- `MS_GRAPH_REFRESH_TOKEN` (tenant): Refresh token used when auth_mode selects refresh_token grant.

## Flows
- `diagnostics`
- `setup_custom`
- `setup_default`
- `verify_webhooks`
