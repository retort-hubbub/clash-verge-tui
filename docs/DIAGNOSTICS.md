# Diagnostics

Every message the validator produces carries a stable code. The codes are the
part to read: the sentence next to one is written for the configuration in
front of you, and the code is what this page explains.

`cvt config validate` prints them; `cvt config generate --json` gives the same
list as data, with `severity`, `code`, `message`, `at` (which node of the
document) and `fix` where a fix is one line.

**Errors** stop the document from being written or applied. **Warnings** do not
— the core accepts the configuration — but each one is something that is
usually not what the author meant. **Notes** are informational.

A code is never reused for a different meaning, and a code that stops being
produced is removed rather than repurposed — a family check that rejected rules
the core loads went that way. Its name is deliberately not repeated here: this
page is asserted against the codes the validator produces, and a code named in
a sentence is not one a reader can look up.

## Errors

| Code | What it means | What to do |
|---|---|---|
| `E-BAD-FILTER` | A group's `filter` is not a regular expression the core can compile | Fix the pattern, or drop `filter` and name the members explicitly |
| `E-CIDR-NO-PREFIX` | A CIDR-shaped rule has no prefix length. The core refuses all five kinds without one — `IP-CIDR`, `IP-CIDR6`, `SRC-IP-CIDR`, `IP-SUFFIX`, `SRC-IP-SUFFIX` | Write `1.2.3.0/24` rather than `1.2.3.4` |
| `E-CONTROLLER-FORMAT` | `external-controller` is not `host:port` | Fix it, or set `core.external_controller` in the settings |
| `E-DANGLING-GROUP-MEMBER` | A group lists a member that is not a proxy, another group, or a built-in | Add the proxy, or remove the name |
| `E-DANGLING-POLICY` | A rule points at a policy that does not exist | Add the group, or use `DIRECT`/`REJECT` |
| `E-DANGLING-DIALER` | A proxy's `dialer-proxy` names something that is not a proxy or a group. The core refuses it: `` dialer-proxy [x] not found `` | Name a proxy or group that exists, or remove `dialer-proxy` |
| `E-DIALER-CYCLE` | Following `dialer-proxy` from a proxy comes back to a name already in the chain — including a proxy dialling through itself. The core refuses it: `` has circular dialer-proxy dependency `` | Break the chain, or remove `dialer-proxy` from one of them |
| `E-DANGLING-PROVIDER` | A group's `use:` names a provider that is not declared | Declare it under `proxy-providers` |
| `E-DANGLING-RULE-SET` | A `RULE-SET` rule names a rule-provider that is not declared | Declare it under `rule-providers` |
| `E-DNS-NO-NAMESERVER` | `dns.enable` is true but no nameserver is configured | Add `dns.nameserver`, or turn DNS off |
| `E-DUPLICATE-GROUP` | Two groups share a name, so one is unreachable | Rename one |
| `E-DUPLICATE-PROXY` | Two proxies share a name | Rename one |
| `E-EMPTY-NAME` | A proxy or group has an empty name | Name it |
| `E-GROUP-EMPTY` | A group lists no members and uses no provider. The core refuses this: `` `use` or `proxies` missing `` | Give it `proxies:`, or a provider with `use:` |
| `E-GROUP-TYPE` | A group's `type` is not one the core knows | Use `select`, `url-test`, `fallback` or `load-balance` |
| `E-MISSING-SERVER` | A proxy has no `server` | Add it — a proxy without an address cannot connect |
| `E-NO-RULES` | The configuration has no rules at all | Add at least a `MATCH` |
| `E-PORT-CONFLICT` | Two listeners are configured on the same port | Give one of them its own port |
| `E-PORT-RANGE` | A port is outside 1–65535 | Fix it |
| `E-RELAY-CYCLE` | Relay groups form a loop, so traffic would never leave | Break the cycle |
| `E-RULE-MALFORMED` | A line in `rules` is not a rule the core can parse | Write `<TYPE>,<payload>,<policy>`, or `MATCH,<policy>` |

## Warnings

| Code | What it means | What to do |
|---|---|---|
| `W-DOMAIN-WILDCARD` | A domain rule starts with `.`, which matches a subdomain but not the domain itself | Write both, or use `DOMAIN-SUFFIX` |
| `W-DUPLICATE-RULE` | The same rule appears twice | Remove one; the second can never run |
| `W-EMPTY-GROUP` | A testable group has no members it can test | Usually a `filter` that matches nothing |
| `W-FAKEIP-NO-RANGE` | `fake-ip` mode with no `fake-ip-range` | Set it, or accept the core's default |
| `W-FAKEIP-RANGE-IGNORED` | `fake-ip-range` is set but the enhanced mode is not `fake-ip` | Remove it, or switch the mode |
| `W-FAKEIP6-ULA` | `fake-ip-range6` is inside the ULA range, which collides with real addresses | Use a range from `2001:2::/48` |
| `W-MATCH-WITH-PAYLOAD` | A payload-less rule (`MATCH`) carries a field it ignores. The core loads this and discards the field | Remove everything after the policy |
| `W-MISSING-PORT` | A proxy has no `port` | Add it unless the protocol does not need one |
| `W-MIXED-PORT-REDUNDANT` | `mixed-port` is set alongside `port`/`socks-port` | Keep one arrangement |
| `W-NO-CONTROLLER` | `external-controller` is not set, so this program cannot manage the core | Set `core.external_controller` in the settings, or declare it in the base profile |
| `W-NO-TERMINAL-RULE` | No `MATCH` rule: traffic that matches nothing is rejected | Add `MATCH,DIRECT` or `MATCH,<group>` |
| `W-RULE-KIND` | A rule uses a type this build does not know | Usually a newer core; check the spelling |
| `W-TERMINAL-NOT-LAST` | A `MATCH` rule is not the last one, so the rules after it can never run | Move it to the end |
| `W-TUN-AUTOROUTE-NO-DNS` | TUN with `auto-route` but no DNS section, so queries leak | Add a `dns` section |
| `W-TUN-NO-DNS` | TUN is enabled but there is no `dns` section | Add one, or accept the system resolver |
| `W-UNREACHABLE-RULES` | Rules sit after one that always matches | Remove them, or move the terminal rule down |

## Notes

| Code | What it means |
|---|---|
| `I-DNS-DISABLED` | `dns.enable` is false, so the core uses the system resolver |
| `I-FAKEIP6-IMPLICIT` | IPv6 is on with fake-ip but `fake-ip-range6` is unset; the core picks one |
| `I-TUN-NO-STACK` | TUN has no `stack`, so the core uses `gvisor` |
| `I-UNUSED-RULE-SET` | A rule-provider is declared but no rule uses it |

## Why a warning is not an error

The distinction is not severity of consequence, it is *what the core does*.

An error is a document the core will refuse to load, or one that cannot do what
it says. A warning is a document the core loads and runs — and that is the
whole reason it is not an error, because refusing a configuration that works is
the one mistake a validator must not make. Several warnings here are things a
user almost certainly did not intend, and the honest way to say so is to say it
without stopping them.

That rule has been got wrong in both directions. A check that rules after a
terminal `MATCH` were unreachable was an *error* until a real core was asked
and accepted the document, and is a warning now. A check that a CIDR rule's
address family matched its kind's name was an error, and was then removed
entirely once `mihomo -t` and a running core showed it loads
`IP-CIDR,2001:db8::/32,DIRECT` and matches traffic with it. Neither name is
repeated here, for the same reason. Every check in this file that describes
what the core does was settled by asking the core.


## Local DNS and TUN conflicts

If applying a profile reports an occupied listener, choose a free controller or
DNS address. `dns.listen: :53` binds all interfaces and commonly conflicts with
systemd-resolved's local stub. Use `127.0.0.1:1053` when TUN's DNS interception
handles client queries; do not disable the system resolver just to free port 53.
Applications explicitly using a host DNS listener must be pointed at its new
address separately.

TUN is affected by both the subscription and `core.tun_enabled`. `profile`
follows the subscription; `on` overrides a subscription that has no TUN section.
Run only one Mihomo TUN owner at a time, including Clash Verge Rev. Different
proxy/controller ports do not isolate default routes or system DNS.

`CAP_NET_ADMIN`, `CAP_NET_RAW` and `CAP_NET_BIND_SERVICE` authorize kernel
operations, not systemd-resolved's D-Bus methods. The core's `resolvectl` helper
uses `--no-ask-password` to prevent repeated policy-agent dialogs. It may be
refused by the system policy; TUI-created TUN uses explicit UDP/TCP DNS hijacking
and does not require that policy to be changed. This helper only affects the
managed child's PATH, not commands in the user's shell.

A successful syntax check cannot guarantee DNS answers, remote proxy availability,
or TLS correctness. Startup additionally checks local conflicts and known DNS/TUN
listener failures; apply failures use runtime snapshots when rollback is enabled.
A Python `Exception ignored while flushing sys.stdout` needs its complete traceback
(e.g. `BrokenPipeError`) to diagnose; the message alone is not evidence of a DNS fault.
