# TeleDDNS — Rework Plan (target release: v0.4.0)

This document is the working plan for the next round of changes. It is derived
from an analysis of the current `src/main.rs` (single-file, ~810 lines), the
packaging/CI, and the [teleddns-server](../teleddns-server/README.md) DDNS
surface. Items are grouped by the eight topics raised, with a concrete design and
a task checklist for each. The end goal is a clean, documented **v0.4.0** with a
new (backward-convertible) config format, new deb/rpm packages, and no golang
history in the changelogs.

## Current-state summary (baseline)

- One `update_worker` loop with a 30 s dampening constant (`MIN_UPDATE_INTERVAL_S`).
  No exponential backoff, no periodic resync, no graceful SIGTERM handling.
- DDNS push: builds a URL per address, sends **v6 in `myip`** (wrong param),
  treats **any HTTP 200 as success** and ignores the dyndns2 body keyword, creates
  a fresh `reqwest::get` per call (no shared client, **no request timeout**).
- Interface filter: `interfaces: [String]` with `*`, exact-name and `-name`
  negation, first match wins. No regex.
- Priority: `compute_v4_metric` / `compute_v6_metric` return `u8`, plus a
  hardcoded `iface_bonus` (en=16, wl=8) and `flag_bonus` (permanent +1). Does not
  detect temporary/privacy addresses or CGNAT (100.64/10).
- Hooks run `sh -c <cmd>` **as root** (service has no `User=`); `nft_sets_outfile`
  writes an arbitrary path as root.
- Cross-compilation via `cargo-zigbuild` for 4 gnu targets (dynamic glibc, not static).
- Version drift: `Cargo.toml`/`teleddns.spec`/man page say `0.2.0`, but git tags
  reach `v0.3.1`.

---

## 1. dyndns2 pseudo-standard compliance

**Goal:** be a correct generic dyndns2 client (works against DuckDNS, No-IP,
dyn.com, ddclient-style servers) while remaining the first-class client for
teleddns-server.

### Findings / gaps

1. **IPv6 goes in the wrong parameter.** `send_ddns_update` always uses `myip`,
   even for IPv6. The standard (and teleddns-server) expects **`myip` = IPv4**,
   **`myipv6` = IPv6**. Generic services silently ignore or reject an IPv6 in `myip`.
2. **Response body is ignored.** Only the HTTP status is inspected; a plain
   dyndns2 server returns **HTTP 200 with a plaintext keyword body** for both
   success and failure (`good`, `nochg`, `nohost`, `badauth`, `abuse`, `911`, …).
   The client currently counts `nohost`/`badauth`/`abuse` as success and would
   also hammer a server returning `abuse`.
3. **No explicit `User-Agent`.** dyn.com and clones reject a missing/blocklisted
   UA with `badagent`. reqwest sends its own UA today; we should send a stable
   `teleddns/<version>` UA.
4. **Credentials only via URL userinfo.** Works (reqwest converts to Basic auth),
   but there is no way to use a **Bearer token** (teleddns-server supports it) and
   no way to keep the secret out of the URL string.
5. **Two requests always.** v4 and v6 are pushed as separate GETs. The standard
   allows one request carrying both (`myip` + `myipv6`); teleddns-server accepts
   both params in one call.

### teleddns-server ∩ dyndns2 (the compatible subset to target)

- Paths: `/nic/update`, `/ddns/update`, `/update` (server accepts all three; the
  client just uses whatever `ddns_url` points at).
- Params: `hostname`, `myip` (v4), `myipv6` (v6).
- Auth: HTTP Basic **or** `Authorization: Bearer <key>`.
- Response vocabulary: `good` / `nochg` / `nohost` / `!yours` / `notfqdn` /
  `badauth` / `abuse` (server) plus generic `badagent` / `911` / `dnserr` /
  `!donator` (other servers). HTTP status is authoritative for teleddns-server,
  but generic servers only encode status in the body — so **parse both**.

### Design

> **Confirmed against teleddns-server `src/ddns.rs`:** `myipv6` is fully supported
> (validated as IPv6, else `notfqdn`); `myip` is family-detected and accepts either
> v4 or v6 — which is why today's "v6-in-`myip`" bug never surfaced against
> teleddns-server. Both params in **one request** are supported (the server collects
> all `(family, addr)` pairs and updates A + AAAA in one call). Decision: **switch v6
> to `myipv6`** and combine into a single GET.

- Send **v4 in `myip`, v6 in `myipv6`**. Combine both into a single GET when both
  changed (falls back to a single param when only one is eligible).
- Add a shared `reqwest::Client` (built once) with a **request timeout**
  (`http_timeout`, default 30 s) and a **`User-Agent: teleddns/<version>`** via
  `env!("CARGO_PKG_VERSION")` (non-generic UA satisfies `badagent`-enforcing
  servers; the server logs it). Optional polish: append `(+<repo-url>)` contact.
- Parse the response into a `DdnsOutcome` from the **first body token** (case-
  insensitive), then fall back to HTTP status:
  - **Success (no retry, adopt as current):** `good`, `nochg`, and any 2xx with `OK`.
  - **Permanent failure (no retry, log ERROR, do not adopt):** `nohost`,
    `notfqdn`, `!yours`, `badauth`, `badagent`, `!donator`, `abuse`, HTTP 401/403/404.
  - **Transient failure (retry with backoff — see §2):** `911`, `dnserr`, any 5xx,
    connection/timeout errors.
- Keep `sanitize_url` for logging. (Explicit auth config — Basic
  `username`/`password` and Bearer `token` — moved to **§9**.)
- **Optional/stretch:** `protocol: dyndns2 | duckdns` selector to support the
  DuckDNS param shape (`domains`, `token`, `ip`, `ipv6`, `OK`/`KO` responses).

### Tasks

- [x] Introduce `DdnsOutcome { Good(addrs), NoChg(addrs), Permanent, Transient, RateLimited }`.
- [x] Fix `send_ddns_update` to emit correct `myip` (v4) / `myipv6` (v6); return `DdnsOutcome`.
- [x] Build a shared `reqwest::Client` with timeout + `User-Agent: teleddns/<version>`.
- [x] Body-keyword parser + classification table; unit tests for each keyword.
- [x] Document the protocol + mapping in `DYNDNS2.md`.
- [x] **Second pass vs. server DYNDNS2.md (0.4.0):** first-*line* parsing (the body is
      one line per hostname, not per family, since server 0.4.0), `abuse` reclassified
      as rate-limited/retryable, `numhost`/`!active`/`ko` added, 429 → rate-limited in
      the status fallback, `good`/`nochg` address list parsed and cross-checked against
      what we sent, permanent failures no longer retried, `hostname` validated at
      startup (single FQDN, §4 syntax), `--oneshot` exits non-zero on failure.
- [x] **Decision: keep one request per family** (do *not* combine v4+v6 into one GET).
      Server §9 merges a dual-family request into a single line whose keyword is the
      *most severe* of the two, which we cannot attribute back to a family; §10 counts
      the rate limit *per family*, so two requests cost the same quota as one combined
      one. Per-family requests are strictly more informative at equal cost.
- [ ] Explicit `username`/`password` (Basic) config — see §9 for Bearer.
- [ ] (Stretch) `protocol` selector for DuckDNS shape.

> **Done (v0.4.0 WIP):** `myipv6`, User-Agent, one request per family, response
> keyword + address parsing, retry classification (`Permanent` vs `Transient` vs
> `RateLimited`), shared client with timeout, hostname validation. The *pacing* of
> those retries is §2.

---

## 2. Unified timer loop (debounce + backoff + rate budget + periodic resync)

**Goal:** one state machine that (a) debounces bursts, (b) retries failures with
exponential backoff always carrying the latest best address, (c) never exceeds the
server's update rate limit, (d) force-resyncs periodically.

### The server's rate limit is the binding constraint

teleddns-server (DYNDNS2.md §10) enforces **60 updates/hour per record set** and
**600 updates/hour per token**, counted **per address family**, and answers `abuse`
(HTTP 429) when either trips. Consequences for the design:

- **Per family, the sustainable rate is one update per 60 s** — and that is a hard
  ceiling, not a target. Anything that can fire faster (a flapping temporary
  address, a link that bounces, a retry loop, a restart loop) has to be governed
  client-side, because the server's answer to overshoot is to stop accepting
  updates *and* to keep counting.
- **One request per family is free of charge relative to a combined request** (the
  counters are per family) — so §1's per-family decision costs nothing here.
- **`abuse` must be retried slowly, not permanently abandoned.** The counter is a
  1-hour in-memory window, so the first retry after `abuse` has to be on the order
  of 15 min, not 60 s. Retrying at `retry_min` would just refresh the limit.
  *(Implemented in §1 as `RETRY_RATELIMIT_MIN_S = 900`.)*
- **600/hour per token** matters when one token serves many hosts; a single client
  cannot exceed it on its own, but a shared token can. Nothing to enforce
  client-side beyond being frugal; worth a README note.

### Design — single `select!` loop

State kept in the worker (per family, `v4`/`v6` independently):

```
desired:      Option<Addr>        // latest computed best
confirmed:    Option<Addr>        // last value the server acked (good/nochg)
rejected:     Option<Addr>        // value the server refused permanently — do not resend
last_success: Instant
backoff:      Duration            // current retry interval, starts at retry_min
next_retry:   Option<Instant>
sent_at:      VecDeque<Instant>   // send timestamps in the last hour (rate budget)
settle_deadline: Option<Instant>
```

Loop body: `tokio::select!` over
1. `rx.recv()` — a netlink-derived address map arrives → recompute `desired`;
   if `desired` changed, set `settle_deadline = now + settle_delay` (coalesce) and
   clear `rejected` (a new value deserves a fresh attempt). The back-off timer is
   **not** cleared: a changing address must not be able to reset a rate-limit wait,
   which is exactly what flapping would do.
2. `sleep_until(next_deadline)` where `next_deadline = min(settle_deadline?,
   next_retry?, rate_budget_deadline?, resync_deadline)`.
3. control-socket `send_update` command (see §8) → force an attempt (bypasses
   settle and backoff, but **not** the rate budget).

On wake / after settle, for each family, send only if **all** of these hold:
- `desired.is_some()` and (`desired != confirmed` or a forced/resync trigger fired);
- `desired != rejected` (a permanent failure is not retried until the value or the
  configuration changes);
- `now >= next_retry` (back-off elapsed);
- the **rate budget** allows it (below).

Then:
- **Success (`good`/`nochg`):** `confirmed = desired`; `rejected = None`;
  `backoff = retry_min`; `last_success = now`; clear `next_retry`.
- **Transient (`911`, 5xx, transport):** `next_retry = now + backoff`;
  `backoff = min(backoff * 2, retry_max)`.
- **Rate limited (`abuse`, 429):** same, but with a floor of `retry_ratelimit_min`
  (15 min) — and mark the local budget as exhausted for the rest of the hour so we
  stop guessing and let the window drain.
- **Permanent:** log ERROR, set `rejected = desired`, no retry scheduled.
- **Resync:** if `now - last_success >= resync_period` and an eligible address
  exists, push even when `desired == confirmed` (2 requests/family/day at 12 h).

### Rate budget (client-side governor)

A sliding-window counter of *sent* requests per family, sized **below** the
server's limit so we trip our own governor rather than the server's:

- Keep the last hour of send timestamps; refuse to send when
  `sent_at.len() >= ddns_max_updates_per_hour` (default **30**, half the server's
  60) and schedule the next attempt for when the oldest entry ages out.
- Additionally enforce `ddns_min_interval` (default **60s**) between two *sent*
  requests of the same family — this replaces the current
  `MIN_UPDATE_INTERVAL_S = 30` dampening, which permits up to 120 updates/hour and
  is therefore *already* able to trip the server's limit on a flapping address.
- The governor delays, it never drops: the pending `desired` is always the latest
  value, so a burst collapses into one request carrying the final address.
- Log at WARN when the governor holds a request back, with the wait — a client that
  is permanently rate-limited by its own governor means address flapping, which is
  a §4 (metric/temporary-address) problem to fix at the source, not here.

**Startup cost:** with no persisted state, every restart re-pushes the current
address (1 request/family). A systemd restart loop therefore eats the quota. Persist
`confirmed` + timestamp to `/var/lib/teleddns/state.json` and skip the initial push
when it still matches (the resync timer bounds the staleness risk).

### Config (replaces `MIN_UPDATE_INTERVAL_S`)

| key | default | meaning |
|---|---|---|
| `ddns_settle_delay` | `5s` | debounce window after a change before pushing |
| `ddns_min_interval` | `60s` | hard minimum between two sent requests, per family |
| `ddns_max_updates_per_hour` | `30` | sliding-window cap per family (server allows 60) |
| `ddns_retry_min` | `60s` | first backoff interval after a transient failure |
| `ddns_retry_max` | `3600s` | backoff ceiling (held indefinitely) |
| `ddns_retry_ratelimit_min` | `900s` | backoff floor after `abuse`/429 |
| `ddns_resync_period` | `43200s` (12 h) | unconditional resync if any eligible addr |
| `ddns_kernel_poll_period` | `0` (disabled) | optional periodic full address re-dump |

Durations parsed as quoted strings (e.g. `"5s"`, `"1h"`) via `humantime-serde`,
matching teleddns-server's convention.

**Decision:** periodic kernel re-dump is **off by default** (user does not need it);
implemented but gated behind `ddns_kernel_poll_period` for completeness.

### Tasks

- [ ] Add `humantime-serde`; add the eight config keys with defaults.
- [ ] Replace `update_worker` with the `select!` state machine above (the interim
      `PushState` from §1 — confirmed/rejected/backoff — folds into it).
- [ ] Sliding-window rate budget per family + `ddns_min_interval`; unit-test that a
      flapping address cannot exceed `ddns_max_updates_per_hour`.
- [ ] Exponential backoff with reset-on-success and the `abuse` floor; unit-test the
      backoff sequence including a rate-limited response.
- [ ] Resync timer; force-push path (bypasses settle/backoff, not the budget).
- [ ] Persist `confirmed` state across restarts (`/var/lib/teleddns/state.json`,
      `StateDirectory=teleddns` in the unit) to keep restart loops off the quota.
- [ ] Expose the budget + backoff state in the control-socket status dump (§8).
- [ ] Graceful shutdown on **SIGTERM/SIGINT** as well as SIGHUP (systemd sends
      SIGTERM); the current HUP-only handler makes `systemctl stop` a hard kill.

---

## 3. Interface filtering — ordered regex rule chain

**Goal:** an ordered list of include/exclude rules evaluated per interface name,
first match wins, with a configurable default. Regex-capable.

### Design

New config, evaluated top-to-bottom (regexes compiled once at startup):

```yaml
interface_rules:
  - { match: "^en",  action: include }
  - { match: "^wl",  action: include }
  - { match: "^wg",  action: exclude }   # e.g. skip WireGuard tunnels
  - { match: ".*",   action: exclude }
# default_action applies if no rule matches (default: exclude)
default_action: exclude
```

- Compile to `Vec<(Regex, Action)>` once; matching per netlink message is cheap
  (a handful of messages/min), so **no precomputed name→flag cache** is needed —
  as noted, IP-level short-circuits already drop most traffic before we reach the
  filter.
- Add the `regex` crate.

### Backward compatibility

Keep parsing the legacy `interfaces: [String]` list and translate at load time:
- `"*"` → `{ match: ".*", action: include }`
- `"-name"` → `{ match: "^name$", action: exclude }`
- `"name"` → `{ match: "^name$", action: include }`
- implicit trailing `default_action: exclude`.

If both `interfaces` and `interface_rules` are present, prefer `interface_rules`
and log a warning. The config-convert tool (§7) rewrites old → new.

### Tasks

- [ ] Add `regex`; add `interface_rules` + `default_action` config.
- [ ] `enum Action { Include, Exclude }`; compile + evaluate function replacing
      `match_iface_pattern`.
- [ ] Legacy `interfaces` → rules translation shim + deprecation warning.
- [ ] Unit tests: ordering, first-match, default, legacy translation.

---

## 4. Address priority / metric algorithm review

**Goal:** a defensible, documented priority scheme covering all common address
types, with an optional user override, and a table in the README.

### Issues in the current metric

- **Temporary/privacy addresses not detected.** RFC 4941 temporary addresses
  (`IFA_F_TEMPORARY`) rotate; picking one for DDNS causes churn. They currently
  score 128, the same as a stable global — should be **de-prioritized**. This is
  also the cheapest way to stay inside the server's update budget (§2): the fix for
  a client that keeps hitting the rate limit belongs here, at the source.
- **CGNAT (100.64.0.0/10, RFC 6598) treated as public.** `Ipv4Addr::is_private()`
  excludes it, so a carrier-NAT address scores 128 (public) but isn't reachable.
  Should be gated like private space.
- **v4 benchmarking 198.18.0.0/15 and reserved 240.0.0.0/4** not excluded.
- **Interface bonus hardcoded** to `en`/`wl`; tunnels (`tun`/`wg`/`ppp`) and the
  VPN-vs-real-interface case the user hit are unaddressed.
- Metric is `u8`; a wider `i32` (or a `(tier, tiebreak)` tuple) gives room for
  clear tiers + user offsets.

### Proposed tiers (higher = preferred; `0`/negative = never selected)

| Rank | v6 | v4 | Selected? |
|---|---|---|---|
| Highest | Global unicast, **stable** (EUI-64 / permanent / not temporary) | Global unicast public | always |
| High | Global unicast **temporary/privacy** | — | always, below stable |
| Medium | Global **deprecated** | — | only if nothing better |
| Low | **ULA** (fc00::/7) | RFC1918 / **CGNAT 100.64/10** | only if `report_ipv6_ula` / `report_ipv4_private` |
| Never | link-local, loopback, multicast, unspecified, documentation, benchmarking, v4-mapped | loopback, link-local, documentation, benchmarking, reserved | never (score 0) |

Within a tier, tiebreak by interface-type bonus (physical `en`/`wl` > others),
then the `permanent` flag.

### Optional user priority rules (stretch)

Separate v4/v6 regex overrides, applied after base scoring:

```yaml
address_priority_rules:
  - { family: v4, iface: "^en", addr: "^203\\.0\\.", priority: 200 }
  - { family: v6, iface: "^en", addr: "^2a02:",     priority: 200 }
```

`priority` sets an absolute score for matching (iface, addr) pairs, letting the
user prefer one subnet on a multi-subnet interface (the one real case the user
recalls). Ship disabled/empty by default.

### Tasks

- [ ] Widen metric to `i32` (or tier tuple); refactor `compute_v4/v6_metric`.
- [ ] Detect `Temporary`; de-prioritize temporary/privacy addresses.
- [ ] Handle CGNAT 100.64/10, v4 benchmarking, reserved; gate CGNAT under
      `report_ipv4_private` (or a new `report_ipv4_cgnat`).
- [ ] Generalize interface-type bonus; consider a small default map.
- [ ] (Stretch) `address_priority_rules` with per-family regex.
- [ ] README priority table (above) as the normative reference.
- [ ] Unit tests per address class.

---

## 5. Security review — hooks & privilege

**Threat model:** the daemon runs as **root** (needed because the common hook is a
firewall reload), and hooks execute a config-defined `sh -c <cmd>` as root. This
is an inherent local privilege-escalation surface: anyone who can edit the config
(or a hook script it calls, or the `nft_sets_outfile` target) gains root.

### Findings

- **Network data → file content is safe-ish today:** `nft_sets_outfile` content is
  built from `IpAddr` Display (canonical) + `u8` prefix, so an attacker who can
  add an interface address cannot inject nft directives. Keep it that way (never
  interpolate free-form strings into the sets file).
- **Shell command is static config**, not derived from network data — so no remote
  injection. The risk is purely local config trust.
- No config-permission checks: a world-writable config or hook script is silently
  honored.
- Full command output logged at INFO — minor info leak.

### Improvements

1. **Make the hook execution user explicit and mandatory** (user's suggestion):
   ```yaml
   hooks:
     - shell: "nft -f /etc/nftables.conf"
       run_as: root            # REQUIRED for shell hooks — forces awareness
   ```
   Missing `run_as` on a `shell` hook is a config error. When `run_as` != the
   daemon user, **drop privileges** (setgid/setuid + `initgroups`) in the child
   before exec (via `nix`), so hooks can run as an unprivileged user when root
   isn't actually needed.
2. **Config & hook-script hardening checks at startup:** refuse (or loudly WARN)
   if the config file or any referenced hook script / `nft_sets_outfile` directory
   is group/world-writable or not owned by root. Recommend `chmod 0600` on the
   config (it holds the DDNS password).
3. **Sanitized child environment:** run hooks with a cleared env and a fixed
   `PATH=/usr/sbin:/usr/bin:/sbin:/bin`.
4. **systemd hardening:** the shipped unit runs as root with minimal sandboxing.
   Add `ProtectSystem=strict`, `ReadWritePaths=` (nft dir, `/run/teleddns`),
   `ProtectHome=true`, `PrivateTmp=true`, `RestrictAddressFamilies=AF_UNIX AF_NETLINK AF_INET AF_INET6`,
   `CapabilityBoundingSet=CAP_NET_ADMIN CAP_SETUID CAP_SETGID` (or `CAP_NET_ADMIN`
   only when no privilege-dropping hooks). Document a **non-root** deployment
   (`User=` + `AmbientCapabilities=CAP_NET_ADMIN`) for users with no root-requiring
   hooks — netlink address monitoring itself needs no privilege.
5. **Docs:** a "Security" section in the README stating the residual root risk and
   the `run_as` contract.

### Tasks

- [ ] `Hook.run_as` field; require it for `shell` hooks; validate at load.
- [ ] Privilege-drop wrapper for the hook child (nix setuid/setgid/initgroups).
- [ ] Startup ownership/permission checks for config + hook targets.
- [ ] Cleared/fixed child environment.
- [ ] Harden `teleddns.service` (and the deploy.sh-generated unit).
- [ ] README "Security" section.

---

## 6. Cross-compilation: drop zig?

**Current:** `cargo-zigbuild` cross-compiles 4 **gnu** targets (dynamic glibc, not
static). zig is used because `reqwest`'s rustls stack pulls in `ring`, whose C
code needs a cross C toolchain; zig supplies that cleanly. Plain `rustc`/`cargo`
cross fails for arm/riscv without a per-arch C toolchain + sysroot (the past
breakage).

**Reality:** "just rustc" is not viable while a C-based crypto backend
(`ring`/`aws-lc-rs`) is in the tree. Options, best-first:

- **A. Native runners + `cross` for the rest (recommended).** Build `amd64` on
  `ubuntu-latest` and `arm64` on the new `ubuntu-24.04-arm` runner with **plain
  cargo** (no zig, no emulation). Build `armhf` + `riscv64` with
  [`cross`](https://github.com/cross-rs/cross) (Docker images bundle the C
  toolchain). Removes zig; most targets become "just cargo".
- **B. Static musl via `cross`.** Target `*-unknown-linux-musl` for fully static,
  portable binaries (no glibc-version coupling) across all four arches. Nicest for
  the GitHub-release tarballs; `.deb`/`.rpm` can still ship the static binary.
  Still needs `cross`/Docker for the C parts, but the artifact is a single static ELF.
- **C. Keep `cargo-zigbuild`.** It works; only revisit if A/B prove simpler.

**Verification (blocking):** whatever we pick, **actually run each arch binary
under qemu-user** in CI (`teleddns --version` + `-o` dry run) — the armhf/riscv
"did not work" history means we must prove the artifacts execute, and confirm
whether they are static (`file teleddns-*`).

### Tasks

- [ ] Decide A vs B; prototype in a branch.
- [ ] Replace the zig steps in `release.yml`; add a qemu smoke-test matrix step.
- [ ] Record in README whether release binaries are static or dynamic-glibc.

---

## 7. Docs, config-convert, version bump to 0.4.0

### Version

- [ ] Bump `Cargo.toml` `0.2.0` → **`0.4.0`** (single source of truth; git tags
      already reached v0.3.1).
- [ ] Update `teleddns.spec` `Version:` + man page `.TH` + any embedded strings.
- [ ] Fresh changelog entries at `0.4.0` for both deb and rpm; **omit the golang
      history** entirely (start the narrative at the Rust rewrite).

### Config-convert

- [ ] Add a `teleddns convert-config <old.yaml> [-o new.yaml]` **subcommand**
      (preferred over a standalone script — always shipped with the binary) that
      migrates: `interfaces` → `interface_rules` + `default_action`; renames/keeps
      auth fields; fills new timing defaults; adds `run_as: root` to shell hooks
      with a warning. Non-destructive (writes a new file / stdout).
- [ ] Reference it from deb `postinst`, rpm `%post`, and `deploy.sh` (offer to
      convert an existing `/etc/teleddns/teleddns.yaml` on upgrade; never
      overwrite without backup).

### README / man / sample

- [ ] Rewrite README: trim stale bits, document the new config schema, the timing
      model (§2), the interface rule chain (§3), the priority table (§4), the
      security contract (§5), and the control socket (§8).
- [ ] Refresh `teleddns.yaml.sample` to the new schema (with the legacy form shown
      as a commented migration note).
- [ ] Update `teleddns.1` for new options/subcommands (`convert-config`, `status`,
      `send-update`).

---

## 8. Control UNIX socket (state dump + trigger)

**Goal:** a UNIX domain socket that dumps runtime state on read and accepts a
`send_update` command.

### Design

- `tokio::net::UnixListener` task at `control_socket` (default
  `/run/teleddns/teleddns.sock`; `RuntimeDirectory=teleddns` in the unit).
- Shared state: a command `Sender` into the worker (for `send_update`) plus an
  `Arc<Mutex<Status>>` (or a snapshot channel) the worker updates.
- **On connect / any read:** emit a JSON status snapshot:
  ```json
  {
    "best": { "v4": "203.0.113.5", "v6": "2a02:..." },
    "confirmed": { "v4": "203.0.113.5", "v6": "2a02:..." },
    "messages_received": 42,
    "last_success_unixts": 1737800000,
    "pending": { "state": "idle|settling|retry-backoff",
                 "next_attempt_unixts": 1737800060,
                 "backoff_secs": 120 },
    "eligible_addresses": [ ... ]
  }
  ```
- **Commands (one line):** `send_update` → force an immediate push (reset settle,
  reset backoff); `dump`/empty → status; optionally `reload`.
  Usage: `echo send_update | socat - UNIX-CONNECT:/run/teleddns/teleddns.sock`.
- **Permissions:** create the socket `0660`, owner `root`, group configurable
  (`control_socket_group`, e.g. `adm`) so non-root operators can query/trigger.
- Convenience subcommands: `teleddns status` and `teleddns send-update` that talk
  to the socket (so users need no `socat`).

### Tasks

- [ ] `control_socket` (+ group) config; `RuntimeDirectory=teleddns` in the unit.
- [ ] `UnixListener` task; `Status` snapshot type (serde JSON).
- [ ] Worker publishes status; `send_update` command channel + forced-push path (ties into §2).
- [ ] `teleddns status` / `teleddns send-update` client subcommands.
- [ ] Document in README + man page.

---

## 9. Explicit auth config (Basic + Bearer token)

**Goal:** allow credentials outside the URL, and support **Bearer** tokens (which
teleddns-server accepts — `Authorization: Bearer <key>`) in addition to HTTP
Basic. Keeping the secret out of `ddns_url` also avoids it ever appearing in a
mistyped log or process listing.

### Design

New optional, backward-compatible config fields:

```yaml
# Option A — HTTP Basic (equivalent to userinfo in the URL)
username: "myuser"
password: "mysecret"

# Option B — Bearer token (teleddns-server API key)
token: "tk_..."
```

- Precedence: `token` (Bearer) > `username`/`password` (Basic) > URL userinfo.
- Wire via reqwest: `.bearer_auth(token)` or `.basic_auth(user, Some(pass))` on the
  shared client request.
- Never log the token; extend redaction so a token is masked in any diagnostic.
- Validation: reject a config that sets both `token` and `username`/`password`.

### Tasks

- [ ] Add `username`/`password`/`token` config fields.
- [ ] Apply Basic/Bearer on the request; precedence + mutual-exclusion validation.
- [ ] Token redaction in logs.
- [ ] Document in `DYNDNS2.md` (auth section) and the README/sample.

---

## Cross-cutting code-quality fixes (fold into the above)

- [ ] Shared `reqwest::Client` with timeout (§1) — today each push builds a client
      and can hang with no timeout, blocking the worker.
- [ ] Replace `.expect()` on MPSC sends and signal binding with graceful handling.
- [ ] Single-source the version string (use `env!("CARGO_PKG_VERSION")` for the UA
      and any logs).
- [ ] Consider splitting `main.rs` into modules (`config`, `netlink`, `metric`,
      `ddns`, `hooks`, `control`, `worker`) once it grows with the above.

---

## Suggested sequencing

1. **Foundation:** version bump + config schema (`humantime-serde`, `regex`),
   legacy shim, `convert-config` (§7, parts of §2/§3).
2. **Core behavior:** timer state machine (§2) + dyndns2 correctness (§1) — these
   are the highest-value fixes and are tightly coupled.
3. **Selection:** metric rework + priority table (§4); interface rule chain (§3).
4. **Ops:** control socket (§8); security hardening + `run_as` (§5).
5. **Ship:** cross-compilation cleanup (§6), README/man rewrite, deb/rpm at 0.4.0.

Breaking config changes land together in **v0.4.0**, with `convert-config`
bridging existing installs.
