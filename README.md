# TeleDDNS

Advanced DDNS client with daemonization (as systemd service), or one-shot running capability
and compatibility for [teleddns-server](https://github.com/tmshlvck/teleddns-server).

When the TeleDDNS runs in daemonized mode it listens for Netlink messages and pools the updates
to minimize both the DDNS convergence time and resource usage.

## Installation

### TL;DR

```
curl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>"
```

Where URL is the API URL including the username and password in `https://user:pass@host.domain.tld/ddns/update` form and domainname is the FQDN of the host (example: `testhost.d.telephant.eu`).

### Requirements / prerequisities

* Fairly recent Linux - say Ubuntu 20.04 or similar
* Cargo
* OpenSSL headers
* systemd

### Installation from git

Install required packages:
```
sudo apt-get install cargo
```

Clone this repo, build and install the software:
```
git clone https://github.com/tmshlvck/teleddns.git
cd teleddns
cargo install --root /usr/local --path .
```

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

### Create, enable and start a systemd unit

```
sudo cp teleddns.service /etc/systemd/system/teleddns.service
sudo systemctl damoen-reload
sudo systemctl enable teleddns
sudo systemctl restart teleddns
```

Check systemd unit with `systemctl status teleddns`. The expected result should be similar to:

```
[th@hroch ~]$ sudo systemctl status teleddns.service
● teleddns.service - teleddns systemd service
     Loaded: loaded (/etc/systemd/system/teleddns.service; enabled; preset: disabled)
    Drop-In: /usr/lib/systemd/system/service.d
             └─10-timeout-abort.conf
     Active: active (running) since Thu 2023-11-30 01:41:56 CET; 9min ago
   Main PID: 145955 (teleddns)
      Tasks: 2 (limit: 37001)
     Memory: 33.8M
        CPU: 297ms
     CGroup: /system.slice/teleddns.service
             └─145955 /usr/bin/python3 /usr/local/bin/teleddns -n

Nov 30 01:41:56 hroch systemd[1]: Started teleddns.service - teleddns systemd service.
Nov 30 01:41:57 hroch teleddns[145955]: 2023-11-30 01:41:57,070 INFO ddns_client: Selected myip4=None myip6=2a02:aa11:380:300:76be:ed8e:57db:1b73 with oldip4=None oldip6=None
Nov 30 01:41:57 hroch teleddns[145955]: 2023-11-30 01:41:57,520 INFO IPv6 address update sent successfully for hroch.d.telephant.eu
Nov 30 01:47:57 hroch teleddns[145955]: 2023-11-30 01:47:57,525 INFO ddns_client: Selected myip4=None myip6=2a02:aa11:380:300:76be:ed8e:57db:1b73 with oldip4=None oldip6=2a02:aa11:380:300:76be:ed8e:57db:1b73
```
