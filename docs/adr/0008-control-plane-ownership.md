# 0008 — Control-plane precedence across settings and profiles

**Status:** accepted

## Context

This program reaches the core over its REST API, and the address and secret for
that API live *in the configuration the core is started with* — the same
document the profiles produce. `external-controller`,
`external-controller-tls`, `external-controller-unix`,
`external-controller-pipe`, `external-controller-routing-mark`,
`external-controller-cors`, `external-ui`, `external-ui-url` and `secret`
are ordinary keys in it, indistinguishable from `mixed-port` or `mode`.

That is a problem with two faces.

The first is that the application's own connection is configuration. An
override, a merge document or a sequence patch could change `external-controller`
and this program would follow it, because the endpoint is read out of the
generated document. It would not break — it would *redirect*, silently and by
design, and a bundle imported from somebody else's installation can carry such a
change. A profile that widens `external-controller-cors` to `*` is opening a door
on the user's behalf without asking.

The second is the opposite, and it is why the obvious fix is wrong: the base
profile is usually a subscription, and a subscription document is **replaced
wholesale** on every update. A setting kept there lasts until the next refresh.
`clash-verge-rev` solves this by forcing its own controller address and secret
over every profile after enhancement, from its application configuration.

## Decision

The control plane has exactly two sources, and a profile is one of them only
when it is a base:

1. **`core.external_controller` and `core.secret` in the settings.** They win
   over everything. Nothing a subscription can reach writes them, and they
   survive every update because they are not in a profile at all.
2. **A base profile that declares one.** This is what keeps an imported
   `clash-verge-rev` installation working, where the controller address is part
   of the configuration the user already had.

Anywhere else is not a source. After the whole chain has been applied, a key the
base declared and an enhancement changed is put back, and a key an enhancement
*introduced* — one the base does not declare and the settings do not set — is
removed. Both outcomes produce a warning naming the key and where to put it
instead, because an override that does not take effect has to say so or the user
edits it again.

Both settings are reachable from the interface, not only from YAML: a rule that
says profiles may not set a value is only usable if there is somewhere to set it.

## Consequences

- When `core.external_controller` or `core.secret` is set, a subscription
  update cannot change that value. Without the setting, the base profile is
  the source, so an update can change it; pin it in settings when stability is
  required.
- An imported installation's controller keeps working when it is declared in
  the base profile.
- An override that used to be able to set `external-controller` no longer can.
  It is removed with a warning naming `core.external_controller`, so the
  migration is one line in a settings file rather than a mystery.
- `external-ui` and `external-ui-url` are also protected. They decide what
  code the core serves at `/ui`, on the controller's origin, so an enhancement
  cannot redirect them. A base profile may declare them; these two keys have
  no application-setting override.
- The check runs after enhancement rather than at parse time, because only then
  is the final document known. That is also why it can report the *key* rather
  than a line number.

## Alternatives considered

**Validate and refuse.** The most obvious option: a profile that declares one,
or an enhancement that changes one, is an error. Rejected because a base profile
legitimately declares one — an imported `clash-verge` home does exactly that —
and because refusing the whole apply for a key that can simply be removed turns
a small mistake into a configuration that will not load.

**Remove every control-plane key and take them only from the settings.**
Cleanest to describe. Rejected because it breaks every imported installation on
first run, and because a user with no settings would have no controller at all:
the validator warns "external-controller is not set; clash-verge-tui cannot
manage this core", which is honest but is not what an import should produce.

**Warn but change nothing.** What the first attempt did. Rejected on the
evidence: an enhancement could still introduce a secret or widen CORS, and the
warning said so without preventing it — which is a report, not a rule.
