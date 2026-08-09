#!/bin/sh
#
# Build a .deb inside a container of the Debian/Ubuntu suite we are targeting.
#
# Cross-compiling with that distribution's own toolchain means the binary is
# linked against exactly that distribution's glibc, which is what makes the
# package correct for the suite it is published to. A binary linked against a
# newer glibc will not start on an older one, which is why a single .deb cannot
# serve every suite.
#
# Runs on amd64 at native speed -- no emulation, no target-arch VMs.
#
# Usage: build-deb.sh <deb-arch> <deb-version>

set -eu

deb_arch="$1"
deb_version="$2"

case "$deb_arch" in
    amd64)   rust_target=x86_64-unknown-linux-gnu ;      gnu_prefix=x86_64-linux-gnu ;;
    arm64)   rust_target=aarch64-unknown-linux-gnu ;     gnu_prefix=aarch64-linux-gnu ;;
    armhf)   rust_target=armv7-unknown-linux-gnueabihf ; gnu_prefix=arm-linux-gnueabihf ;;
    riscv64) rust_target=riscv64gc-unknown-linux-gnu ;   gnu_prefix=riscv64-linux-gnu ;;
    *) echo "unsupported Debian architecture: $deb_arch" >&2 ; exit 1 ;;
esac

export DEBIAN_FRONTEND=noninteractive

# build-essential covers the native case; the cross package pulls in the
# target-arch gcc and the matching multiarch sysroot. gcc is needed because the
# rustls stack pulls in `ring`, which compiles C.
packages="ca-certificates curl build-essential pkg-config"
if [ "$deb_arch" != amd64 ]; then
    packages="$packages crossbuild-essential-$deb_arch"
fi

apt-get update -qq
# shellcheck disable=SC2086
apt-get install -y --no-install-recommends $packages

# Use rustup rather than the distribution's rustc: older LTS releases ship a
# rustc below the crate's rust-version.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable --target "$rust_target"
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

cargo install cargo-deb --locked

# cargo wants the linker per target; the `cc` crate (for `ring`) wants CC.
target_uscore="$(echo "$rust_target" | tr '-' '_')"
target_upper="$(echo "$target_uscore" | tr 'a-z' 'A-Z')"
export "CARGO_TARGET_${target_upper}_LINKER=${gnu_prefix}-gcc"
export "CC_${target_uscore}=${gnu_prefix}-gcc"

cargo build --release --locked --target "$rust_target"
cargo deb --no-build --target "$rust_target" --deb-version "$deb_version"

mkdir -p dist
cp "target/$rust_target/debian/"*.deb dist/
