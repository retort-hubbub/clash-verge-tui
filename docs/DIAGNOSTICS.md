# Diagnostics

Every message the validator produces carries a stable code. The codes are the
part to read: the sentence next to one is written for the configuration in
front of you, and the code is what this page explains.

`cvt config validate` prints them; `cvt config generate --json` gives the same
list as data, with `severity`, `code`, `message`, `at` (which node of the
document) and `fix` where a fix is one line.

**Errors** block writing or applying the generated configuration. **Warnings** flag potential issues
without blocking apply. **Notes** provide additional information.

Diagnostic codes are not reused. Removed codes are omitted from this reference.

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
| `W-NO-CONTROLLER` | `external-controller` is not set, so this program cannot manage the core | Set `core.external_controller` in the application settings |
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
systemd-resolved's local stub. Use `127.0.0.1:1053`; with active systemd-resolved,
the application routes system DNS to that listener through its TUN link.
Do not disable the system resolver just to free port 53.
Applications explicitly using a host DNS listener must be pointed at its new
address separately.

A subscription with `dns.listen: :53` remains usable. If it conflicts, the TUI
shows the observed listener addresses, protocols and owner categories: system
DNS service, another proxy core, or an unknown/inaccessible process. Process
name and PID are shown when readable. An active resolved stub on
`127.0.0.53:53` or `127.0.0.54:53` can be identified by its endpoint when process
descriptors are inaccessible; the dialog explicitly marks that classification
as inferred, not confirmed ownership.

The dialog first proposes `127.0.0.1:53` if available, keeping the DNS port while
avoiding the stub's distinct loopback address. Otherwise it proposes a free
local port, starting at 1053. Accepting persists `core.dns_listen` as an
application override; cancelling leaves settings unchanged. The subscription
document and existing services are preserved. Listener availability is checked
again before launch. A local-only listener cannot serve LAN DNS clients through
the original wildcard address. Edit/remove `core.dns_listen` in the settings
file to change/reset the override. CLI conflict errors include the same owner
classification and setting name. A separate listener does not make simultaneous
TUN routing by two proxy cores safe.

TUN is affected by both the subscription and `core.tun_enabled`. `profile`
follows the subscription; `on` overrides a subscription that has no TUN section.
Run only one Mihomo TUN owner at a time, including Clash Verge Rev. Different
proxy/controller ports do not isolate default routes or system DNS. Linux
conflict detection excludes exited/zombie processes and parser/version probes,
and rechecks process birth identity before reporting a conflict.

A rejected TUI profile switch keeps the previous profile selected. Runtime
recovery uses the exact pre-switch document; `config.previous.yaml` is not an
automatic startup fallback. If recovery itself fails, the error states that the
old profile remains selected and the runtime could not be resumed.

`CAP_NET_ADMIN`, `CAP_NET_RAW` and `CAP_NET_BIND_SERVICE` authorize kernel
operations, not systemd-resolved's D-Bus methods. When resolved is active, the
explicit TUN authorization also installs
`/etc/polkit-1/rules.d/49-clash-verge-tui-<uid>-resolver.rules`. It grants that
user only four Link operations on `cvt-mihomo`: set DNS servers, set domains,
set default DNS routing and revert. It does not grant a root shell or authority
over physical interfaces. Administrators can remove this file to revoke the
resolver grant; replacing Mihomo still requires renewing its capabilities.

After the owned core's TUN appears, the application sets the local DNS server,
the `~.` routing domain and `default-route yes`, flushes caches, and reads the
Link settings back. These [Link-level settings](https://github.com/systemd/systemd/blob/main/man/resolvectl.xml)
send ordinary system DNS queries to Mihomo, including queries originating from
the local systemd-resolved stub. Commands have deadlines and use
`--no-ask-password`; missing authorization is an error rather than another
password dialog. A recorded interface index limits cleanup to the same Link.
Stop, TUN disable and failed handoff reset that Link; a vanished/recreated
interface is not adopted for cleanup.

With this integration, `dns.enable` must be true and `dns.listen` must be local.
Missing listeners default to `127.0.0.1:1053`. Upstreams pointing to `system`,
the resolved stub or Mihomo's own listener are rejected to prevent recursive
resolution. Use independent upstream DNS servers. Without active resolved,
the application does not install a resolver policy or change system DNS.

A successful syntax check cannot guarantee DNS answers, remote proxy availability,
or TLS correctness. Startup additionally checks local conflicts and known DNS/TUN
listener failures; apply failures use runtime snapshots when rollback is enabled.

### Managed download failures and recovery

Core installation refuses missing/mismatched GitHub SHA-256 digests, oversized
responses, decompression over 128 MiB, or unsuccessful/timed-out version checks.
The archive limit is 64 MiB; requests time out after 90 seconds and the overall
installation deadline is five minutes. These checks happen before replacement.
The last executable is kept at `<home>/core/mihomo.previous`; stop the managed
core before restoring it, then check network capabilities before restarting.
GitHub release metadata supplies integrity information, not an independent signature.

Mihomo and speedtest-go downloads first try the owned running core's HTTP proxy,
then environment proxies, then direct access. Each route has a 10-second connect
timeout and a 20-second read timeout; all-route failures include each route's error. Explicit proxying avoids host resolution of the GitHub destination,
but follows the core's rules. Direct fallback still uses the system network and
cannot bypass TUN interception.

### Copying from SSH or tmux

Ctrl+Y copies complete open messages/previews or the selected value. In SSH and
headless sessions, copying sends OSC 52 to the terminal. If the terminal does not
allow it, no clipboard update occurs. Configure terminal/tmux clipboard support
or copy with the terminal's own selection mode; the TUI does not enable clipboard
permissions automatically. Desktop clipboard tools are attempted only outside SSH.
