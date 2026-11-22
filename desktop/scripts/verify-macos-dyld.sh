#!/bin/bash
# Fail the build if dyld would refuse to load any Mach-O file in the app.
#
# Usage: verify-macos-dyld.sh <path to .app>
#
# Why: after the Mac runner moved to macOS 27.2 and Xcode 27.2 (Oct 2026),
# some dylibs that cargo strips came out with a LINKEDIT string pool that
# the new dyld rejects ("mis-aligned LINKEDIT string pool"). It hit the
# build's own proc macros first (the 1.1.78 beta failed to compile), but
# the same thing in a shipped binary would mean an app that does not launch
# on new macOS. `dyld_info` runs dyld's own load-time validation, so this
# catches it before the app is uploaded or published.
set -euo pipefail

APP="${1:?usage: verify-macos-dyld.sh <path to .app>}"
[ -d "$APP" ] || { echo "::error::verify-macos-dyld: no app at $APP"; exit 1; }

checked=0
bad=0
while IFS= read -r -d '' f; do
  case "$(file -b "$f")" in
    *Mach-O*) ;;
    *) continue ;;
  esac
  checked=$((checked + 1))
  # dyld_info exits non-zero when image validation fails. Any non-zero status
  # fails the check; the known messages also fail it in case a validation
  # problem is printed without a non-zero status. The output is only used
  # to explain the failure.
  status=0
  out="$(dyld_info -platform "$f" 2>&1)" || status=$?
  if [ "$status" -ne 0 ] || printf '%s\n' "$out" | grep -i -E 'mis-aligned|malformed|truncated' >/dev/null; then
    reason="$(printf '%s\n' "$out" | grep -v -F "$f" | grep -v '^[[:space:]]*$' | head -3 | tr '\n' ' ' || true)"
    echo "::error::dyld would reject ${f#"$APP"/} (dyld_info exit ${status}): ${reason:-no message}"
    bad=$((bad + 1))
  fi
done < <(find "$APP" -type f -print0)

if [ "$checked" -eq 0 ]; then
  echo "::error::verify-macos-dyld: found no Mach-O files in $APP"
  exit 1
fi
if [ "$bad" -ne 0 ]; then
  echo "::error::verify-macos-dyld: $bad of $checked Mach-O files would fail to load"
  exit 1
fi
echo "verify-macos-dyld: all $checked Mach-O files in $(basename "$APP") pass dyld validation"
