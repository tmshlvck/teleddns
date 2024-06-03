# TeleDDNS

Advanced DDNS client with daemonization (as systemd service), or one-shot running capability
and password hashing option for [ddnsmapi](https://github.com/tmshlvck/ddnsmapi).

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
* Python 3.9+
* systemd
* pip

### Installation from git

Install required packages:
```
sudo apt-get install python3-pip python3-poetry git
```

Clone this repo, build and install the software:
```
git clone https://github.com/tmshlvck/teleddns.git
cd teleddns
poetry build
sudo pip install dist/teleddns-*.whl
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
teleddns -d
```

The exected output should look like this:
```
[th@hroch ~]$ teleddns -d
2023-11-30 01:41:44,863 DEBUG netlink: addr=127.0.0.1/8 afi=2 ifa_flags=128
2023-11-30 01:41:44,863 DEBUG netlink: iface index=1 name=lo state=up opestate=UNKNOWN
2023-11-30 01:41:44,863 DEBUG netlink: iface index=1 flags 65609 IFF_LOWER_UP=True IFF_UP=True IFF_RUNNING=True
2023-11-30 01:41:44,863 DEBUG considering IP address 127.0.0.1 from interface lo with ifa_flags 128
2023-11-30 01:41:44,864 DEBUG  -> interface lo denied by interface filter
2023-11-30 01:41:44,864 DEBUG netlink: addr=192.168.1.82/24 afi=2 ifa_flags=512
2023-11-30 01:41:44,864 DEBUG netlink: iface index=2 name=wlp6s0 state=up opestate=UP
2023-11-30 01:41:44,864 DEBUG netlink: iface index=2 flags 69699 IFF_LOWER_UP=True IFF_UP=True IFF_RUNNING=True
2023-11-30 01:41:44,864 DEBUG considering IP address 192.168.1.82 from interface wlp6s0 with ifa_flags 512
2023-11-30 01:41:44,864 DEBUG  -> interface wlp6s0 allowed by interface filter
2023-11-30 01:41:44,864 DEBUG netlink: addr=::1/128 afi=10 ifa_flags=640
2023-11-30 01:41:44,865 DEBUG netlink: iface index=1 name=lo state=up opestate=UNKNOWN
2023-11-30 01:41:44,865 DEBUG netlink: iface index=1 flags 65609 IFF_LOWER_UP=True IFF_UP=True IFF_RUNNING=True
2023-11-30 01:41:44,865 DEBUG considering IP address ::1 from interface lo with ifa_flags 640
2023-11-30 01:41:44,865 DEBUG  -> interface lo denied by interface filter
2023-11-30 01:41:44,865 DEBUG netlink: addr=2a02:aa11:380:300:76be:ed8e:57db:1b73/64 afi=10 ifa_flags=512
2023-11-30 01:41:44,866 DEBUG netlink: iface index=2 name=wlp6s0 state=up opestate=UP
2023-11-30 01:41:44,866 DEBUG netlink: iface index=2 flags 69699 IFF_LOWER_UP=True IFF_UP=True IFF_RUNNING=True
2023-11-30 01:41:44,866 DEBUG considering IP address 2a02:aa11:380:300:76be:ed8e:57db:1b73 from interface wlp6s0 with ifa_flags 512
2023-11-30 01:41:44,866 DEBUG  -> interface wlp6s0 allowed by interface filter
2023-11-30 01:41:44,866 DEBUG IPv6 address: 2a02:aa11:380:300:76be:ed8e:57db:1b73
2023-11-30 01:41:44,866 DEBUG   -> global_unicast
2023-11-30 01:41:44,866 DEBUG netlink: addr=fe80::2a31:bec4:4aa6:5fc9/64 afi=10 ifa_flags=640
2023-11-30 01:41:44,866 DEBUG netlink: iface index=2 name=wlp6s0 state=up opestate=UP
2023-11-30 01:41:44,867 DEBUG netlink: iface index=2 flags 69699 IFF_LOWER_UP=True IFF_UP=True IFF_RUNNING=True
2023-11-30 01:41:44,867 DEBUG considering IP address fe80::2a31:bec4:4aa6:5fc9 from interface wlp6s0 with ifa_flags 640
2023-11-30 01:41:44,867 DEBUG  -> interface wlp6s0 allowed by interface filter
2023-11-30 01:41:44,867 DEBUG IPv6 address: fe80::2a31:bec4:4aa6:5fc9
2023-11-30 01:41:44,867 DEBUG   -> private
2023-11-30 01:41:44,867 INFO ddns_client: Selected myip4=None myip6=2a02:aa11:380:300:76be:ed8e:57db:1b73 with oldip4=None oldip6=None
2023-11-30 01:41:44,867 DEBUG Executing update with hostname hroch.d.telephant.eu and myip 2a02:aa11:380:300:76be:ed8e:57db:1b73
2023-11-30 01:41:44,868 DEBUG Starting new HTTPS connection (1): slon.telephant.eu:443
2023-11-30 01:41:45,358 DEBUG https://slon.telephant.eu:443 "GET /ddns/update?hostname=hroch.d.telephant.eu&myip=2a02%3Aaa11%3A380%3A300%3A76be%3Aed8e%3A57db%3A1b73 HTTP/1.1" 200 33
2023-11-30 01:41:45,358 DEBUG Received response code: 200 result: {'success': True, 'message': 'noop'}
2023-11-30 01:41:45,359 INFO IPv6 address update sent successfully for hroch.d.telephant.eu
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
