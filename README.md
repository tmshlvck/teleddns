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
| Debian, Ubuntu, Pop!_OS, Mint, etc. | APT repository (apt.telephant.eu) |
| Fedora | COPR repository |
| Other Linux | Binary download from GitHub releases |
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
echo "deb [signed-by=/usr/share/keyrings/teleddns.gpg] https://apt.telephant.eu/teleddns/ stable main" | sudo tee /etc/apt/sources.list.d/teleddns.list
sudo apt update
sudo apt install teleddns
```

#### Fedora (COPR)

```bash
sudo dnf copr enable tmshlvck/teleddns
sudo dnf install teleddns
```

#### Binary Download

Pre-compiled binaries for amd64, arm64, armhf, and riscv64 are available on [GitHub Releases](https://github.com/tmshlvck/teleddns/releases).

Then edit and install `teleddns.service`.

### Installation from sources

Clone this repo, build and install the software:
```
git clone https://github.com/tmshlvck/teleddns.git
cd teleddns
cargo install --root /usr/local --path .
```

Then edit and install `teleddns.service`.

### Setup the client

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
Saved state to "/var/lib/teleddns/state.json"
Sucessfully shutdown
```

A second run right afterwards publishes nothing and says so — the address is already
on record:
```
Adopting 2a02:aa11:380:300:ef1a:78c9:f995:e73d from the state file (confirmed 12s ago)
```

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
| `state_file` | `/var/lib/teleddns/state.json` | remembers the acknowledged addresses across restarts (`''` disables) |

Behaviour worth knowing:

- **A permanent failure is not retried.** `badauth`, `!yours`, `nohost`, `notfqdn`
  and friends mean the request or the configuration is wrong; the client logs an
  ERROR and waits for the address to change or for the next resync tick instead of
  hammering the server. See [DYNDNS2.md](DYNDNS2.md) for the full keyword table.
- **The state file keeps restarts cheap.** Without it, every restart re-publishes
  the current address; with a crash loop that alone can exhaust the hourly budget.
  It is ignored once it is older than `ddns_resync_period`, and it is not believed
  if the hostname or the server in the config changed.
- **`teleddns -o` uses the same state**, so running it from cron or a
  NetworkManager dispatcher script does not re-publish an unchanged address on
  every tick. It exits non-zero when the update did not go through.
- **If the governor keeps holding updates back** (a WARN in the log), the address is
  flapping — usually an RFC 4941 temporary/privacy address being picked. Fix the
  selection, not the limit.

