#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PROFILE=${PERSEVAL_BUILD_PROFILE:-release}
OUTPUT=${PERSEVAL_APP_OUTPUT:-"$ROOT/target/macos/Perseval.app"}
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
SIGN_IDENTITY=${PERSEVAL_CODESIGN_IDENTITY:--}
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
BUILD_NUMBER=${PERSEVAL_BUILD_NUMBER:-1}

if [ -z "$VERSION" ]; then
  echo "could not read workspace version from Cargo.toml" >&2
  exit 2
fi

case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac

case "$PROFILE" in
  debug)
    BUILD_ARGS=""
    BINARY="$TARGET_DIR/debug/perseval"
    MCP_BINARY="$TARGET_DIR/debug/perseval-mcp"
    ;;
  release)
    BUILD_ARGS="--release"
    BINARY="$TARGET_DIR/release/perseval"
    MCP_BINARY="$TARGET_DIR/release/perseval-mcp"
    ;;
  *)
    echo "PERSEVAL_BUILD_PROFILE must be debug or release" >&2
    exit 2
    ;;
esac

cd "$ROOT"
# shellcheck disable=SC2086
cargo build -p perseval-app --bin perseval -p perseval-mcp --bin perseval-mcp $BUILD_ARGS

rm -rf "$OUTPUT"
mkdir -p "$OUTPUT/Contents/MacOS" "$OUTPUT/Contents/Resources"
cp "$ROOT/apps/perseval-app/macos/Info.plist" "$OUTPUT/Contents/Info.plist"
cp "$ROOT/apps/perseval-app/macos/Perseval.icns" "$OUTPUT/Contents/Resources/Perseval.icns"
cp "$BINARY" "$OUTPUT/Contents/MacOS/perseval"
cp "$MCP_BINARY" "$OUTPUT/Contents/Resources/perseval-mcp"
chmod 755 "$OUTPUT/Contents/MacOS/perseval"
chmod 755 "$OUTPUT/Contents/Resources/perseval-mcp"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$OUTPUT/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$OUTPUT/Contents/Info.plist"

if [ "$SIGN_IDENTITY" = "-" ]; then
  codesign --force --sign - "$OUTPUT/Contents/Resources/perseval-mcp"
  codesign --force --sign - "$OUTPUT"
else
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$OUTPUT/Contents/Resources/perseval-mcp"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$OUTPUT"
fi

plutil -lint "$OUTPUT/Contents/Info.plist" >/dev/null
codesign --verify --deep --strict "$OUTPUT"
codesign --verify --strict "$OUTPUT/Contents/Resources/perseval-mcp"
echo "$OUTPUT"
