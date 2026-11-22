#!/bin/bash
set -euo pipefail

original_args=("$@")
project=""
sdk=""
arch=""
configuration=""
deployment_target=""
symroot=""
targets=()

while (($# > 0)); do
  case "$1" in
    -project)
      project="${2:-}"
      shift 2
      ;;
    -sdk)
      sdk="${2:-}"
      shift 2
      ;;
    -arch)
      arch="${2:-}"
      shift 2
      ;;
    -configuration)
      configuration="${2:-}"
      shift 2
      ;;
    -target)
      targets+=("${2:-}")
      shift 2
      ;;
    MACOSX_DEPLOYMENT_TARGET=*)
      deployment_target="${1#*=}"
      shift
      ;;
    SYMROOT=*)
      symroot="${1#*=}"
      shift
      ;;
    *)
      shift
      ;;
  esac
done

if [[ "$(basename "$project")" != "pomace.xcodeproj" || "$sdk" != "macosx" ]]; then
  exec /usr/bin/xcodebuild "${original_args[@]}"
fi

if [[ -z "$arch" || -z "$configuration" || -z "$deployment_target" || -z "$symroot" || ${#targets[@]} -eq 0 ]]; then
  echo "incomplete cidre shim build arguments" >&2
  exit 2
fi

project_root="$(cd "$(dirname "$project")" && pwd)"
sdk_path="$(/usr/bin/xcrun --sdk "$sdk" --show-sdk-path)"
clang="$(/usr/bin/xcrun --sdk "$sdk" --find clang)"
output_dir="$symroot/$configuration"
mkdir -p "$output_dir"

for target in "${targets[@]}"; do
  source_file="$project_root/$target/$target.m"
  object_file="$output_dir/$target.o"
  library_file="$output_dir/lib$target.a"
  if [[ ! -f "$source_file" ]]; then
    echo "missing cidre shim source: $source_file" >&2
    exit 2
  fi
  "$clang" \
    -arch "$arch" \
    -isysroot "$sdk_path" \
    "-mmacosx-version-min=$deployment_target" \
    -fmodules \
    -fobjc-arc \
    -Os \
    -c "$source_file" \
    -o "$object_file"
  /usr/bin/libtool -static -o "$library_file" "$object_file"
done
