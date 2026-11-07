#!/usr/bin/env bash
# Works out the next version from Conventional Commit messages since the last v* tag.
#
# Run it from the repository root with full git history available (checkout with fetch-depth: 0).
# Prints key=value lines (append them to $GITHUB_OUTPUT):
#   release   true if a numbered release should be published, otherwise false
#   version   the version to build: the next release, or <last>-dev.<sha> when nothing is releasable
#   tag       v<version> when release=true, empty otherwise
#   bump      none | patch | minor | major | initial
#   previous  the last v* tag, or "none"
#
# Rules (the highest one found in any commit since the last tag wins):
#   type!: ...  or a "BREAKING CHANGE:" line in the body   -> major
#   feat                                                    -> minor
#   fix, perf                                               -> patch
#   anything else (docs, chore, ci, test, refactor, ...)    -> no release
# The first release has no tag to bump from, so it uses the version in Cargo.toml as it is.
set -euo pipefail

cargo_version="$(grep -m1 '^version' Cargo.toml | sed -E 's/^version *= *"([^"]+)".*/\1/')"
short="$(git rev-parse --short=7 HEAD)"
last_tag="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)"

if [[ -n "$last_tag" ]]; then range="$last_tag..HEAD"; else range="HEAD"; fi

level=0 # 0 none, 1 patch, 2 minor, 3 major
while IFS= read -r commit; do
  subject="$(git log -1 --format=%s "$commit")"
  body="$(git log -1 --format=%b "$commit")"
  lower="$(printf '%s' "$subject" | tr '[:upper:]' '[:lower:]')"

  type=""
  bang=""
  if [[ "$lower" =~ ^([a-z]+)(\([^\)]*\))?(!)?: ]]; then
    type="${BASH_REMATCH[1]}"
    bang="${BASH_REMATCH[3]}"
  fi

  this=0
  [[ "$type" == "fix" || "$type" == "perf" ]] && this=1
  [[ "$type" == "feat" ]] && this=2
  [[ -n "$bang" ]] && this=3
  if grep -qE '^BREAKING[ -]CHANGE:' <<<"$body"; then this=3; fi

  if (( this > level )); then level=$this; fi
done < <(git rev-list "$range")

names=(none patch minor major)

if [[ -z "$last_tag" ]]; then
  if (( level > 0 )); then
    release=true; version="$cargo_version"; bump=initial
  else
    release=false; version="${cargo_version}-dev.${short}"; bump=none
  fi
else
  base="${last_tag#v}"
  core="${base%%[-+]*}"
  IFS=. read -r major minor patch <<<"$core"
  case "$level" in
    3) major=$((major + 1)); minor=0; patch=0 ;;
    2) minor=$((minor + 1)); patch=0 ;;
    1) patch=$((patch + 1)) ;;
  esac
  if (( level > 0 )); then
    release=true; version="$major.$minor.$patch"; bump="${names[$level]}"
  else
    release=false; version="${core}-dev.${short}"; bump=none
  fi
fi

echo "release=$release"
echo "version=$version"
if [[ "$release" == true ]]; then echo "tag=v$version"; else echo "tag="; fi
echo "bump=$bump"
echo "previous=${last_tag:-none}"
