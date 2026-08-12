#!/usr/bin/env bash
#
# Build the exact documentation tree deployed to GitHub Pages.
#
# Layout:
#   /                 mdBook guide
#   /api/             Rust workspace API documentation
#   /api/java/controller-spaces/  Spaces Java SDK documentation
#   /api/java/controller-core/    profile-neutral Core Java SDK documentation
#   /controller/      mdBook Controller integration guide

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SITE_DIR="$ROOT/target/site"

mdbook build
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
./gradlew --no-daemon \
  :coordination:sdk:controller-core:javadoc \
  :implementations:spaces:sdk:controller-spaces:javadoc

rm -rf -- "$SITE_DIR"
mkdir -p \
  "$SITE_DIR/api/java/controller-core" \
  "$SITE_DIR/api/java/controller-spaces"
cp -R target/book/. "$SITE_DIR/"
cp -R target/doc/. "$SITE_DIR/api/"
cp -R \
  implementations/spaces/sdk/java/build/docs/javadoc/. \
  "$SITE_DIR/api/java/controller-spaces/"
cp -R \
  control/coordination/sdk/java/build/docs/javadoc/. \
  "$SITE_DIR/api/java/controller-core/"

# A Cargo workspace does not generate a root index. Keep /api/ useful and make
# the primary application-facing crate the stable entry point.
if [ ! -f "$SITE_DIR/api/index.html" ]; then
  cp docs/site/rustdoc-index.html "$SITE_DIR/api/index.html"
fi

python3 ci/check-doc-links.py "$SITE_DIR" "/mumble-server-runtime/"

test -f "$SITE_DIR/index.html"
test -f "$SITE_DIR/api/mumble_server_runtime_shard/index.html"
test -f "$SITE_DIR/api/java/controller-spaces/index.html"
test -f "$SITE_DIR/api/java/controller-core/index.html"
test -f "$SITE_DIR/controller/getting-started.html"
cmp target/book/controller/index.html "$SITE_DIR/controller/index.html"

echo "Documentation site built at $SITE_DIR"
