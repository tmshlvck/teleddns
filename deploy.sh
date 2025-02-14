#!/bin/bash

TELEDDNS_VERSION="v0.1.3"

if (( $# != 2 )); then
    echo -e "Usage:\ncurl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>"
    echo -e "URL: DDNS API server URL\ndomainname: domain name (FQDN) to set"
    exit -1
fi

DDNSURL="$1"
DDNSNAME="$2"

if [ `uname -m` == "x86_64" ]; then
    curl -o /tmp/teleddns.tar.gz -L "https://github.com/tmshlvck/teleddns/releases/download/$TELEDDNS_VERSION/teleddns-x86_64-unknown-linux-gnu.tar.gz"
elif [ `uname -m` == "aarch64" ]; then
    curl -o /tmp/teleddns.tar.gz -L "https://github.com/tmshlvck/teleddns/releases/download/$TELEDDNS_VERSION/teleddns-aarch64-unknown-linux-gnu.tar.gz"
elif [ `uname -m` == "armv7l" ]; then
    curl -o /tmp/teleddns.tar.gz -L "https://github.com/tmshlvck/teleddns/releases/download/$TELEDDNS_VERSION/teleddns-armv7-unknown-linux-gnueabihf.tar.gz"
elif [ `uname -m` == "riscv64" ]; then
    curl -o /tmp/teleddns.tar.gz -L "https://github.com/tmshlvck/teleddns/releases/download/$TELEDDNS_VERSION/teleddns-riscv64gc-unknown-linux-gnu.tar.gz"
else
    echo "Error: Unsupported architecture `uname -m`."
    exit -1
fi

sudo mkdir -p /usr/local/bin
cd /usr/local/bin
sudo tar xf /tmp/teleddns.tar.gz
rm -f /tmp/teleddns.tar.gz

sudo mkdir -p /etc/teleddns/
sudo bash -c "cat << EOF > /etc/teleddns/teleddns.yaml
---
debug: False

ddns_url: \"$DDNSURL\"
hostname: \"$DDNSNAME\"
enable_ipv6: True
enable_ipv4: False
interfaces:
- '*'
#hooks:
#- nft_sets_outfile: \"/etc/nftables.d/00-localnets.rules\"
#  shell: \"nft -f /etc/nftables.conf\"
EOF"

sudo bash -c "cat << EOF > /etc/systemd/system/teleddns.service
[Unit]
Description=teleddns systemd service
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/teleddns
Restart=always

[Install]
WantedBy=multi-user.target

EOF"

sudo systemctl daemon-reload
sudo systemctl enable teleddns.service
sudo systemctl restart teleddns.service

echo "Succesfully deployed teleddns with DDNS domain name: $DDNSNAME"
