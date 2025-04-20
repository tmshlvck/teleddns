#!/bin/bash

SRC_GITHUB_API="https://api.github.com/repos/tmshlvck/teleddns/releases/latest"
SRC_PREFIX="https://github.com/tmshlvck/teleddns/releases/download"

if (( $# != 2 )); then
    echo -e "Usage:\ncurl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>"
    echo -e "URL: DDNS API server URL\ndomainname: domain name (FQDN) to set"
    exit -1
fi

DDNSURL="$1"
DDNSNAME="$2"

TELEDDNS_VERSION=`curl -s "$SRC_GITHUB_API" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'`
if ! echo "$TELEDDNS_VERSION" | grep -E "v[0-9\.]+"; then
    echo "Failed to get latest version: $TELEDDNS_VERSION"
    exit -1
fi

if [ `uname -m` == "x86_64" ]; then
    SRC="$SRC_PREFIX/$TELEDDNS_VERSION/teleddns-x86_64-unknown-linux-gnu.tar.gz"
elif [ `uname -m` == "aarch64" ]; then
    SRC="$SRC_PREFIX/$TELEDDNS_VERSION/teleddns-aarch64-unknown-linux-gnu.tar.gz"
elif [ `uname -m` == "armv7l" ]; then
    SRC="$SRC_PREFIX/$TELEDDNS_VERSION/teleddns-armv7-unknown-linux-gnueabihf.tar.gz"
elif [ `uname -m` == "riscv64" ]; then
    SRC="$SRC_PREFIX/$TELEDDNS_VERSION/teleddns-riscv64gc-unknown-linux-gnu.tar.gz"
else
    echo "Error: Unsupported architecture `uname -m`."
    exit -1
fi

echo "Downloading $SRC ..."
curl -o /tmp/teleddns.tar.gz -L $SRC
if ! [ -f "/tmp/teleddns.tar.gz" ]; then
    echo "Failed to download source archive. Halting..."
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
