#!/usr/bin/env bash
# Writes Markdown release notes for the commits since the previous release, grouped by Conventional Commit type.
# GitHub's own generator is not used because it compares against the previous *tag*, and the rolling
# "latest" pre-release is a tag too.
#
#   release-notes.sh PREVIOUS_TAG THIS_TAG [OWNER/REPO]     (PREVIOUS_TAG may be empty for the first release)
#
# Run it from the repository root with full history.
set -euo pipefail

prev="${1:-}"
tag="${2:?usage: release-notes.sh PREVIOUS_TAG THIS_TAG [OWNER/REPO]}"
repo="${3:-}"

if [[ -n "$prev" ]]; then range="$prev..HEAD"; else range="HEAD"; fi

breaking=""
feats=""
fixes=""
others=""

while IFS=$'\t' read -r hash subject; do
  [[ -z "$hash" ]] && continue
  [[ "$subject" == Merge\ * ]] && continue

  lower="$(printf '%s' "$subject" | tr '[:upper:]' '[:lower:]')"
  type=""; scope=""; bang=""; text="$subject"
  if [[ "$lower" =~ ^([a-z]+)(\(([^\)]*)\))?(!)?:[[:space:]]* ]]; then
    type="${BASH_REMATCH[1]}"
    scope="${BASH_REMATCH[3]}"
    bang="${BASH_REMATCH[4]}"
    text="${subject:${#BASH_REMATCH[0]}}"
  fi

  line="- "
  [[ -n "$scope" ]] && line+="**$scope:** "
  line+="$text (\`$hash\`)"$'\n'

  body="$(git log -1 --format=%b "$hash")"
  if [[ -n "$bang" ]] || grep -qE '^BREAKING[ -]CHANGE:' <<<"$body"; then
    breaking+="$line"
    continue # listed once, under "Breaking changes"
  fi

  case "$type" in
    feat)      feats+="$line" ;;
    fix | perf) fixes+="$line" ;;
    *)         others+="$line" ;;
  esac
done < <(git log "$range" --format='%h%x09%s')

section() { # title, content
  [[ -n "$2" ]] && printf '### %s\n\n%s\n' "$1" "$2"
  return 0
}

section "Breaking changes" "$breaking"
section "Features" "$feats"
section "Fixes" "$fixes"
section "Other changes" "$others"

if [[ -n "$repo" && -n "$prev" ]]; then
  printf '**Full changelog**: https://github.com/%s/compare/%s...%s\n' "$repo" "$prev" "$tag"
elif [[ -z "$prev" ]]; then
  printf 'First release.\n'
fi
