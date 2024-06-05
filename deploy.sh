#!/bin/bash

if (( $# != 3 )); then
    echo "Usage:\ncurl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>"
    echo "URL: DDNS API server URL\ndomainname: domain name (FQDN) to set"
fi

DDNSURL="$1"
DDNSNAME="$2"

if command -v apt-get >/dev/null 2>&1; then
    echo "Detected Debian-based distro"
    sudo apt-get update
    sudo apt-get -y install python3-pip
fi

if command -v dnf >/dev/null 2>&1; then
    echo "Detected Fedora-based distro"
    sudo dnf -y install python3-pip
fi

if command -v pacman >/dev/null 2>&1; then
    echo "Detected Arch-based distro"
    sudo pacman --noconfirm -S python-pip
fi

sudo PIP_BREAK_SYSTEM_PACKAGES=1 pip install teleddns
sudo mkdir -p /etc/teleddns/
sudo bash -c "cat << EOF > /etc/teleddns/teleddns.yaml
---
debug: False

ddns_url: '$DDNSURL'
hostname: '$DDNSNAME'
enable_ipv6: True
enable_ipv4: False
interfaces:
        - '*'
EOF"

sudo bash -c "cat << EOF > /etc/systemd/system/teleddns.service
[Unit]
Description=teleddns systemd service
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/teleddns -n
Restart=always

[Install]
WantedBy=multi-user.target
EOF"

systemctl daemon-reload
systemctl enable teleddns.service
systemctl restart teleddns.service

echo "Succesfully deployed teleddns with DDNS domain name: $DDNSNAME"
