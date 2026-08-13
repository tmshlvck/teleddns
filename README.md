# TeleDDNS

Advanced DDNS client with daemonization (as systemd service), or one-shot running capability
and compatibility for [teleddns-server](https://github.com/tmshlvck/teleddns-server).

When the TeleDDNS runs in daemonized mode it listens for Netlink messages and pools the updates
to minimize both the DDNS convergence time and resource usage.

## Installation

### TL;DR (Automated Install)

```
curl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>
```

Where URL is the API URL including the username and password in `https://user:pass@host.domain.tld/ddns/update` form and domainname is the FQDN of the host (example: `testhost.d.telephant.eu`).

The deploy script automatically detects your distribution and installs using the best available method:

| Distribution | Installation Method |
|--------------|---------------------|
| Debian, Ubuntu, Pop!_OS, Mint, etc. (supported suites) | APT repository (apt.telephant.eu) |
| Fedora | COPR repository |
| Other Linux, or a release we do not package | Static binary from GitHub releases |
| Fallback (if binary unavailable) | `cargo install` from crates.io |

The script will:
- Install teleddns using the appropriate package manager or binary
- Create configuration in `/etc/teleddns/teleddns.yaml` (if not already present)
- Enable and start the systemd service

After installation, check status with:
```
sudo systemctl status teleddns
sudo journalctl -u teleddns -f
```

### Manual Installation

#### Packages

* [crates.io](https://crates.io/crates/teleddns) - use `cargo install teleddns`
* [Fedora COPR](https://copr.fedorainfracloud.org/coprs/tmshlvck/teleddns/package/teleddns/) - use `dnf copr enable tmshlvck/teleddns`
* [Debian/Ubuntu APT](https://apt.telephant.eu/teleddns/) - see [installation instructions](https://apt.telephant.eu/teleddns/INSTALL-DEB.txt)

#### Debian/Ubuntu (APT)

```bash
curl -fsSL https://apt.telephant.eu/teleddns/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/teleddns.gpg
echo "deb [signed-by=/usr/share/keyrings/teleddns.gpg] https://apt.telephant.eu/teleddns/ $(. /etc/os-release; echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}") main" | sudo tee /etc/apt/sources.list.d/teleddns.list
sudo apt update
sudo apt install teleddns
```

Supported suites are `trixie` (Debian 13), `noble` (Ubuntu 24.04 LTS) and
`resolute` (Ubuntu 26.04 LTS). Use the suite matching your system — packages are
built against each suite's glibc and a package from a newer suite will not run on
an older release. On anything else, use the static binary below.

#### Fedora (COPR)

```bash
sudo dnf copr enable tmshlvck/teleddns
sudo dnf install teleddns
```

#### Binary Download

Pre-compiled binaries for amd64, arm64 and armhf are available on [GitHub Releases](https://github.com/tmshlvck/teleddns/releases).

These are **statically linked** (musl), so they run on any distribution regardless
of its glibc version. This is the recommended option for distributions we do not
package for, and the only pre-built option for armhf on non-Debian systems.

One consequence of static linking: name resolution uses musl's own resolver, which
reads `/etc/resolv.conf` and ignores `nsswitch.conf`. That is fine for the usual
case of a public DDNS hostname, including on systemd-resolved systems, but if you
depend on a custom NSS module to resolve your DDNS server, use a package instead.

Then edit and install `teleddns.service`.

#### What each artifact is

| Artifact | Linking | Architectures |
|---|---|---|
| GitHub release tarballs | static musl, portable | amd64, arm64, armhf |
| `.deb` (apt.telephant.eu) | dynamic, built against the glibc of the suite it is published to | amd64, arm64, armhf, riscv64 — per suite |
| `.rpm` (COPR) | dynamic, built natively by Fedora | x86_64, aarch64, riscv64 |

`.deb` packages are built separately for each supported suite, so the package you
install always matches your distribution's glibc. A package built for a newer
release will not run on an older one, which is why they are not interchangeable.

There is **no armv7hf RPM**: Fedora has no 32-bit ARM build target. On 32-bit ARM
use the `.deb` or the static binary above.

There is **no static riscv64 binary**: the cross-compilation toolchain we use has no
released riscv64 musl image. On riscv64 use the `.deb` or the RPM, both of which are
built for it, or build from source.

### Installation from sources

Clone this repo, build and install the software:
```
git clone https://github.com/tmshlvck/teleddns.git
cd teleddns
cargo install --root /usr/local --path .
```

Then edit and install `teleddns.service`.

### Setup the client

Unknown keys in the configuration are **refused**, not ignored: teleddns exits with
an error naming the offending key and the line it is on, and listing the keys it does
accept. A misspelled setting is a mistake worth seeing at startup rather than one that
silently leaves the default in place.

Create configuration file (modify the following example):
```
sudo mkdir /etc/teleddns/
sudo bash -c 'cat <<EOF >/etc/teleddns/teleddns.yaml
---
debug: False

ddns_url: 'https://USERNAME:PASSWORD@ddns-server.domain.tld/ddns/update'
hostname: 'myhostname.ddns.domain.tld'
enable_ipv6: True
enable_ipv4: False
interfaces:
- '*'
hooks:
- nft_sets_outfile: "/etc/nftables.d/00-localnets.rules"
  shell: "nft -f /etc/nftables.conf"

EOF'
```

Test the client:
```
teleddns -o
```

The exected output should look like this:
```
Read config file /etc/teleddns/teleddns.yaml finished
Set log level Info
Running in oneshot mode...
Sending DDNS: 2a02:aa11:380:300:ef1a:78c9:f995:e73d
DDNS GET to URL https://th:<PASSWORD>@slon.telephant.eu/ddns/update?myipv6=2a02%3Aaa11%3A380%3A300%3Aef1a%3A78c9%3Af995%3Ae73d&hostname=tapir.d.telephant.eu succeeded: HTTP 200 OK, body: "good 2a02:aa11:380:300:ef1a:78c9:f995:e73d"
Sucessfully shutdown
```

It exits non-zero if the update did not go through, so it is also usable as a check.

### Daemon mode vs. `--oneshot`

**Run the daemon for normal operation.** It watches Netlink and publishes when an
address actually changes, which is what the DDNS protocol expects. It is also the
cheaper option locally: one process with a small resident set, no repeated
interpreter or binary start-up, and no work at all while the address is stable.

**`--oneshot` is a manual tool** for forcing an update through and for debugging the
DDNS exchange and the hook scripts. Every run publishes unconditionally.

**Running `--oneshot` from cron or a timer is an anti-pattern.** It re-publishes an
address that has not changed on every tick, which is exactly what the server's rate
limit exists to stop — a five-minute timer spends 12 updates per hour per family for
no benefit, and a one-minute timer sits permanently on `abuse`. It also reacts to a
real address change up to a whole period late, where the daemon reacts in seconds.
If you are reaching for a timer because the daemon is not picking a change up, that
is worth reporting as a bug rather than working around.

## Update timing and rate limiting

teleddns-server accepts **60 updates per hour and address family** per record set
(and 600 per hour and token); past that it answers `abuse` and stops accepting
updates for a while. A DDNS client is not supposed to send updates on a timer at
all — only when the address actually changes — so the client is built to stay well
inside that budget, and every knob below is optional with a default that already
does.

| key | default | what it does |
|---|---|---|
| `ddns_settle_delay` | `5s` | wait for the address set to stop changing before publishing, so a link bounce becomes one update |
| `ddns_min_interval` | `60s` | hard minimum between two updates of the same family |
| `ddns_max_updates_per_hour` | `30` | sliding-window cap per family, half of what the server allows |
| `ddns_retry_min` / `ddns_retry_max` | `60s` / `1h` | exponential back-off after a transient failure |
| `ddns_retry_ratelimit_min` | `15min` | back-off floor after `abuse`/HTTP 429 — the server counts a one-hour window |
| `ddns_resync_period` | `12h` | re-publish unconditionally this often (`0s` disables) |
| `ddns_kernel_poll_period` | `0s` (off) | periodically re-read all addresses from the kernel |
| `http_timeout` | `30s` | timeout for one DDNS request |

Behaviour worth knowing:

- **A permanent failure is not retried.** `badauth`, `!yours`, `nohost`, `notfqdn`
  and friends mean the request or the configuration is wrong; the client logs an
  ERROR and waits for the address to change or for the next resync tick instead of
  hammering the server. See [DYNDNS2.md](DYNDNS2.md) for the full keyword table.
- **Nothing is remembered across restarts.** The daemon starts with a clean slate
  and publishes the current address once on startup, which costs one update per
  family. That is deliberate: keeping no state on disk keeps the client simple, and
  one update per restart is nowhere near the budget. The shipped unit sets
  `StartLimitBurst` so a crash loop gives up instead of restarting forever.
- **`teleddns -o` always publishes.** It is a manual tool — see below.
- **If the governor keeps holding updates back** (a WARN in the log), the address is
  flapping — usually an RFC 4941 temporary/privacy address being picked. Fix the
  selection, not the limit.

