#!/usr/bin/env bash
# Setup workspace context for building a3s standalone.
#
# a3s depends on one path crate from the a3s monorepo:
#   ../updater  → a3s-updater
#
# Before: ./ = Cli repo root
# After:  ./ = workspace root with crates/cli/, crates/updater/

set -euo pipefail

TMPDIR="$(mktemp -d)"
cp -a . "$TMPDIR/cli"

# Clean current directory (except .git)
find . -maxdepth 1 ! -name '.' ! -name '.git' -exec rm -rf {} +

mkdir -p crates
cp -a "$TMPDIR/cli/." crates/cli/
rm -rf "$TMPDIR"

# Fetch updater from the monorepo via sparse-checkout
git clone --depth=1 --filter=blob:none --sparse \
  https://github.com/A3S-Lab/a3s.git _monorepo
(cd _monorepo && git sparse-checkout set crates/updater)
cp -a _monorepo/crates/updater crates/updater
rm -rf _monorepo

cat > Cargo.toml << 'EOF'
[workspace]
resolver = "2"
members = [
    "crates/cli",
    "crates/updater",
]

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
strip = "symbols"
panic = "abort"
EOF

echo "Workspace ready. a3s at: crates/cli/"
