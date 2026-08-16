#!/usr/bin/env bash
# package-macos-arm64.sh — build the one supported kyb release asset.
#
# ARTIFACT CONTRACT
#   The release has exactly one platform: aarch64-apple-darwin (Apple Silicon).
#   Intel macOS is explicitly unsupported and this script refuses to run on it.
#
#   asset name  kyb-${TAG}-aarch64-apple-darwin.tar.gz
#   layout      one top-level directory, kyb-${TAG}-aarch64-apple-darwin/, holding
#                 bin/kyb          the shell CLI, checked in at skills/kyb/bin/kyb
#                 bin/kyb-server   the release binary from target/release/kyb-server
#                 skills/          the complete skills tree, including skills/install.sh
#                 LICENSE
#                 README.md
#                 MANIFEST.txt     name/version/tag/target/archive, one key=value per line
#
#   Both packaging/homebrew/kyb.rb.in and .github/workflows/update-homebrew.yml
#   consume this layout. Moving or renaming a member here means changing the
#   formula template in the same commit. The archive is verified against the
#   contract below before the script exits, so a silent layout drift fails the
#   tag build instead of shipping a broken bottle-less formula.
#
# DETERMINISM
#   Re-running on the same inputs produces a byte-identical archive: members are
#   emitted in a fixed sorted order, mtimes are pinned, uid/gid are zeroed, the
#   format is ustar, macOS xattrs/AppleDouble forks are stripped, and gzip runs
#   with -n so no build timestamp is stored.
set -euo pipefail

die() { echo "package-macos-arm64: $*" >&2; exit 1; }

usage() {
  cat >&2 <<'EOF'
usage: scripts/package-macos-arm64.sh <tag> [output-dir]

  <tag>         release tag, for example v0.1.4
  [output-dir]  directory the .tar.gz is written to (default: dist)

environment:
  KYB_PACKAGE_MTIME  touch(1) timestamp pinned onto every archive member
                     (default 202001010000.00)
EOF
}

TAG="${1:-}"
OUT_DIR="${2:-dist}"

[ -n "$TAG" ] || { usage; exit 2; }
if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  usage
  die "unsupported release tag: $TAG (expected vMAJOR.MINOR.PATCH)"
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="aarch64-apple-darwin"
PKG="kyb-${TAG}-${TARGET}"
VERSION="${TAG#v}"
MTIME="${KYB_PACKAGE_MTIME:-202001010000.00}"

# The asset is a native Apple Silicon build: refuse to produce a mislabelled
# archive from an Intel or non-macOS host.
[ "$(uname -s)" = "Darwin" ] || die "macOS is required, this host is $(uname -s)"
[ "$(uname -m)" = "arm64" ] || die "Apple Silicon is required, this host is $(uname -m)"

SERVER_BIN="$REPO_ROOT/target/release/kyb-server"
CLI_BIN="$REPO_ROOT/skills/kyb/bin/kyb"

for required_input in \
  "$SERVER_BIN" \
  "$CLI_BIN" \
  "$REPO_ROOT/skills/install.sh" \
  "$REPO_ROOT/skills/validate-frontmatter.py" \
  "$REPO_ROOT/skills/kyb/SKILL.md" \
  "$REPO_ROOT/LICENSE" \
  "$REPO_ROOT/README.md"; do
  [ -f "$required_input" ] || die "missing required input: $required_input"
done

if command -v lipo >/dev/null 2>&1; then
  lipo -archs "$SERVER_BIN" | tr ' ' '\n' | grep -qx arm64 \
    || die "$SERVER_BIN is not an arm64 binary"
fi

STAGE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/kyb-package.XXXXXX")"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE="$STAGE_ROOT/$PKG"

mkdir -p "$STAGE/bin"
install -m 755 "$SERVER_BIN" "$STAGE/bin/kyb-server"
install -m 755 "$CLI_BIN" "$STAGE/bin/kyb"
cp -R "$REPO_ROOT/skills" "$STAGE/skills"
install -m 644 "$REPO_ROOT/LICENSE" "$STAGE/LICENSE"
install -m 644 "$REPO_ROOT/README.md" "$STAGE/README.md"
chmod 755 "$STAGE/skills/install.sh" "$STAGE/skills/kyb/bin/kyb"

cat > "$STAGE/MANIFEST.txt" <<EOF
name=kyb
version=$VERSION
tag=$TAG
target=$TARGET
archive=${PKG}.tar.gz
EOF

# macOS drags extended attributes and resource forks along with cp; left in
# place they surface as ._ AppleDouble members and make the output vary run to run.
if command -v xattr >/dev/null 2>&1; then
  xattr -cr "$STAGE"
fi
find "$STAGE" -name '._*' -delete
find "$STAGE" -type d -exec chmod 755 {} +
find "$STAGE" -exec touch -h -t "$MTIME" {} +

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
ARCHIVE="$OUT_DIR/${PKG}.tar.gz"
rm -f "$ARCHIVE"

# A sorted, explicit member list plus --no-recursion pins the order inside the
# archive; tar's own directory walk order is filesystem-dependent.
FILELIST="$STAGE_ROOT/filelist.txt"
(cd "$STAGE_ROOT" && find "$PKG" -print) | LC_ALL=C sort > "$FILELIST"

(
  cd "$STAGE_ROOT"
  COPYFILE_DISABLE=1 tar \
    --format=ustar \
    --no-recursion \
    --numeric-owner \
    --uid 0 \
    --gid 0 \
    -cf - -T "$FILELIST"
) | gzip -9 -n > "$ARCHIVE"

# Contract check: the consumers below break silently on a layout change, so fail here.
MEMBERS="$(tar -tzf "$ARCHIVE")"
for required_member in \
  "$PKG/bin/kyb" \
  "$PKG/bin/kyb-server" \
  "$PKG/skills/install.sh" \
  "$PKG/skills/validate-frontmatter.py" \
  "$PKG/skills/kyb/SKILL.md" \
  "$PKG/skills/kyb/bin/kyb" \
  "$PKG/LICENSE" \
  "$PKG/README.md" \
  "$PKG/MANIFEST.txt"; do
  printf '%s\n' "$MEMBERS" | grep -Fxq "$required_member" \
    || die "archive is missing $required_member"
done

TOP_LEVEL="$(printf '%s\n' "$MEMBERS" | awk -F/ '{print $1}' | LC_ALL=C sort -u)"
[ "$TOP_LEVEL" = "$PKG" ] \
  || die "archive must hold exactly one top-level directory, found: $TOP_LEVEL"

SHA256="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
echo "archive: $ARCHIVE"
echo "sha256:  $SHA256"

# Let a GitHub Actions step consume the values without re-deriving them.
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "archive=$ARCHIVE"
    echo "asset_name=${PKG}.tar.gz"
    echo "sha256=$SHA256"
  } >> "$GITHUB_OUTPUT"
fi
