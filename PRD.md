# TeleDDNS Go Client — Product Requirements Document

## Overview

This is **teleddns-go**, a Linux dynamic DNS client that watches the kernel's
`rtnetlink` socket for changes to network interfaces and addresses, selects the
most appropriate public address per host, and reports it to a
[teleddns-server](https://github.com/tmshlvck/teleddns-server) over HTTP.

It is a Go re-implementation of the original Rust client (project **teleddns**,
which keeps that name). The Rust source lives at `../teleddns/` (single file:
`src/main.rs`, ~813 LOC). The Go port is deployed in the same role: as a
long-running systemd service, or as a one-shot CLI invocation.

- Go module path: `github.com/tmshlvck/teleddns-go`.
- The Rust project remains `teleddns`; the Go project is `teleddns-go`.

Goals for the Go port:

- Functional parity with the Rust client on the netlink + DDNS side.
- More complete and explicit handling of rtnetlink events than the Rust client
  (which collapses everything except `NewAddress` into a generic "re-read all
  state" branch).
- Idiomatic Go layout (multiple packages, testable components) instead of one
  monolithic file.
- Same on-disk configuration format (`teleddns.yaml`) so existing deployments
  can switch binaries without re-configuring.

Non-goals:

- Re-implementing the Rust client's exact internal structure.
- Supporting non-Linux platforms (rtnetlink is Linux-specific).
- Building the server side. Only the client is in scope.

## Reference behavior (from the Rust client)

Key parameters and constants observed in `../teleddns/src/main.rs`:

- Subscribes to netlink multicast groups: `RTNLGRP_LINK`,
  `RTNLGRP_IPV4_IFADDR`, `RTNLGRP_IPV6_IFADDR`.
- Minimum interval between DDNS updates: `MIN_UPDATE_INTERVAL_S = 30` seconds
  (rate-limit / dampening window).
- On startup, performs a full dump of links + addresses and emits an initial
  "update" event so a first DDNS push happens promptly.
- Per-address metric: computed by `compute_v4_metric` / `compute_v6_metric`.
  Higher metric wins. Loopback/link-local/documentation/multicast/unspecified
  are forced to 0. EUI-64 v6 addresses get a base of `129` vs `128` for other
  global addresses, so with the `> metric` comparison EUI-64 wins the tie.
  **Decision: the Go port matches the Rust behavior exactly** — EUI-64
  preferred, deprecated addresses demoted to metric 1. See "Open questions".
- Interface bonus: `en*` → +16, `wl*` → +8, otherwise 0.
- Flag bonus: `Permanent` → +1.
- Interface filter list: literal name, `*` wildcard, `-name` for negative
  match. Matched left-to-right.
- ULA (`fc00::/7`) and RFC1918 are excluded unless `report_ipv6_ula` /
  `report_ipv4_private` is set.
- DDNS protocol: HTTP GET to `ddns_url` with query params `myip=<addr>` and
  `hostname=<fqdn>`. Credentials are embedded in the URL
  (`https://user:pass@host/...`).
- Logs sanitize the password in the URL before printing.
- Hooks fire after every observed state change (not only on selected-address
  change): `nft_sets_outfile` writes nftables sets for all subnets currently
  on matched interfaces; `shell` runs an arbitrary shell command.
- Graceful shutdown: SIGHUP terminates the daemon (note: HUP, not TERM/INT —
  this is unusual and the Go port should also handle SIGTERM/SIGINT).

## Configuration

The Go client must read the existing YAML format unchanged. Schema:

```yaml
debug: false                      # optional bool; forces debug log level
ddns_url: 'https://user:pass@host/ddns/update'
hostname: 'myhost.ddns.example.com'
enable_ipv6: true
enable_ipv4: false
report_ipv4_private: false        # optional, default false
report_ipv6_ula: false            # optional, default false
interfaces:
  - '*'                           # or 'eth0', '-virbr0', etc.
hooks:                            # optional list
  - nft_sets_outfile: "/etc/nftables.d/00-localnets.rules"
    shell: "nft -f /etc/nftables.conf"
```

CLI flags must match the Rust binary:

- `-c, --config <FILE>` (default `/etc/teleddns/teleddns.yaml`)
- `-o, --oneshot` — perform one update cycle then exit
- `-v, --verbose` / `-q, --quiet` — log level adjustment
- `-V, --version`, `-h, --help`

## Milestone 1 — Netlink observer

**Goal:** stand up a daemon that opens an rtnetlink socket, subscribes to the
link and IPv4/IPv6 address multicast groups, and prints every relevant event
to the console in a human-readable form. No DDNS, no filtering, no hooks.

### Scope

- Project skeleton: `go.mod`, `cmd/teleddns/main.go`, `internal/netlink/`,
  `internal/config/`, `internal/log/` (thin slog wrapper or just stdlib).
- Dependency choice for netlink: **`github.com/vishvananda/netlink`** (decided).
  It exposes `LinkSubscribe` / `AddrSubscribe` for rtnetlink multicast
  notifications and `LinkList` / `AddrList` for the initial dump as typed Go
  values, which keeps the client code small. Documented in the README.
- Config loader that parses `teleddns.yaml` into a Go struct. The full schema
  is loaded even though only `debug` and `interfaces` are consulted in M1.
- Initial state dump: enumerate all links and addresses on startup and log
  them. This mirrors the Rust client's "trigger the first update" behavior.
- Subscribe to `RTNLGRP_LINK`, `RTNLGRP_IPV4_IFADDR`, `RTNLGRP_IPV6_IFADDR`.
  Note: the rust client comments out `RTNLGRP_IPV4_NETCONF` and
  `RTNLGRP_IPV6_NETCONF`. Go port should leave them off by default but make
  it easy to toggle on for debugging.
- Decode and log **every** event type relevant to address tracking:
  - `RTM_NEWLINK` — new interface, or link attribute change (operstate
    transitions, flag changes, MTU, master, etc.).
  - `RTM_DELLINK` — interface removed.
  - `RTM_NEWADDR` — address added, or address attributes/flags changed
    (e.g. `IFA_F_TENTATIVE` clearing after DAD, `IFA_F_DEPRECATED`).
  - `RTM_DELADDR` — address removed.
  - `RTM_GETLINK` / `RTM_GETADDR` responses from the initial dump.
- Each log line should include: event kind, ifindex, ifname (when known),
  link flags (UP/LOWER_UP/RUNNING), address + prefix length, address scope,
  address flags, address family. Format: structured log (key=value) plus a
  one-line human summary.
- The Rust client treats anything other than `NewAddress` as "something
  changed, re-read everything". The Go client should at minimum **distinguish
  and log** all four `RTM_*` kinds plus operstate transitions explicitly so a
  user inspecting `journalctl -u teleddns` can see what actually happened.
- Signal handling: SIGINT, SIGTERM, SIGHUP all initiate graceful shutdown.
  Close the netlink socket, drain any in-flight goroutines, exit 0.
- `--oneshot` in M1: dump current state to stdout and exit. Don't subscribe.

### Out of scope for M1

- Interface name filtering (parsed but not applied).
- Metric computation, best-address selection.
- DDNS HTTP client, hooks, nft set writer, rate limiting.

### Acceptance criteria

- `teleddns -c ./teleddns.yaml` running in a terminal prints a sensible
  startup banner, the initial set of links and addresses, and then logs
  every subsequent change as the operator runs commands like:
  - `ip link set dev dummy0 up` / `down`
  - `ip addr add 192.0.2.5/24 dev dummy0` / `ip addr del ...`
  - `ip link add dummy1 type dummy` / `ip link del dummy1`
  - SLAAC arrival on a wlan/eth interface (tentative → permanent transition
    must be visible as a flag change event)
- `teleddns -o` prints current state once and exits 0.
- `kill -HUP`, `kill -TERM`, `kill -INT` all exit cleanly.
- Runs unprivileged for the initial dump but documents that multicast
  subscription requires either `CAP_NET_ADMIN` or root (same as Rust).

### Suggested file layout

```
cmd/teleddns/main.go            CLI, signal handling, wiring
internal/config/config.go       YAML loader, struct definitions
internal/applog/applog.go       slog setup, level mapping
internal/watch/events.go        Event / LinkState / AddrState types
internal/watch/decode.go        netlink values → typed state (flags, scope, family)
internal/watch/watch.go         Watcher: initial dump + subscription stream
internal/observe/print.go       Pretty-print events to the log
```

The package directory is `internal/watch`, not `internal/netlink`, to avoid a
name collision with the imported `github.com/vishvananda/netlink` package. The
library hides the raw socket and message decoding, so `socket.go` / `dump.go`
collapse into `watch.go`.

## Milestone 2 — Filtering, selection, DDNS push

**Goal:** turn the observer into a functional DDNS client. Match the Rust
client's externally-visible behavior on the happy path.

### Scope

- Implement interface filtering per `interfaces:` list:
  - Exact match wins immediate accept.
  - `-name` entry wins immediate reject.
  - `*` matches all (unless an earlier `-name` rejected).
  - Matched left-to-right.
- Implement `compute_v4_metric` / `compute_v6_metric` ports including the
  documentation/benchmark/multicast/loopback/link-local/EUI-64 cases. Match
  the Rust constants exactly (0, 1, 128, 129, plus iface/flag bonuses).
- `iface_bonus`: `en*` → 16, `wl*` → 8, else 0.
- `flag_bonus`: `IFA_F_PERMANENT` → +1.
- ULA / private inclusion gated by `report_ipv6_ula` / `report_ipv4_private`.
- Maintain a single in-memory map keyed by `(addr, prefix_len)` with value
  `{iface, subnet_addr, metric, flags}` — same shape as the Rust
  `IfaceIpAddr` → `IfaceIpAddrData` map. Rebuild it from a fresh netlink
  dump on every change in M2 (no incremental updates yet; that's M3).
- `select_best`: highest-metric v4 and v6 across the map. Emit one DDNS
  update per family when `enable_ipv4` / `enable_ipv6` is true and the
  selected best differs from the last successfully pushed value.
- DDNS HTTP client:
  - GET `ddns_url` with added query params `myip=<addr>` and
    `hostname=<fqdn>`.
  - URL parsed with `net/url` so credentials embedded as
    `https://user:pass@host/...` are forwarded as `Authorization: Basic`.
  - Sanitize the password when logging the URL (mirror Rust's `sanitize_url`:
    replace the password span between the first `:` after scheme and the `@`
    with `<PASSWORD>`).
  - Treat HTTP 200 as success. Log status + truncated body on non-200.
  - On network error or non-200, do **not** update `last_pushed`; leave it
    pending so the next state change or rate-limit tick retries.
  - Reasonable client timeouts (e.g. 30s total, 10s dial).
- Rate limiting: at most one update cycle per family per
  `MIN_UPDATE_INTERVAL_S = 30s`. When events arrive inside the window, hold
  them and run the update when the timer expires (the Rust loop uses
  `tokio::time::timeout` to coalesce — Go port can use a single
  `time.Timer` reset, or a small `select` loop).
- Oneshot mode: dump state once, run hooks (see below), push best v4/v6,
  exit. No subscription.
- Hooks (basic): if a hook has `nft_sets_outfile`, write nftables `define`
  blocks for the v4 and v6 subnets currently in the map (dedup by
  `(subnet_addr, prefix_len)`). If a hook has `shell`, run it via
  `/bin/sh -c` after the file write. Hooks fire on every observed state
  change in the map, same as the Rust client.

### Out of scope for M2

- Incremental state updates (still rebuild the whole map per change).
- Persistent dedup of redundant DDNS pushes across restarts.
- Backoff on repeated DDNS failures (beyond the 30s minimum interval).
- TLS pinning, custom CA bundles.

### Acceptance criteria

- Pointed at a real teleddns-server with a working URL + credentials, the
  client successfully publishes the current best IPv6 (and/or IPv4) within
  ~30 seconds of startup and within ~30 seconds of any subsequent address
  change.
- `journalctl -u teleddns` shows: which interfaces matched the filter, the
  metric assigned to each address, which address was selected as best,
  and the sanitized GET URL + response code.
- Adding a second matching interface with a higher-metric address (e.g.
  unplugging Ethernet so wifi becomes best) triggers exactly one new DDNS
  push for the affected family.
- A failing DDNS server (return 500, or refused connection) does not crash
  the daemon and does not advance the "last pushed" state; the next state
  change retries.
- nft sets output file matches the Rust output byte-for-byte on the same
  input (modulo map iteration order — sort entries for stability in the
  Go port; the Rust client doesn't sort, which is a minor weakness worth
  fixing here).

### Suggested additions to the layout

```
internal/filter/filter.go       interface pattern matching
internal/metric/metric.go       compute_v4_metric / compute_v6_metric
internal/state/state.go         IfaceIpAddr map, select_best
internal/ddns/client.go         HTTP GET, URL sanitization
internal/hooks/nft.go           write nftables sets
internal/hooks/shell.go         run shell hook
internal/daemon/loop.go         rate-limited update worker
```

## Milestone 3 — Incremental state, dedup, optimization

**Goal:** reduce churn and resource usage. The Rust client re-dumps every
link and every address on every relevant netlink message, which is
correct-but-wasteful. The Go client should maintain state incrementally and
suppress no-op work.

### Scope

- Incremental state cache:
  - Apply `NewLink` / `DelLink` directly to a local link table.
  - Apply `NewAddr` / `DelAddr` directly to a local address table.
  - Recompute the derived `(addr, prefix_len) → IfaceIpAddrData` map from
    these tables only when a relevant change actually occurred (e.g. ignore
    LinkFlagsChanged that doesn't alter UP/LOWER_UP/RUNNING).
  - Periodic re-sync: every N minutes (configurable, default 15min), do a
    full netlink dump and reconcile against the cached tables. Log any
    discrepancies — these indicate dropped multicast messages (which do
    happen under socket buffer pressure).
- Update dedup:
  - Hash the derived map (sorted by `(addr, prefix_len)`) and skip the
    update cycle entirely when the hash is unchanged.
  - For DDNS pushes, track `last_pushed_v4` / `last_pushed_v6` in memory.
    Suppress a push when the selected address equals the last successfully
    pushed one. The Rust client already does this in memory; M3 should
    additionally **persist** these to a state file (default
    `/var/lib/teleddns/state.json`) so a restart loop doesn't hammer the
    server.
  - Add a "force update" interval (default 24h) so the server learns we're
    still alive even when nothing changed. Keepalive cadence configurable.
- Hook dedup:
  - Compute a hash of the nft sets output and only write the file (and run
    the shell hook) when the content changed. The Rust client rewrites the
    file every cycle.
- Netlink robustness:
  - Detect `ENOBUFS` on the socket (kernel dropped messages) and trigger a
    full re-dump immediately.
  - Increase `SO_RCVBUF` on the netlink socket.
  - Handle netlink message truncation / multipart sequences correctly
    (rtnetlink dump responses are multipart).
- Backoff on DDNS failures:
  - Exponential backoff per family, capped at e.g. 15 min. Reset on any
    successful push.
  - Don't backoff on `MIN_UPDATE_INTERVAL_S` — that's a rate limit, not a
    failure.
- Metrics / observability (optional, behind a flag):
  - Counts of events received per RTM kind.
  - Time spent in each update cycle.
  - Last successful push timestamp.
  - Either Prometheus `/metrics` on a localhost port, or a `SIGUSR1`
    handler that dumps the above to the log.
- Memory hygiene: the address map shouldn't grow unboundedly when
  interfaces flap. Verify with `pprof` over a long-running test.

### Out of scope for M3

- IPv6 source-address selection beyond the metric heuristic
  (the kernel already does the right thing for outbound traffic; we only
  care about which address to *advertise* via DDNS).
- Multi-server / multi-hostname configurations.
- IPv4 NAT detection (querying STUN or an HTTP echo to discover the
  externally-visible v4 when `enable_ipv4: true` is set on a NATed host).

### Acceptance criteria

- During a 24h soak with no actual address change, exactly one DDNS push
  per family occurs (the keepalive), no nft set rewrites occur, and CPU
  use is < 0.1% averaged.
- Killing the daemon and restarting it within seconds does not produce an
  immediate DDNS push (state file consulted, no change → no push) unless
  the persisted state is older than the keepalive interval.
- A simulated dropped multicast (`SO_RCVBUF` deliberately tiny, fire a
  burst of `ip addr` changes) is detected via `ENOBUFS` and the daemon
  recovers via full re-dump within one cycle.
- Adding then immediately removing the same address produces zero DDNS
  pushes once the rate-limit window settles.

## Open questions

1. **EUI-64 vs privacy-extension preference.** *Resolved: match Rust.* The
   Rust client gives EUI-64 addresses a higher base metric (129) than other
   global addresses (128), so EUI-64 wins the tie. The Go port keeps this
   exactly as-is.
2. **`IFA_F_DEPRECATED` handling.** *Resolved: match Rust.* Deprecated v6
   addresses get metric 1 ("use only if nothing else is available"). A
   deprecated address is never promoted to "best" while any non-deprecated
   global address exists, because the latter's metric (≥128) always beats 1.
3. **Netlink library.** *Resolved: `github.com/vishvananda/netlink`.* Chosen
   for its ergonomic typed `LinkSubscribe` / `AddrSubscribe` / `LinkList` /
   `AddrList` API. It is pure Go for the rtnetlink paths used here (no cgo).
4. **State directory path.** Rust client has none. Proposed
   `/var/lib/teleddns/` for the Go port — confirm before M3.
5. **Should the Go binary be drop-in name-compatible with the Rust one?**
   I.e. install as `/usr/bin/teleddns` and use the same systemd unit. If
   yes, packaging needs to handle the transition (Conflicts: in deb/rpm).
   If no, pick a different binary name (`teleddns-go`?). Default: drop-in
   replacement, same binary name.

## References

- Existing Rust implementation: `../teleddns/src/main.rs`
- Sample config: `../teleddns/teleddns.yaml.sample`
- Systemd unit: `../teleddns/teleddns.service`
- Server side: https://github.com/tmshlvck/teleddns-server
