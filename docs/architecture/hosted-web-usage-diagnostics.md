# Hosted-web usage diagnostics

The historical `usage` / `malformed_response` failure does not identify a rejected field, and its response body was not retained. The [OpenAI SDK usage schema](https://github.com/openai/openai-python/blob/main/src/openai/types/responses/response_usage.py) requires the same primary counters and detail objects as our decoder; the hosted-web ceilings match Morons' configured provider limits. Neither observation establishes the incident's cause.

Add a closed usage-rejection reason to the existing default-off bounded debug sink at the owned transport failure. Distinguish missing usage, invalid schema, each token ceiling, subset inconsistencies, and total mismatch. Retain canonical `usage` diagnostics and all existing acceptance checks; no raw fields, counts, credentials, body capture, retries, new log destination, or persistence vocabulary changes. Shared Responses validation supplies the reason so diagnostics cannot drift from validation.

Synthetic regressions cover each reason and accepted boundary values. A fresh live probe is separate from replaying the uncertain search and requires owner authorization; this change alone does not establish that live search succeeds.
