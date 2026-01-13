# Whatsapp Component

WhatsApp provider component supporting egress, ingress, and formatting.

## Runtime config
- Injected as `provider_runtime_config.json` (json, schema v1).

## Secrets
- `WHATSAPP_TOKEN` (tenant): Access token used for WhatsApp Graph API calls.
- `WHATSAPP_PHONE_NUMBER_ID` (tenant): Phone number ID associated with the WhatsApp sender.
- `WHATSAPP_VERIFY_TOKEN` (tenant): Verify token used for webhook validation (if configured).
