# Messaging Webchat Pack

Provider-core WebChat messaging pack (send + ingest).

## Pack ID
- `messaging-webchat`

## Providers
- `messaging.webchat` (capabilities: messaging; ops: send, ingest)

## Components
- `ai.greentic.component-templates`
- `messaging-provider-webchat`
- `templates`

## Secrets
- `jwt_signing_key` – HS256 key used to mint Direct Line JWTs; scope = `{env, tenant}`.

## Flows
- `diagnostics`
- `setup_custom`
- `setup_default`
- `verify_webhooks`

## Setup
Inputs:
- Config required: `public_base_url`
- Config optional: `ingress_path`
- Secrets required: `jwt_signing_key`
- Secrets optional: none

Writes:
- Config keys: public_base_url, ingress_path
- Secrets: none

Webhooks:
- public_base_url (the component registers whatever URL you provide; do not append extra segments)
- Operator must also route `/v3/directline/**` into the provider’s `ingest_http` so the new polling-only Direct Line endpoints run inside the wasm.  Streaming (`/stream`) is not implemented yet, so WebChat clients should disable WebSocket and poll `/activities` instead.

## Direct Line (polling) contract

- `POST /v3/directline/tokens/generate`: mint user tokens (optional `{"user":{"id":"..."}}` payload, rate-limited via secrets store). Requires `env`/`tenant`/`team` query params if the tenant context differs from defaults.
- `POST /v3/directline/conversations`: requires `Authorization: Bearer <user-token>`; returns conversation ID + conv token (no `streamUrl` since streaming is disabled) and initializes a persistent state entry.
- `POST /v3/directline/conversations/{id}/activities`: requires convo token bound to `{id}`; accepts minimal activity JSON, validates attachments (whitelisted MIME types + 512 KiB limit), increments a watermark, and stores the activity.
- `GET /v3/directline/conversations/{id}/activities`: requires convo token; returns `{activities:[...], watermark:"<next>"}` with only entries newer than the requested watermark (or all if no watermark query). Responds `200` with `activities:[]` when there are no new activities and keeps watermark unchanged.
- All endpoints respond with JSON errors (`{"error": "...", "message": "..."}`) and rely on `jwt_signing_key` for verifying/issuing HS256 tokens.

Subscriptions:
- none

OAuth:
- not required
