#!/usr/bin/env bash
# Builds "LLM Switch.app" (and a zip of it) from the Rust sources.
#
#   packaging/macos/bundle.sh                 build for this Mac's architecture
#   packaging/macos/bundle.sh --universal     Apple Silicon + Intel in one app (needs both Rust targets)
#   packaging/macos/bundle.sh --sign "Developer ID Application: Your Name (TEAMID)"
#
# Options:
#   --universal        build aarch64 and x86_64, merge them with lipo
#   --target TRIPLE    build for one specific target
#   --sign IDENTITY    codesign identity (default "-" = ad-hoc, fine for your own Mac)
#   --out DIR          output folder (default: dist)
#   --skip-build       do not run cargo; take the binaries from --bin-dir
#   --bin-dir DIR      folder holding `llm-switch` and `llm-switch-cli` (with --skip-build)
#
# Output: dist/LLM Switch.app and dist/LLM-Switch-<version>-macos.zip
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

APP_NAME="LLM Switch"
GUI_BIN="llm-switch"
CLI_BIN="llm-switch-cli"

UNIVERSAL=0
TARGET=""
IDENTITY="-"
OUT="dist"
SKIP_BUILD=0
BIN_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --universal)  UNIVERSAL=1 ;;
    --target)     TARGET="${2:?--target needs a value}"; shift ;;
    --sign)       IDENTITY="${2:?--sign needs a value}"; shift ;;
    --out)        OUT="${2:?--out needs a value}"; shift ;;
    --skip-build) SKIP_BUILD=1 ;;
    --bin-dir)    BIN_DIR="${2:?--bin-dir needs a value}"; shift ;;
    -h|--help)    sed -n '2,19p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

have() { command -v "$1" >/dev/null 2>&1; }

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/^version *= *"([^"]+)".*/\1/')"
[[ -n "$VERSION" ]] || { echo "could not read the version from Cargo.toml" >&2; exit 1; }

# ---- build -----------------------------------------------------------------------------------
if [[ $SKIP_BUILD -eq 1 ]]; then
  [[ -n "$BIN_DIR" ]] || { echo "--skip-build needs --bin-dir" >&2; exit 2; }
  GUI_PATH="$BIN_DIR/$GUI_BIN"
  CLI_PATH="$BIN_DIR/$CLI_BIN"
else
  have cargo || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }
  if [[ $UNIVERSAL -eq 1 ]]; then
    TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
  elif [[ -n "$TARGET" ]]; then
    TARGETS=("$TARGET")
  else
    TARGETS=("$(rustc -vV | sed -n 's/^host: //p')")
  fi
  for t in "${TARGETS[@]}"; do
    have rustup && rustup target add "$t" >/dev/null
    echo "==> cargo build --release --target $t"
    cargo build --release --target "$t"
  done
  if [[ ${#TARGETS[@]} -eq 1 ]]; then
    GUI_PATH="target/${TARGETS[0]}/release/$GUI_BIN"
    CLI_PATH="target/${TARGETS[0]}/release/$CLI_BIN"
  else
    have lipo || { echo "lipo not found (a universal build needs macOS)" >&2; exit 1; }
    mkdir -p target/universal
    for name in "$GUI_BIN" "$CLI_BIN"; do
      echo "==> lipo $name"
      lipo -create -output "target/universal/$name" "target/${TARGETS[0]}/release/$name" "target/${TARGETS[1]}/release/$name"
    done
    GUI_PATH="target/universal/$GUI_BIN"
    CLI_PATH="target/universal/$CLI_BIN"
  fi
fi

for f in "$GUI_PATH" "$CLI_PATH" assets/icon.icns packaging/macos/Info.plist.in; do
  [[ -f "$f" ]] || { echo "missing file: $f" >&2; exit 1; }
done

# ---- assemble the bundle -----------------------------------------------------------------------
APP="$OUT/$APP_NAME.app"
echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$GUI_PATH" "$APP/Contents/MacOS/$GUI_BIN"
cp "$CLI_PATH" "$APP/Contents/MacOS/$CLI_BIN"
chmod +x "$APP/Contents/MacOS/$GUI_BIN" "$APP/Contents/MacOS/$CLI_BIN"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
# macOS wants a plain x.y.z in the plist, so drop any pre-release suffix such as -dev.abc1234.
sed "s/@VERSION@/${VERSION%%[-+]*}/g" packaging/macos/Info.plist.in > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# ---- sign ----------------------------------------------------------------------------------------
if have codesign; then
  echo "==> codesign (identity: $IDENTITY)"
  if [[ "$IDENTITY" == "-" ]]; then
    codesign --force --deep --sign - "$APP"
  else
    # A real Developer ID needs the hardened runtime and a secure timestamp to be notarized.
    codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP"
  fi
  codesign --verify --deep --strict "$APP" && echo "    signature verifies"
else
  echo "==> codesign not found (not on macOS): the bundle is NOT signed" >&2
fi

# ---- zip -------------------------------------------------------------------------------------------
ZIP="$OUT/LLM-Switch-$VERSION-macos.zip"
rm -f "$ZIP"
if have ditto; then
  ditto -c -k --keepParent "$APP" "$ZIP"
elif have zip; then
  (cd "$OUT" && zip -qry "$(basename "$ZIP")" "$APP_NAME.app")
else
  echo "==> neither ditto nor zip found; skipping the archive" >&2
  ZIP=""
fi

echo
echo "Done."
echo "  app: $APP"
[[ -n "$ZIP" ]] && echo "  zip: $ZIP"
exit 0
