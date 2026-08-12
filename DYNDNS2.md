# dyndns2 in teleddns (client side)

What this client puts on the wire, and how it interprets what comes back. The
**normative** contract is
[`DYNDNS2.md` in teleddns-server](https://github.com/tmshlvck/teleddns-server/blob/main/DYNDNS2.md);
all `§n` references below — and in the source comments — point there. This file only
records our side of it: the subset we use, and why.

Verified against **teleddns-server 0.4.0**. The client also aims to work against
generic dyndns2 services (dyn.com and clones, ddclient-compatible servers).

## What we send

```
GET <ddns_url>?hostname=<fqdn>&myip=<v4>          # IPv4 update
GET <ddns_url>?hostname=<fqdn>&myipv6=<v6>        # IPv6 update
User-Agent: teleddns/<version>
```

- **`GET`**, one request per address family, with a 30 s timeout on a shared
  keep-alive client.
- **`myip` carries IPv4 only, `myipv6` carries IPv6 only.** teleddns-server
  auto-detects the family in `myip`, but generic services do not, and an IPv4
  address in `myipv6` is `notfqdn` (§4).
- **Exactly one hostname per request.** The server accepts a comma-separated list of
  up to 20 names (§4), and this client deliberately does not use it: it publishes one
  name, so a comma in the configured `hostname` is a configuration error and is
  rejected at startup along with anything else outside the accepted name syntax
  (labels of 1–63 characters from `[A-Za-z0-9-_]`, an optional leading `*`, a
  tolerated trailing dot).
- **Exactly one address per request.** `myip` also accepts a list (§5), and we do not
  use that either — two addresses of the same family are a malformed request, and the
  cross-family case is covered by sending each family separately.
- **We never omit the address.** Source-address detection (§4) is the server's
  fallback for clients that cannot see their own address; we compute the best local
  address ourselves, which is also what lets us verify what was published.
- **One request per family rather than one combined request** — deliberate: §9 merges
  a dual-family request into a single line carrying the *most severe* of the two
  keywords, which cannot be attributed back to a family, while §10 counts the rate
  limit *per family*, so the second request costs no extra quota.

Credentials go in the `ddns_url` userinfo today (reqwest turns them into HTTP Basic).
The URL is redacted before it reaches the log. Bearer tokens and out-of-URL
credentials are TODO.md §9.

## How we read the answer

Per §12.2 the body decides and the HTTP status is only a fallback:

1. Take the first non-empty line (§7 is one line per hostname, and a request-level
   failure is a single line — with one hostname per request, either way our result is
   the first line).
2. First whitespace-separated token is the keyword, matched case-insensitively; the
   remainder of a `good`/`nochg` line is the address set now on record.
3. Unrecognized or empty body → classify by HTTP status. This is also what keeps
   pre-0.4.0 teleddns-server (which answered `200` with a JSON body) working, and
   what handles the non-keyword `403 forbidden` from the operator's IP allow-list
   (§1).

On `good`/`nochg` we compare the reported addresses against the one we sent and warn
on a mismatch. The list legitimately contains the other family's address too, so only
the absence of *our* address is suspicious.

### Keyword → action

| Keyword | Class | What we do |
|---|---|---|
| `good`, `nochg` | success | adopt as confirmed; never re-send the same value |
| `abuse`, HTTP 429 | rate limited | retry, but not before 15 min (§10 counts a 1-hour window; a 60 s retry would just refresh it) |
| `911`, `dnserr`, 5xx, transport error | transient | retry with exponential back-off, 60 s → 1 h |
| `nohost`, `notfqdn`, `numhost`, `!yours`, `badauth`, `badagent`, `!donator`, `!active`, `ko`, other 4xx | permanent | log an ERROR and **stop**: no retry until the address or the configuration changes (§12.3 — retrying `badauth` in a loop is how clients get blocked) |

`ko` is DuckDNS' rejection keyword (sent with HTTP 200); `nofqdn` is accepted as a
spelling variant of `notfqdn` seen in the wild. `dnserr` and `!donator` are never sent
by teleddns-server (§11) but are part of the dyndns2 vocabulary.

Losing an address is not an update: the DDNS endpoint never deletes a record set
(§6), so we just forget what we published and wait for a new address.

## Rate limiting

teleddns-server allows **60 updates/hour per record set** and **600/hour per token**,
counted per family (§10). The client governs itself to stay inside that budget rather
than discovering it through an `abuse`:

- a debounce window (`ddns_settle_delay`, 5 s) so a link bounce is one update, not
  three;
- a hard minimum between two updates of the same family (`ddns_min_interval`, 60 s —
  the sustainable rate the server's limit implies);
- a sliding one-hour window capped at `ddns_max_updates_per_hour` (30, half of what
  the server allows), so our own governor trips first;
- exponential back-off on failure, with an hour-scale floor after `abuse`;
- an unconditional `ddns_resync_period` (12 h) re-publish, which is also the only
  thing that ever retries a permanently rejected name.

The governor delays, it never drops: the pending address is always the latest one, so
a burst collapses into a single update carrying the final value. When it does hold an
update back it says so at WARN — that means the address is flapping (typically an RFC
4941 temporary address being selected), which is worth fixing at the source rather
than by raising the limit.

The 600/hour per token limit is shared by every host using the same token; one client
cannot exceed it on its own, but a fleet sharing one key can.
