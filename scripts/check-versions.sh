#!/bin/bash
# Check version alignment across Cargo.toml, teleddns.spec, and debian/changelog
# Exit with error if versions don't match

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

# Extract versions
CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
SPEC_VERSION=$(grep '^Version:' teleddns.spec | awk '{print $2}')
DEB_VERSION=$(head -1 debian/changelog | grep -oP '\(\K[^-)]+')

echo "Version check:"
echo "  Cargo.toml:       $CARGO_VERSION"
echo "  teleddns.spec:    $SPEC_VERSION"
echo "  debian/changelog: $DEB_VERSION"

ERRORS=0

if [[ "$CARGO_VERSION" != "$SPEC_VERSION" ]]; then
    echo "ERROR: Cargo.toml ($CARGO_VERSION) != teleddns.spec ($SPEC_VERSION)"
    ERRORS=1
fi

if [[ "$CARGO_VERSION" != "$DEB_VERSION" ]]; then
    echo "ERROR: Cargo.toml ($CARGO_VERSION) != debian/changelog ($DEB_VERSION)"
    ERRORS=1
fi

if [[ $ERRORS -eq 0 ]]; then
    echo "All versions match: $CARGO_VERSION"
    exit 0
else
    echo "Version mismatch detected!"
    exit 1
fi
