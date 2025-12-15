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
* [Fedora COPR](https://copr.fedorainfracloud.org/coprs/tmshlvck/teleddns/package/teleddns/) ![Copr build status](https://copr.fedorainfracloud.org/coprs/tmshlvck/teleddns/package/teleddns/status_image/last_build.png)
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
[2025-02-02T23:16:05Z INFO  teleddns] Read config file /etc/teleddns/teleddns.yaml finished
[2025-02-02T23:16:05Z INFO  teleddns] Set log level Info
[2025-02-02T23:16:05Z INFO  teleddns] Main loop now waiting for the oneshot run to finish
[2025-02-02T23:16:05Z INFO  teleddns] Trigger the first update
[2025-02-02T23:16:35Z INFO  teleddns] Sending DDNS: 2a02:aa11:380:300:ef1a:78c9:f995:e73d
[2025-02-02T23:16:36Z INFO  teleddns] DDNS GET to URL https://th:<PASSWORD>@slon.telephant.eu/ddns/update?myip=2a02%3Aaa11%3A380%3A300%3Aef1a%3A78c9%3Af995%3Ae73d&hostname=tapir.d.telephant.eu succeeded with code: 200 OK, text: Ok("{\"detail\":\"DDNS noop AAAA label='tapir' zone.origin='d.telephant.eu.' -> 2a02:aa11:380:300:ef1a:78c9:f995:e73d\"}")
[2025-02-02T23:16:36Z INFO  teleddns] Sucessfully shutdown
```

