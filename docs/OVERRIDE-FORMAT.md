# Writing an override

A subscription is somebody else's document. This project exists so that the
things you want to change about it live in a *different* document — one you
wrote, one a subscription update will not replace, and one that can be
validated, diffed and undone before it reaches the core.

There are three shapes, and they are all you need:

| Shape | Profile type | For |
|---|---|---|
| **Override** | `override` | changing specific paths, and growing specific lists |
| **Merge** | `merge` | a patch document that is merged into the configuration |
| **Sequence patch** | `rules`, `proxies`, `groups` | adding and removing entries in one of those three lists |

They compose: the chain runs base → merges → overrides → sequence patches, and
a later stage sees the result of the earlier ones.

## Where a document lives

Profiles live under `<home>/profiles/`, one file each, and the index at
`<home>/profiles.yaml` says which is which:

```yaml
current: Rabcdefghij              # the base profile
chain:                            # optional; otherwise derived from options
  - Rmerge00000
  - Mdefault
items:
  - { uid: Rabcdefghij, type: remote, name: My Airport, file: Rabcdefghij.yaml }
  - { uid: Rmerge00000, type: merge,  name: my patch,   file: Rmerge00000.yaml }
  - { uid: Mdefault,    type: override, name: tweaks,   file: Mdefault.yaml }
```

`cvt profiles chain <uid>…` sets that list, and `cvt profiles list` shows what
is in it. Without an explicit chain, the base profile's `option` fields decide
what follows it — which is how an imported `clash-verge-rev` installation keeps
the enhancements a user had bound to each subscription.

## Override

An override names what to change and leaves everything else alone. Every one of
its operations is idempotent — applying the same override twice changes nothing
the second time — with one documented exception, below.

```yaml
remove:
  - "dns.fallback"
  - "proxy-groups[name=广告拦截]"

set:
  mode: global
  "dns.enable": false
  "tun.enable": true

prepend:
  rules:
    - DOMAIN-SUFFIX,intranet.example,DIRECT

append:
  rules:
    - DOMAIN-SUFFIX,corp.example,DIRECT
    - MATCH,PROXY
```

**Paths.** A path is a sequence of steps, and the syntax is small on purpose:

```
dns.enable                     the `enable` key under `dns`
proxies[3].name                the 4th proxy's name
proxies[-1]                    the last proxy
proxy-groups[name=PROXY].url   the url-test URL of the group called PROXY
```

An index must be in range, and a selector must match: a path that cannot
resolve is reported rather than silently creating something. The one place a
path *is* allowed to create structure is an `append` or a `prepend` target,
because growing a list the subscription never had is the point of them.

**`remove` takes every match.** `proxies[name=A]` removes every entry called
`A`, not the first one, so running the override twice means what running it
once meant. Removing something that is not there is not an error.

**`append` keeps `rules` well-formed.** A rule appended *after* a terminal
`MATCH` can never fire, which is the most common way a hand-written rule
silently does nothing. When the target list holds a terminal rule, `append`
inserts immediately **before** it. Appending a rule that is *itself* terminal
takes the existing catch-all's place rather than stacking above it — two
catch-alls is a document the validator warns about, and only the first can ever
run.

Set `append_before_terminal: false` for the literal behaviour.

**The exception to idempotence** is an edit that addresses a list by
*position*, and it has two forms. A removal by position:

```yaml
remove: ["proxies[1]"]        # one-shot
```

The first application removes whatever is at index 1; the second removes
whatever has moved into that slot. And a `set` by position:

```yaml
set:
  "rules[0]": DOMAIN-SUFFIX,mine.example,DIRECT
append:
  rules:
    - DOMAIN-SUFFIX,mine.example,DIRECT
```

Here the append puts the new rule first and the `set` rewrites it — so the
first application sets the rule the append just added, and the second sets
whatever the *next* append moved into slot zero.

Both are supported, because acting on the first element of a list a
subscription controls is a real thing to want, and both are one-shot: the
position is not stable, so the document cannot promise anything about it. Name
the element (`proxies[name=A]`) or remove the value with a sequence patch
instead.

**Two operations cannot contradict each other.** An override that gives a path
a *list* while a `set` needs it to be a *mapping* describes a document that
cannot exist, and is refused when the file is read:

```yaml
set:
  "dns.nameserver.foo": 1     # needs `dns.nameserver` to be a mapping
append:
  dns.nameserver: ["8.8.8.8"] # and this makes it a list
```

Reaching *into* a list is fine and is not a contradiction — that is how an
element of one is addressed:

```yaml
set:
  "dns.nameserver[0]": 9.9.9.9
append:
  dns.nameserver: ["8.8.8.8"]
```

## Merge

A merge document is merged into the configuration key by key. Where a key holds
a list, the list is *replaced* by default; where it holds a mapping, the two
are merged recursively.

```yaml
dns:
  enable: true
  nameserver:
    - 1.1.1.1
    - 8.8.8.8
```

A `null` means "this key should not be there", wherever it appears — as a key's
value, in a subtree that does not exist yet, or as an element of a list:

```yaml
dns:
  fallback: null        # drop it, whatever the subscription put there
```

The array strategies are chosen per key when the merge is written in code; for
a document, the `prepend-*` and `append-*` directives say the same thing in a
form `clash-verge-rev` also understands:

```yaml
prepend-rules:
  - DOMAIN-SUFFIX,intranet.example,DIRECT
append-proxies:
  - { name: "backup", type: socks5, server: 10.0.0.1, port: 1080 }
```

| Directive | Effect |
|---|---|
| `prepend-rules`, `prepend-proxies`, `prepend-proxy-groups` | the patch's entries first, so yours win |
| `append-rules`, `append-proxies`, `append-proxy-groups` | the patch's entries last |

A document may use the directives and the plain form together; they are
rewritten into one patch before the merge runs. An override is usually the
better tool for these, because it can say *where* in the list the entries go
and it will not duplicate an entry that is already there.

## Sequence patch

A sequence patch touches exactly one of `rules`, `proxies` or `proxy-groups`,
and it is the tool for *removing* an entry by value:

```yaml
# profile type: rules
prepend:
  - DOMAIN-SUFFIX,intranet.example,DIRECT
append:
  - DOMAIN-SUFFIX,corp.example,DIRECT
delete:
  - DOMAIN-SUFFIX,ads.example,REJECT
```

Deletion runs first, so an entry may be deleted from the subscription and
re-added by the same patch without ending up twice. An entry named twice in one
patch is added once — a patch is a document somebody wrote, and writing the
same line twice is a slip, not a request for two of them.

For `proxies` and `proxy-groups`, an entry is identified by its `name`; for
`rules`, by the whole line.

## Applying one

```console
$ cvt profiles list                 # the profiles, and which is the base
$ cvt profiles chain                # the patch chain, in the order it runs
$ cvt config diff                   # what this would change, before it changes it
$ cvt config generate --json        # the document's diagnostics plus, per profile,
                                    # a note saying what that stage did
$ cvt config generate --apply       # write it, validate it, hand it to the core
```

When an enhancement does nothing — a merge whose keys are all already what it
asks for, an override already applied — its note says so, which is the
difference between "my override is not working" and "my override has nothing
left to do".

`--apply` refuses to write a document with errors, snapshots the previous one,
and rolls back if the core will not come up with the new one. `cvt config
rollback` restores the last snapshot by hand, and `cvt config snapshots` lists
them.

## A base profile can keep its own DNS

A subscription that ships a `dns` block usually ships one tuned to its own
resolvers. An enhancement written for a *different* subscription quietly
replacing it is how a working configuration starts resolving through somebody
else's server. The base profile can refuse that:

```yaml
# profiles.yaml, on the base entry
option:
  protect_dns: true
```

Its own `dns` section is then restored after every enhancement, and the warning
names it — an override that does not take effect has to say so, or the user
edits it again. Two limits are deliberate: a base with no `dns` of its own
protects nothing (there is nothing to restore, and an enhancement adding one is
doing what was asked), and the switch protects the *whole* section rather than
named keys, because "keep my DNS" is the thing a person means.

## What an override cannot do

The control plane — `external-controller`, its TLS/unix/pipe variants, `secret`
and `external-controller-cors` — is the *application's*, not a profile's. An
enhancement that tries to change or introduce one has it removed, with a
warning naming the key: the endpoint is read out of the generated document, so
a document that rewrites it does not break this program's connection, it
*redirects* it. Set `core.external_controller` and `core.secret` in the
settings, or declare them in the base profile.

There is no JavaScript. A `script` profile is reported as skipped rather than
silently ignored — see `docs/FEATURE-COVERAGE.md` for why, and for what a
script can do that these three shapes cannot.
