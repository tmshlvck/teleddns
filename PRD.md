# TeleDDNS Go Client — Design & Roadmap

## Overview

**teleddns-go** is a Linux dynamic DNS client that watches the kernel's
`rtnetlink` socket for changes to network interfaces and addresses, selects the
most appropriate public address per host, and reports it to a
[teleddns-server](https://github.com/tmshlvck/teleddns-server) over HTTP. It
runs as a long-running systemd service or as a one-shot CLI invocation.

It is a Go re-implementation of the original Rust client. Since v0.3.0 it is the
canonical implementation in this repository (the Rust source is preserved on the
`rust` branch and the `v0.2.0` tag). It is a **drop-in replacement**: same binary
name (`teleddns`), same `/etc/teleddns/teleddns.yaml`, same systemd unit.

- Go module path: `github.com/tmshlvck/teleddns-go`; package/binary: `teleddns`.

Goals:

- Functional parity with the Rust client on the netlink + DDNS side.
- More explicit handling of rtnetlink events than the Rust client (which
  collapses everything except `NewAddress` into a generic "re-read all state").
- Idiomatic Go layout (multiple small, testable packages) instead of one file.
- Same on-disk configuration format so existing deployments switch binaries
  without re-configuring.

Non-goals: re-implementing the Rust internal structure; non-Linux platforms
(rtnetlink is Linux-specific); the server side.

## Architecture & package layout

```
cmd/teleddns/main.go     CLI, signal handling, wiring
internal/config/         teleddns.yaml loader, struct definitions
internal/applog/         slog setup, verbosity mapping
internal/watch/          rtnetlink observer: startup dump + event stream
                         (events.go types, decode.go, watch.go)
internal/observe/        pretty-print events to the log
internal/filter/         interface pattern matching
internal/metric/         compute_v4_metric / compute_v6_metric
internal/state/          (addr, prefix_len) map; SelectBest
internal/ddns/           HTTP GET, URL sanitization
internal/hooks/          nftables sets + shell hooks
internal/daemon/         rate-limited update worker
```

Netlink: [`github.com/vishvananda/netlink`](https://github.com/vishvananda/netlink)
exposes typed `LinkSubscribe` / `AddrSubscribe` for multicast notifications and
`LinkList` / `AddrList` for the startup dump, keeping the client code small and
pure-Go (no cgo) for the rtnetlink paths used here. The internal package is
named `internal/watch`, not `internal/netlink`, to avoid colliding with the
imported `netlink` package.

## Reference behavior (from the Rust client)

Key parameters and constants the Go port matches:

- Subscribes to multicast groups `RTNLGRP_LINK`, `RTNLGRP_IPV4_IFADDR`,
  `RTNLGRP_IPV6_IFADDR`.
- `MIN_UPDATE_INTERVAL_S = 30` seconds — minimum interval between DDNS updates
  (rate-limit / dampening window).
- On startup, dumps all links + addresses and emits an initial update so the
  first DDNS push happens promptly (~30s in).
- Per-address metric (`compute_v4_metric` / `compute_v6_metric`), higher wins:
  loopback/link-local/documentation/multicast/unspecified → 0; deprecated v6
  → 1; global → 128; EUI-64 v6 → 129 (so EUI-64 wins the tie). Interface bonus
  `en*` → +16, `wl*` → +8. Flag bonus `IFA_F_PERMANENT` → +1.
- Interface filter list: literal name accept, `-name` reject, `*` wildcard;
  matched left-to-right.
- ULA (`fc00::/7`) and RFC1918 excluded unless `report_ipv6_ula` /
  `report_ipv4_private` is set.
- DDNS protocol: HTTP GET to `ddns_url` with query params `myip=<addr>` and
  `hostname=<fqdn>`; credentials embedded in the URL; the password is sanitized
  to `<PASSWORD>` in logs.
- Hooks fire after every observed state change (not only on selected-address
  change): `nft_sets_outfile` writes nftables sets for all subnets on matched
  interfaces; `shell` runs an arbitrary command.
- Shutdown: the Go port handles SIGHUP (as Rust did) plus SIGTERM/SIGINT.

## Configuration

The YAML format is read unchanged from the Rust client:

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

CLI flags match the Rust binary: `-c/--config <FILE>` (default
`/etc/teleddns/teleddns.yaml`), `-o/--oneshot`, `-v/--verbose`, `-q/--quiet`,
`-V/--version`, `-h/--help`.

## Resolved decisions

- **EUI-64 vs privacy preference** — match Rust: EUI-64 base 129 > other global
  128, so EUI-64 wins the tie.
- **`IFA_F_DEPRECATED`** — match Rust: deprecated v6 gets metric 1, never beaten
  unless nothing else exists.
- **Netlink library** — `github.com/vishvananda/netlink` (typed, pure-Go for our
  paths).
- **State directory** — `/var/lib/teleddns/`; `state.json` holds the M3 persisted
  push state. Packaging creates it `0750` and removes it on purge.
- **Drop-in replacement** — install as `/usr/bin/teleddns`, reuse the same unit,
  config path, and man page. The Go package upgrades the Rust package of the
  same name in place; no `Conflicts:`/rename needed.

## Milestones

### M1 — Netlink observer ✅ DONE

An rtnetlink observer that dumps the current links and addresses on startup and
logs every subsequent change. Decodes and distinguishes all four `RTM_*` kinds
(NEWLINK/DELLINK/NEWADDR/DELADDR) plus operstate transitions, with a structured
key=value log line per event. `--oneshot` dumps state once and exits; SIGINT/
SIGTERM/SIGHUP shut down gracefully. Interface filtering, metric selection, DDNS
and hooks were out of scope here. Multicast subscription needs `CAP_NET_ADMIN`
or root; the initial dump runs unprivileged.

### M2 — Filtering, selection, DDNS push ✅ DONE

Turns the observer into a functional DDNS client matching the Rust client's
externally-visible happy-path behavior:

- Interface filtering (exact accept / `-name` reject / `*` wildcard,
  left-to-right).
- `compute_v4/v6_metric` ports with the exact Rust constants and iface/flag
  bonuses; ULA/private gated by config.
- In-memory `(addr, prefix_len) → {iface, subnet, metric, flags}` map, rebuilt
  from a fresh netlink dump on every change (no incremental updates yet).
- `SelectBest`: highest-metric v4 and v6; one DDNS push per family when enabled
  and the selection differs from the last successful push.
- DDNS HTTP client: GET with `myip`/`hostname`; embedded credentials forwarded;
  password sanitized in logs; HTTP 200 = success; failures leave `last_pushed`
  pending to retry; sane timeouts.
- Rate limiting: at most one cycle per 30s, coalescing events in the window.
- Hooks: write nftables `define` blocks for current subnets and run the shell
  hook, on every observed state change.

Intentional deviations from Rust (mostly fixing minor weaknesses):

- `SelectBest` breaks metric ties deterministically (smaller address wins) and
  nft set output is sorted, vs. Rust's nondeterministic map iteration.
- `--oneshot` returns non-zero if a push fails (Rust always exits 0), so it is
  usable from cron.
- Live netlink events log at Debug; Info logs are the state summary, selected
  best per family, and the sanitized push URL + status.
- **Event-level dampening** (pulled forward from M3): a NEWADDR re-announcement
  for an address already tracked with identical flags is logged and ignored
  without rebuilding state (`state.Map.Knows`), mirroring the Rust
  `iface_addrs_map` check and avoiding a full re-dump on every SLAAC lifetime
  refresh. Flag changes (e.g. DEPRECATED set) are still acted on. The worker's
  30s coalescing is a separate, time-based rate limit.

### M3 — Incremental state, dedup, optimization (not started)

**Goal:** reduce churn and resource use. The Rust client (and the Go port so
far) re-dumps every link and address on every relevant message — correct but
wasteful. Maintain state incrementally and suppress no-op work.

- **Incremental state cache:** apply NewLink/DelLink and NewAddr/DelAddr to
  local tables; recompute the derived map only when a relevant change occurred
  (ignore link flag changes that don't alter UP/LOWER_UP/RUNNING). Periodic
  re-sync (default 15min) does a full dump and reconciles, logging
  discrepancies (dropped multicast messages happen under buffer pressure).
- **Update dedup:** hash the sorted derived map and skip the cycle when
  unchanged. Track `last_pushed_v4/v6` and suppress redundant pushes; **persist**
  them to `/var/lib/teleddns/state.json` so a restart loop doesn't hammer the
  server. Add a configurable "force update" / keepalive interval (default 24h).
- **Hook dedup:** hash the nft sets output and only write the file (and run the
  shell hook) when content changed.
- **Netlink robustness:** detect `ENOBUFS` (kernel dropped messages) and
  re-dump immediately; raise `SO_RCVBUF`; handle multipart dump responses.
- **DDNS backoff:** exponential per-family backoff capped ~15min, reset on
  success. (The 30s minimum interval is a rate limit, not a failure backoff.)
- **Observability (optional, behind a flag):** per-RTM-kind event counts, cycle
  timing, last successful push timestamp — via Prometheus `/metrics` on
  localhost or a `SIGUSR1` log dump.
- **Memory hygiene:** the address map must not grow unboundedly under interface
  flap; verify with `pprof` over a long-running test.

Out of scope: IPv6 source-address selection beyond the metric heuristic;
multi-server/multi-hostname; IPv4 NAT detection (STUN/HTTP echo).

**Acceptance:** a 24h soak with no address change produces exactly one push per
family (the keepalive), no nft rewrites, <0.1% CPU; restart within seconds
produces no push (state file consulted) unless older than the keepalive; a
simulated dropped multicast is detected via `ENOBUFS` and recovered within one
cycle; add-then-remove of the same address produces zero pushes once the window
settles.

### M4 — Packaging & CI for the Go build ✅ DONE

Reworked the RPM spec, Debian packaging, and the two GitHub Actions workflows to
build, package, and publish the Go binary as a drop-in replacement, preserving
the existing distro/arch coverage and publish channels.

- **`teleddns.spec`:** build with the Go toolchain (`golang`, `GOTOOLCHAIN=auto`
  so older distro Go fetches the version `go.mod` requires) instead of
  cargo/openssl. Static `CGO_ENABLED=0` build with `-trimpath` and
  `-ldflags "-s -w -X main.version=%{version}"`; **no PIE** (a DDNS client gains
  little from ASLR, and dropping it lets the internal linker build statically
  with no C toolchain). `%global debug_package %{nil}` because Go's compressed
  DWARF trips `find-debuginfo`. Owns `/var/lib/teleddns` (0750).
  `ExclusiveArch: x86_64 aarch64` (COPR builds natively per chroot).
- **`debian/`:** standard debhelper files replacing the old `cargo-deb` flow —
  `control` (Arch `any`, `Depends: ${misc:Depends}` only; the static binary has
  no shared-lib deps), `rules` (static `go build`, `DEB_HOST_ARCH` → `GOARCH`/
  `GOARM`), `changelog`, `install`, `docs`, `manpages`, `dirs`
  (`/var/lib/teleddns`), `source/format` (`3.0 (native)`). `rules` overrides
  `dh_dwz`/`dh_strip`/`dh_makeshlibs`/`dh_shlibdeps` to no-ops since the binary
  is stripped at link time and fully static — this also avoids needing the
  target's binutils, so cross builds need no C toolchain. `postrm` purges
  `/var/lib/teleddns`; `postinst` sets it `0750`.
- **`release.yml`:** `setup-go` + a `{amd64,arm64,armhf,riscv64}` matrix
  (`fail-fast: false`) building `.deb`s with `dpkg-buildpackage`; the crates.io
  publish job is removed. The GitHub Release (per-arch `teleddns-<arch>`
  binaries) and APT-publish (reprepro → `trixie` + `noble`) jobs are preserved.
- **`copr.yml`:** unchanged submission flow (builds from the spec URL); writes
  the full `~/.config/copr` from the `COPR_TOKEN` secret. Chroots:
  `fedora-{43,44,rawhide}-{x86_64,aarch64}`.
- Both workflows trigger on tag push (`v*`).

Out of scope: new distros/arches; SLSA provenance / extra signing; migrating
the APT host or COPR project.

## References

- Rust implementation: `rust` branch / `v0.2.0` tag in this repository.
- Server side: https://github.com/tmshlvck/teleddns-server
