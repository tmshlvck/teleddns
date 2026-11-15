#!/bin/bash

set -e

SRC_GITHUB_API="https://api.github.com/repos/tmshlvck/teleddns/releases/latest"
SRC_PREFIX="https://github.com/tmshlvck/teleddns/releases/download"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

if (( $# != 2 )); then
    echo -e "Usage:\ncurl -s -L https://raw.githubusercontent.com/tmshlvck/teleddns/master/deploy.sh | bash -s <URL> <domainname>"
    echo -e "URL: DDNS API server URL\ndomainname: domain name (FQDN) to set"
    exit 1
fi

DDNSURL="$1"
DDNSNAME="$2"

# Detect distribution
detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        DISTRO_ID="$ID"
        DISTRO_ID_LIKE="$ID_LIKE"
    else
        DISTRO_ID="unknown"
        DISTRO_ID_LIKE=""
    fi
}

# Check if distro is Debian-based (Debian, Ubuntu, Pop!_OS, etc.)
is_debian_based() {
    case "$DISTRO_ID" in
        debian|ubuntu|pop|linuxmint|elementary|zorin|kali|raspbian)
            return 0
            ;;
    esac
    # Also check ID_LIKE for derivatives
    if [[ "$DISTRO_ID_LIKE" == *"debian"* ]] || [[ "$DISTRO_ID_LIKE" == *"ubuntu"* ]]; then
        return 0
    fi
    return 1
}

# Check if distro is Fedora-based
is_fedora_based() {
    case "$DISTRO_ID" in
        fedora)
            return 0
            ;;
    esac
    return 1
}

# Get architecture for binary download. The GitHub release ships statically
# linked musl binaries, which run on any distribution regardless of its glibc.
get_binary_target() {
    case "$(uname -m)" in
        x86_64)
            echo "x86_64-unknown-linux-musl"
            ;;
        aarch64)
            echo "aarch64-unknown-linux-musl"
            ;;
        armv7l)
            echo "armv7-unknown-linux-musleabihf"
            ;;
        armv6l | armv5*)
            # ARMv6/ARMv5, e.g. the original Raspberry Pi 1 / Zero: no
            # hardware-float ABI, so the armhf build above will not run.
            echo "arm-unknown-linux-musleabi"
            ;;
        riscv64)
            echo "riscv64gc-unknown-linux-musl"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Pick the APT suite matching this system. Packages are built separately per
# suite against that suite's glibc, so they are not interchangeable -- installing
# the wrong one gives a binary that will not start.
get_apt_suite() {
    # UBUNTU_CODENAME first: derivatives such as Mint or Pop!_OS carry their own
    # VERSION_CODENAME but keep the Ubuntu one alongside it.
    codename="${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}"
    case "$codename" in
        trixie|noble|resolute)
            echo "$codename"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Install via APT (Debian/Ubuntu/Pop!_OS)
install_apt() {
    info "Detected Debian-based distribution: $DISTRO_ID"

    SUITE=$(get_apt_suite)
    if [ -z "$SUITE" ]; then
        warn "No APT packages for this release (${UBUNTU_CODENAME:-${VERSION_CODENAME:-unknown}})"
        return 1
    fi

    info "Installing from APT repository (suite: $SUITE)..."

    # Install gnupg if not present
    if ! command -v gpg &> /dev/null; then
        sudo apt-get update
        sudo apt-get install -y gnupg
    fi

    # Add GPG key
    info "Adding repository GPG key..."
    curl -fsSL https://apt.telephant.eu/teleddns/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/teleddns.gpg

    # Add repository
    info "Adding APT repository..."
    echo "deb [signed-by=/usr/share/keyrings/teleddns.gpg] https://apt.telephant.eu/teleddns/ $SUITE main" | sudo tee /etc/apt/sources.list.d/teleddns.list > /dev/null

    # Install package
    info "Installing teleddns package..."
    sudo apt-get update
    sudo apt-get install -y teleddns

    return 0
}

# Install via COPR (Fedora)
install_copr() {
    info "Detected Fedora: $DISTRO_ID"
    info "Installing from COPR repository..."

    # Enable COPR repository
    info "Enabling COPR repository..."
    sudo dnf -y copr enable tmshlvck/teleddns

    # Install package
    info "Installing teleddns package..."
    sudo dnf -y install teleddns

    return 0
}

# Install binary from GitHub releases
install_binary() {
    info "Downloading binary from GitHub releases..."

    TELEDDNS_VERSION=$(curl -fsSL "$SRC_GITHUB_API" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
    if ! echo "$TELEDDNS_VERSION" | grep -qE "^v[0-9\.]+$"; then
        error "Failed to get latest version: $TELEDDNS_VERSION"
        return 1
    fi
    info "Latest version: $TELEDDNS_VERSION"

    TARGET=$(get_binary_target)
    if [ -z "$TARGET" ]; then
        warn "Unsupported architecture: $(uname -m)"
        return 1
    fi

    SRC="$SRC_PREFIX/$TELEDDNS_VERSION/teleddns-${TARGET}.tar.gz"
    info "Downloading $SRC ..."

    TMPDIR_DL=$(mktemp -d)
    if ! curl -f -o "$TMPDIR_DL/teleddns.tar.gz" -L "$SRC"; then
        warn "Failed to download binary for $TARGET"
        rm -rf "$TMPDIR_DL"
        return 1
    fi

    info "Installing binary to /usr/local/bin..."
    sudo mkdir -p /usr/local/bin
    sudo tar xf "$TMPDIR_DL/teleddns.tar.gz" -C /usr/local/bin
    rm -rf "$TMPDIR_DL"

    # Install systemd service for binary installs
    install_systemd_service "/usr/local/bin/teleddns"

    return 0
}

# Install via cargo from crates.io
install_cargo() {
    info "Building from crates.io using cargo..."

    # Check if cargo is available
    if ! command -v cargo &> /dev/null; then
        error "cargo is not installed. Please install Rust/Cargo first:"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        return 1
    fi

    # TLS is rustls, so no OpenSSL headers are needed. A C compiler is, because
    # the rustls stack pulls in the `ring` crate.
    if is_debian_based; then
        if ! dpkg -s gcc &> /dev/null 2>&1; then
            warn "Installing gcc..."
            sudo apt-get update
            sudo apt-get install -y gcc
        fi
    elif is_fedora_based; then
        if ! rpm -q gcc &> /dev/null 2>&1; then
            warn "Installing gcc..."
            sudo dnf -y install gcc
        fi
    fi

    info "Running cargo install teleddns..."
    cargo install teleddns

    # Determine cargo bin path
    CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/teleddns"
    if [ ! -f "$CARGO_BIN" ]; then
        error "Failed to find installed binary at $CARGO_BIN"
        return 1
    fi

    # Copy to system location
    info "Installing binary to /usr/local/bin..."
    sudo cp "$CARGO_BIN" /usr/local/bin/teleddns
    sudo chmod +x /usr/local/bin/teleddns

    # Install systemd service for cargo installs
    install_systemd_service "/usr/local/bin/teleddns"

    return 0
}

# Install systemd service (for binary/cargo installs)
install_systemd_service() {
    local EXEC_PATH="$1"
    info "Installing systemd service..."

    sudo bash -c "cat << EOF > /etc/systemd/system/teleddns.service
[Unit]
Description=TeleDDNS - Advanced DDNS Client
After=network-online.target
Wants=network-online.target
# Give up rather than restart forever: each start publishes the current address.
StartLimitIntervalSec=300
StartLimitBurst=5
ConditionPathExists=/etc/teleddns/teleddns.yaml

[Service]
Type=simple
ExecStart=$EXEC_PATH
Restart=on-failure
RestartSec=10
NoNewPrivileges=yes
ProtectHome=yes
PrivateTmp=yes
KillSignal=SIGTERM
TimeoutStopSec=35

[Install]
WantedBy=multi-user.target
EOF"

    sudo systemctl daemon-reload
}

# Create configuration file if not present
create_config() {
    if [ -f /etc/teleddns/teleddns.yaml ]; then
        warn "Configuration file /etc/teleddns/teleddns.yaml already exists, skipping..."
        return 0
    fi

    info "Creating configuration file..."
    # YAML single-quoted strings: the only escape is '' for a literal quote. The
    # heredoc is expanded once, by this shell, so nothing in the arguments is ever
    # evaluated as shell code.
    local url="${DDNSURL//\'/\'\'}"
    local name="${DDNSNAME//\'/\'\'}"
    # The file holds the DDNS password: create it 0600 before writing to it.
    sudo install -d -m 755 /etc/teleddns
    sudo install -m 600 /dev/null /etc/teleddns/teleddns.yaml
    sudo tee /etc/teleddns/teleddns.yaml > /dev/null << EOF
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

# Enable and start service
start_service() {
    info "Enabling and starting teleddns service..."
    sudo systemctl enable teleddns.service
    sudo systemctl restart teleddns.service
}

# Main installation logic
main() {
    detect_distro
    info "Detected distribution: $DISTRO_ID"

    INSTALL_SUCCESS=false

    # Try package manager first for known distributions
    if is_debian_based; then
        if install_apt; then
            INSTALL_SUCCESS=true
        else
            warn "APT installation failed, trying fallback methods..."
        fi
    elif is_fedora_based; then
        if install_copr; then
            INSTALL_SUCCESS=true
        else
            warn "COPR installation failed, trying fallback methods..."
        fi
    fi

    # Fallback: try binary download
    if [ "$INSTALL_SUCCESS" = false ]; then
        if install_binary; then
            INSTALL_SUCCESS=true
        else
            warn "Binary download failed, trying cargo build..."
        fi
    fi

    # Final fallback: cargo build
    if [ "$INSTALL_SUCCESS" = false ]; then
        if install_cargo; then
            INSTALL_SUCCESS=true
        else
            error "All installation methods failed!"
            exit 1
        fi
    fi

    # Create configuration (always, if not present)
    create_config

    # Start service
    start_service

    info "Successfully deployed teleddns with DDNS domain name: $DDNSNAME"
    echo ""
    echo "Check status with: sudo systemctl status teleddns"
    echo "View logs with:    sudo journalctl -u teleddns -f"
}

main
