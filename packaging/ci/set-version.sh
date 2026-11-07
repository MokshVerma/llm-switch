#!/usr/bin/env bash
# Sets the package version in Cargo.toml for this build only. CI runs it on a throw-away checkout, so the
# change is never committed: the git tag is the source of truth for released versions.
#
#   packaging/ci/set-version.sh 0.3.0
set -euo pipefail

version="${1:?usage: set-version.sh VERSION}"

# Replace only the first `version = ` line, which is the one in [package].
awk -v v="$version" '!done && /^version[ \t]*=/ { print "version = \"" v "\""; done = 1; next } { print }' Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml

grep -m1 '^version' Cargo.toml
