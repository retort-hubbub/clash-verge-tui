# 0008 — Application-owned management interfaces

**Status:** accepted; strengthened after the v0.8.2 security review.

## Context

Subscriptions are replaced by remote publishers on update. Management addresses,
credentials, CORS and controller web UI therefore cannot be trusted to a base
subscription, an imported profile, or a later enhancement.

The earlier decision allowed base profiles to supply these fields when settings
were absent. That preserved imported endpoints but also let subscriptions expose
unauthenticated listeners or supply a publisher-known token.

## Decision

`Service` always constructs the pipeline with application control-plane settings.
On settings load, absent addresses become `127.0.0.1:9090` and absent secrets are
replaced by 32 bytes of OS randomness, stored as a hexadecimal token. Migration is
serialized on Unix so simultaneous startup cannot persist different tokens.
An explicitly empty secret remains allowed; the TUI, generated-config warnings
and doctor report unauthenticated API access.

After enhancement, the application controller and token replace profile values.
All other `CONTROL_PLANE` fields, including alternate listeners, CORS, web UI
and external DoH, are removed. The same class is checked before startup to
regenerate legacy runtime documents that do not match application settings.
The endpoint fallback never reads management credentials from a subscription.
An already running owned process retains its recorded endpoint until reloaded.

The standalone `Pipeline` library can still protect a caller-supplied base
without application settings; application front ends use the stricter service
pipeline. Controller settings remain editable in the TUI and application YAML.

## Consequences

- Imported controller ports must be set explicitly in application settings.
- A default port conflict is reported; the user can choose another loopback port.
- Subscription UI content and alternate API listeners are not deployed.
- Stored tokens remain plaintext. Private Unix directories, atomic 0600 files
  and the executable's 077 creation mask restrict filesystem access.
- GitHub-independent signatures and secret vault storage are separate concerns.
