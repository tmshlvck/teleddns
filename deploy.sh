#!/bin/bash
#
# Install and configure TeleDDNS:
#
#   curl -fsSL https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <DDNS URL> <FQDN>
#
# Tries, in order: the APT repository (Debian/Ubuntu suites we publish), COPR
# (Fedora), the static binary from the latest GitHub release, cargo. A method
# that fails is rolled back before the next one is tried. Runs as root or as a
# user with sudo, and is safe to re-run: a second run gives the same result.

set -euo pipefail

APT_REPO=https://apt.telephant.eu/teleddns/
# The Codenames in teleinfra ansible/slon-reprepro.yml.
APT_SUITES="trixie noble resolute"
# The paths Ansible's deb822_repository (name: teleddns) uses, so that this script
# and the Ansible role manage the same files instead of duplicating the repo.
APT_KEY=/etc/apt/keyrings/teleddns.asc
APT_SOURCES=/etc/apt/sources.list.d/teleddns.sources
COPR_REPO=tmshlvck/teleddns
COPR_REPO_FILE=/etc/yum.repos.d/_copr:copr.fedorainfracloud.org:tmshlvck:teleddns.repo
RELEASE_URL=https://github.com/tmshlvck/teleddns/releases/latest/download
LOCAL_BIN=/usr/local/bin/teleddns
LOCAL_UNIT=/etc/systemd/system/teleddns.service
CONFIG=/etc/teleddns/teleddns.yaml

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*" >&2; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

if (( $# != 2 )); then
    echo "Usage: curl -fsSL https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <DDNS URL> <FQDN>" >&2
    echo "  DDNS URL: the DDNS update URL, with credentials" >&2
    echo "  FQDN:     the domain name to keep updated" >&2
    exit 1
fi
DDNSURL="$1"
DDNSNAME="$2"

if [ "$(id -u)" -ne 0 ] && ! command -v sudo > /dev/null; then
    error "Run as root, or install sudo"
    exit 1
fi
as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

# Run an install method with errexit in force and leave its exit code in $STATUS.
# `if method` or `method || ...` would switch errexit off inside the method and
# silently carry on past a failed step. The subshell also scopes the method's
# variables and EXIT trap (its rollback) to that one attempt.
attempt() {
    set +e
    ( set -e; "$@" )
    STATUS=$?
    set -e
}

# The installed program must actually run on this machine (CPU, libc).
check_runs() {
    "$1" --version > /dev/null || { error "$1 does not run on this machine"; return 1; }
}

# --- APT --------------------------------------------------------------------

apt_suite() {
    local codename="${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}"
    case " $APT_SUITES " in
        *" $codename "*) echo "$codename" ;;
    esac
}

apt_rollback() {
    warn "Rolling back the APT installation"
    [ "$NEW_PKG" = 0 ] || as_root apt-get remove -y -qq teleddns || true
    [ "$NEW_SOURCES" = 0 ] || as_root rm -f "$APT_SOURCES"
    [ "$NEW_KEY" = 0 ] || as_root rm -f "$APT_KEY"
    as_root rm -f "$APT_KEY.tmp"
    as_root apt-get update -qq || true
}

install_apt() {
    local suite="$1"
    info "Installing from the APT repository, suite $suite"

    NEW_PKG=0 NEW_SOURCES=0 NEW_KEY=0
    dpkg -s teleddns > /dev/null 2>&1 || NEW_PKG=1
    [ -e "$APT_SOURCES" ] || NEW_SOURCES=1
    [ -e "$APT_KEY" ] || NEW_KEY=1
    trap apt_rollback EXIT

    # Replaces the pre-deb822 setup of earlier versions of this script.
    as_root rm -f /etc/apt/sources.list.d/teleddns.list /usr/share/keyrings/teleddns.gpg

    as_root install -d -m 755 /etc/apt/keyrings
    curl -fsSL "${APT_REPO}pubkey.gpg" | as_root tee "$APT_KEY.tmp" > /dev/null
    as_root chmod 644 "$APT_KEY.tmp"
    as_root mv "$APT_KEY.tmp" "$APT_KEY"

    as_root tee "$APT_SOURCES" > /dev/null << EOF
Types: deb
URIs: $APT_REPO
Suites: $suite
Components: main
Signed-By: $APT_KEY
EOF

    as_root apt-get update -qq
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq teleddns
    check_runs /usr/bin/teleddns
    trap - EXIT
}

# --- COPR -------------------------------------------------------------------

copr_rollback() {
    warn "Rolling back the COPR installation"
    [ "$NEW_PKG" = 0 ] || as_root dnf -y -q remove teleddns || true
    [ "$NEW_REPO" = 0 ] || as_root dnf -y -q copr remove "$COPR_REPO" || true
}

install_copr() {
    info "Installing from COPR ($COPR_REPO)"

    NEW_PKG=0 NEW_REPO=0
    rpm -q teleddns > /dev/null 2>&1 || NEW_PKG=1
    [ -e "$COPR_REPO_FILE" ] || NEW_REPO=1
    trap copr_rollback EXIT

    if ! dnf copr --help > /dev/null 2>&1; then
        info "Installing the dnf copr plugin"
        as_root dnf -y -q install 'dnf-command(copr)'
    fi
    as_root dnf -y -q copr enable "$COPR_REPO"
    as_root dnf -y -q install teleddns
    check_runs /usr/bin/teleddns
    trap - EXIT
}

# --- Static binary and cargo ------------------------------------------------

# The GitHub release ships statically linked musl binaries, which run on any
# distribution regardless of its libc.
binary_target() {
    case "$(uname -m)" in
        x86_64) echo x86_64-unknown-linux-musl ;;
        aarch64) echo aarch64-unknown-linux-musl ;;
        armv7l) echo armv7-unknown-linux-musleabihf ;;
        # ARMv6/ARMv5, e.g. the original Raspberry Pi 1 / Zero: no ARMv7.
        armv6l | armv5*) echo arm-unknown-linux-musleabi ;;
        riscv64) echo riscv64gc-unknown-linux-musl ;;
    esac
}

install_binary() {
    local target tarball
    target="$(binary_target)"
    if [ -z "$target" ]; then
        warn "No release binary for $(uname -m)"
        return 1
    fi
    tarball="teleddns-$target.tar.gz"
    info "Installing the static binary $tarball from the latest GitHub release"

    # Not local: the EXIT trap runs after the function has returned.
    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT
    curl -fsSL -o "$TMP/$tarball" "$RELEASE_URL/$tarball"
    if curl -fsSL -o "$TMP/SHA256SUMS" "$RELEASE_URL/SHA256SUMS"; then
        (cd "$TMP" && grep " $tarball\$" SHA256SUMS | sha256sum -c --quiet -)
    else
        warn "The release has no SHA256SUMS; not verifying the download"
    fi
    tar -xzf "$TMP/$tarball" -C "$TMP" teleddns
    check_runs "$TMP/teleddns"

    as_root install -D -m 755 "$TMP/teleddns" "$LOCAL_BIN"
    install_local_unit
}

install_cargo() {
    info "Building from crates.io with cargo"
    if ! command -v cargo > /dev/null; then
        error "cargo is not installed. Install Rust first:"
        error "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        return 1
    fi
    # TLS is rustls, so no OpenSSL headers are needed. A C compiler is, because
    # the rustls stack pulls in the `ring` crate.
    if ! command -v cc > /dev/null; then
        if command -v apt-get > /dev/null; then
            as_root apt-get install -y -qq gcc
        elif command -v dnf > /dev/null; then
            as_root dnf -y -q install gcc
        fi
    fi
    cargo install --locked teleddns
    local bin="${CARGO_HOME:-$HOME/.cargo}/bin/teleddns"
    check_runs "$bin"
    as_root install -D -m 755 "$bin" "$LOCAL_BIN"
    install_local_unit
}

# The unit for binary and cargo installs; packages ship their own.
install_local_unit() {
    info "Installing $LOCAL_UNIT"
    as_root tee "$LOCAL_UNIT" > /dev/null << EOF
[Unit]
Description=TeleDDNS - Advanced DDNS Client
After=network-online.target
Wants=network-online.target
# Give up rather than restart forever: each start publishes the current address.
StartLimitIntervalSec=300
StartLimitBurst=5
ConditionPathExists=$CONFIG

[Service]
Type=simple
ExecStart=$LOCAL_BIN
Restart=on-failure
RestartSec=10
NoNewPrivileges=yes
ProtectHome=yes
PrivateTmp=yes
KillSignal=SIGTERM
TimeoutStopSec=35

[Install]
WantedBy=multi-user.target
EOF
    as_root systemctl daemon-reload
}

# A unit in /etc/systemd/system overrides the packaged one, so an earlier binary
# or cargo install would keep running instead of the package.
remove_local_install() {
    if [ -f "$LOCAL_UNIT" ] && grep -q "^ExecStart=$LOCAL_BIN\$" "$LOCAL_UNIT"; then
        info "Removing the earlier binary install ($LOCAL_BIN, $LOCAL_UNIT)"
        as_root rm -f "$LOCAL_UNIT" "$LOCAL_BIN"
        as_root systemctl daemon-reload
    fi
}

# --- Configuration and service ----------------------------------------------

create_config() {
    if as_root test -f "$CONFIG"; then
        info "$CONFIG already exists, leaving it as it is"
        return
    fi
    info "Creating $CONFIG"
    # YAML single-quoted strings: the only escape is '' for a literal quote. The
    # heredoc is expanded once, by this shell, so nothing in the arguments is ever
    # evaluated as shell code.
    local url="${DDNSURL//\'/\'\'}"
    local name="${DDNSNAME//\'/\'\'}"
    # The file holds the DDNS password: create it 0600 before writing to it.
    as_root install -d -m 755 /etc/teleddns
    as_root install -m 600 /dev/null "$CONFIG"
    as_root tee "$CONFIG" > /dev/null << EOF
---
debug: false

ddns_url: '$url'
hostname: '$name'
enable_ipv6: true
enable_ipv4: false
interfaces:
- '*'
#hooks:
#- nft_sets_outfile: "/etc/nftables.d/00-localnets.rules"
#  shell: "nft -f /etc/nftables.conf"
EOF
}

start_service() {
    info "Enabling and (re)starting teleddns.service"
    as_root systemctl enable teleddns.service
    as_root systemctl restart teleddns.service
}

# --- Main -------------------------------------------------------------------

main() {
    local suite="" packaged=0
    ID="" VERSION_ID="" VERSION_CODENAME="" UBUNTU_CODENAME=""
    # shellcheck disable=SC1091
    [ -f /etc/os-release ] && . /etc/os-release
    info "Detected distribution: ${ID:-unknown} ${VERSION_CODENAME:-$VERSION_ID}"

    STATUS=1
    if command -v apt-get > /dev/null; then
        suite="$(apt_suite)"
        if [ -n "$suite" ]; then
            attempt install_apt "$suite"
        else
            info "No APT packages for '${UBUNTU_CODENAME:-${VERSION_CODENAME:-unknown}}' (published: $APT_SUITES)"
        fi
    elif [ "$ID" = fedora ]; then
        attempt install_copr
    fi
    if [ "$STATUS" = 0 ]; then
        packaged=1
    else
        attempt install_binary
        if [ "$STATUS" != 0 ]; then
            warn "Binary install failed, trying cargo"
            attempt install_cargo
        fi
        if [ "$STATUS" != 0 ]; then
            error "All installation methods failed"
            exit 1
        fi
    fi

    if [ "$packaged" = 1 ]; then
        remove_local_install
    fi
    create_config
    start_service

    info "Deployed teleddns for $DDNSNAME"
    echo "Check status with: systemctl status teleddns"
    echo "View logs with:    journalctl -u teleddns -f"
}

main
