#!/bin/bash
# Create a vendored source tarball for offline builds
# Usage: ./scripts/create-vendor-tarball.sh [output_dir]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Convert output dir to absolute path
if [[ -n "$1" ]]; then
    OUTPUT_DIR="$(cd "$1" && pwd)"
else
    OUTPUT_DIR="$PROJECT_ROOT"
fi

cd "$PROJECT_ROOT"

# Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
PACKAGE_NAME="teleddns"
# Note: Tarball follows Debian naming convention {package}-{version}.tar.gz
# All release tarballs include vendored dependencies for offline builds
TARBALL_NAME="${PACKAGE_NAME}-${VERSION}.tar.gz"

echo "Creating vendored tarball for $PACKAGE_NAME v$VERSION..."

# Create a temporary directory for the tarball contents
TEMP_DIR=$(mktemp -d)
DEST_DIR="$TEMP_DIR/${PACKAGE_NAME}-${VERSION}"

# Copy project files (excluding .git, target, and existing vendor)
echo "Copying project files..."
mkdir -p "$DEST_DIR"
rsync -a --exclude='.git' --exclude='target' --exclude='vendor' --exclude='*.tar.gz' . "$DEST_DIR/"

# Vendor dependencies
echo "Vendoring dependencies..."
cd "$DEST_DIR"
cargo vendor vendor > /dev/null

# Create .cargo/config.toml for offline builds
echo "Creating .cargo/config.toml..."
mkdir -p .cargo
cat > .cargo/config.toml << 'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

# Create the tarball
echo "Creating tarball..."
cd "$TEMP_DIR"
tar czf "$OUTPUT_DIR/$TARBALL_NAME" "${PACKAGE_NAME}-${VERSION}"

# Cleanup
rm -rf "$TEMP_DIR"

echo "Created: $OUTPUT_DIR/$TARBALL_NAME"
echo "Size: $(du -h "$OUTPUT_DIR/$TARBALL_NAME" | cut -f1)"
