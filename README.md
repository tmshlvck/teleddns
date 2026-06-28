# TeleDDNS

Advanced DDNS client with daemonization (as a systemd service) or one-shot
running capability, for use with
[teleddns-server](https://github.com/tmshlvck/teleddns-server).

When TeleDDNS runs in daemon mode it watches the kernel's `rtnetlink` socket for
interface and address changes, selects the most appropriate public address per
host, and reports it to the server over HTTP — pooling updates to minimize both
DDNS convergence time and resource usage.

> As of **v0.3.0** TeleDDNS is implemented in Go. It is a drop-in replacement
> for the earlier Rust client: same binary name, same `/etc/teleddns/teleddns.yaml`
> configuration, same systemd unit. See [`PRD.md`](PRD.md) for the design and
> the implementation roadmap.

## Installation

### TL;DR (automated install)

```
curl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>
```

Where `URL` is the API URL including the username and password in
`https://user:pass@host.domain.tld/ddns/update` form and `domainname` is the
FQDN of the host (example: `testhost.d.telephant.eu`).

The deploy script auto-detects your distribution and installs using the best
available method:

| Distribution | Installation method |
|--------------|---------------------|
| Debian, Ubuntu, Pop!_OS, Mint, etc. | APT repository (apt.telephant.eu) |
| Fedora | COPR repository |
| Other Linux | Binary download from GitHub releases |
| Fallback (if no binary) | Build from source with the Go toolchain |

The script installs the package (or binary), creates a configuration in
`/etc/teleddns/teleddns.yaml` if one is not already present, and enables and
starts the systemd service. Afterwards:

```
sudo systemctl status teleddns
sudo journalctl -u teleddns -f
```

### Manual installation

#### Packages

* [Fedora COPR](https://copr.fedorainfracloud.org/coprs/tmshlvck/teleddns/package/teleddns/) ![Copr build status](https://copr.fedorainfracloud.org/coprs/tmshlvck/teleddns/package/teleddns/status_image/last_build.png)
* [Debian/Ubuntu APT](https://apt.telephant.eu/teleddns/)

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

#### Binary download

Pre-compiled static binaries for `amd64`, `arm64`, `armhf`, and `riscv64` are
attached to each [GitHub Release](https://github.com/tmshlvck/teleddns/releases)
as `teleddns-<arch>`. Download the one for your architecture, install it, then
edit and install `teleddns.service` (see below):

```bash
sudo install -m 755 teleddns-amd64 /usr/local/bin/teleddns
```

### Installation from source

Requires the Go toolchain (1.26+):

```bash
git clone https://github.com/tmshlvck/teleddns.git
cd teleddns
go build -o teleddns ./cmd/teleddns
sudo install -m 755 teleddns /usr/local/bin/teleddns
```

Then edit and install `teleddns.service`.

## Setup the client

Create the configuration file (adjust the example):

```bash
sudo mkdir -p /etc/teleddns/
sudo bash -c 'cat <<EOF >/etc/teleddns/teleddns.yaml
---
debug: false
ddns_url: "https://USERNAME:PASSWORD@ddns-server.domain.tld/ddns/update"
hostname: "myhostname.ddns.domain.tld"
enable_ipv6: true
enable_ipv4: false
interfaces:
- "*"
#hooks:
#- nft_sets_outfile: "/etc/nftables.d/00-localnets.rules"
#  shell: "nft -f /etc/nftables.conf"
EOF'
```

See [`teleddns.yaml.sample`](teleddns.yaml.sample) for the fully annotated
schema.

Test the client with a single update cycle:

```bash
teleddns -o
```

The expected output looks like this:

```
level=INFO msg="teleddns-go starting" version=0.3.0 config=/etc/teleddns/teleddns.yaml mode=oneshot interfaces=[*] enable_ipv6=true enable_ipv4=false
level=INFO msg="address state" addresses="127.0.0.1/8 iface=lo metric=0; 192.168.1.25/24 iface=enp2s0 metric=0; ::1/128 iface=lo metric=0; 2001:db8:1:2:a8e0:e08:464a:107/64 iface=enp2s0 metric=144; fe80::dd30:e743:84be:2d2a/64 iface=enp2s0 metric=0"
level=INFO msg="selected best addresses" best6=2001:db8:1:2:a8e0:e08:464a:107 best4=none
level=INFO msg="sending DDNS update" ip=2001:db8:1:2:a8e0:e08:464a:107 hostname=myhostname.ddns.domain.tld url="https://USERNAME:<PASSWORD>@ddns-server.domain.tld/ddns/update?hostname=myhostname.ddns.domain.tld&myip=2001%3Adb8%3A1%3A2%3Aa8e0%3Ae08%3A464a%3A107"
level=INFO msg="DDNS update succeeded" url="https://USERNAME:<PASSWORD>@ddns-server.domain.tld/ddns/update?..." status=200 body="{\"detail\":\"DDNS noop AAAA label='myhostname' -> 2001:db8:1:2:a8e0:e08:464a:107\"}"
```

The initial state dump runs unprivileged. Subscribing to rtnetlink multicast
groups for live notifications (daemon mode) may require `CAP_NET_ADMIN` or root.

## Enable and start the systemd service

If you installed from a package the unit is already present. For binary or
source installs:

```bash
sudo cp teleddns.service /etc/systemd/system/teleddns.service
sudo systemctl daemon-reload
sudo systemctl enable teleddns
sudo systemctl restart teleddns
```

Check it with `systemctl status teleddns`:

```
● teleddns.service - TeleDDNS - Advanced DDNS Client
     Loaded: loaded (/etc/systemd/system/teleddns.service; enabled)
     Active: active (running)
   Main PID: 145955 (teleddns)

systemd[1]: Started teleddns.service - TeleDDNS - Advanced DDNS Client.
teleddns[145955]: level=INFO msg="DDNS update succeeded" status=200 ...
```

Follow the logs with `journalctl -u teleddns -f`.

## Usage

```sh
teleddns                         # run as a daemon (default config path)
teleddns -c ./teleddns.yaml      # daemon, explicit config
teleddns -o                      # one-shot: one update cycle then exit
teleddns -V                      # print version
```

| Flag | Description |
|------|-------------|
| `-c`, `--config <FILE>` | config file path (default `/etc/teleddns/teleddns.yaml`) |
| `-o`, `--oneshot` | run one update cycle and exit (non-zero exit if the push fails) |
| `-v`, `--verbose` | increase log verbosity (repeatable) |
| `-q`, `--quiet` | decrease log verbosity (repeatable) |
| `-V`, `--version` | print version and exit |

`SIGINT`, `SIGTERM` and `SIGHUP` all trigger a graceful shutdown.

## License

GPLv3. See [`LICENSE`](LICENSE).
