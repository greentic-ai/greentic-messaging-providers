# Webex webhook component

`webex-webhook` reconciles a Webex `messages.created` subscription so the endpoint you provide in `public_base_url` is registered exactly as-is.

## Inputs
- `public_base_url` (required): the full callback URL that Webex should hit. This component does *not* append or alter the path.
- `secret_token` (optional): when provided, the webhook is configured with that secret so Webex populates `X-Webex-Signature` on every callback.
- `dry_run` (optional): skips real API calls and reports the planned actions.
- `api_base_url` (optional): override for the Webex REST endpoint; defaults to `https://webexapis.com/v1`.
- `env`/`env_id`, `tenant`/`tenant_id`, `team`/`team_id`: used together to derive a deterministic webhook name (`greentic:{env}:{tenant}:{team}:webex`). If any piece is missing, the component falls back to `greentic:webex` and still reconciles the webhook by target URL.

## Outputs
The component returns JSON that includes:
- `provider`: always `webex`.
- `target_url`: the provided `public_base_url`.
- `webhook_name`: the resolved name used for the subscription.
- `actions`: which API calls were executed (`list`, `create`, `update`, `delete`, `noop`, `dry-run`).
- `webhooks`: details about the managed webhook(s) after reconciliation.
- `notes`: reminders that callbacks come with `X-Webex-Signature` and that bots only see rooms they join.

## Behavior
- In live mode this component lists existing webhooks, creates or updates the `messages.created` subscription, and removes extra copies that share the same name.
- In dry-run mode it skips the Webex API and reports what would happen.
- The secret token you pass in must be validated by your ingress handler by re-computing the HMAC for the `X-Webex-Signature` header; there is no built-in validator in this component.
