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

# Get architecture for binary download
get_binary_target() {
    case "$(uname -m)" in
        x86_64)
            echo "x86_64-unknown-linux-gnu"
            ;;
        aarch64)
            echo "aarch64-unknown-linux-gnu"
            ;;
        armv7l)
            echo "armv7-unknown-linux-gnueabihf"
            ;;
        riscv64)
            echo "riscv64gc-unknown-linux-gnu"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Install via APT (Debian/Ubuntu/Pop!_OS)
install_apt() {
    info "Detected Debian-based distribution: $DISTRO_ID"
    info "Installing from APT repository..."

    # Install gnupg if not present
    if ! command -v gpg &> /dev/null; then
        sudo apt-get update
        sudo apt-get install -y gnupg
    fi

    # Add GPG key
    info "Adding repository GPG key..."
    curl -fsSL https://apt.telephant.eu/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/telephant.gpg

    # Add repository
    info "Adding APT repository..."
    echo "deb [signed-by=/usr/share/keyrings/telephant.gpg] https://apt.telephant.eu stable main" | sudo tee /etc/apt/sources.list.d/telephant.list > /dev/null

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

    TELEDDNS_VERSION=$(curl -s "$SRC_GITHUB_API" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
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

    if ! curl -f -o /tmp/teleddns.tar.gz -L "$SRC"; then
        warn "Failed to download binary for $TARGET"
        rm -f /tmp/teleddns.tar.gz
        return 1
    fi

    info "Installing binary to /usr/local/bin..."
    sudo mkdir -p /usr/local/bin
    sudo tar xf /tmp/teleddns.tar.gz -C /usr/local/bin
    rm -f /tmp/teleddns.tar.gz

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

    # Check for OpenSSL development headers
    if is_debian_based; then
        if ! dpkg -s libssl-dev &> /dev/null 2>&1; then
            warn "Installing libssl-dev..."
            sudo apt-get update
            sudo apt-get install -y libssl-dev pkg-config
        fi
    elif is_fedora_based; then
        if ! rpm -q openssl-devel &> /dev/null 2>&1; then
            warn "Installing openssl-devel..."
            sudo dnf -y install openssl-devel pkg-config
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

[Service]
Type=simple
ExecStart=$EXEC_PATH
Restart=on-failure
RestartSec=10
NoNewPrivileges=yes
ProtectHome=yes
PrivateTmp=yes

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
