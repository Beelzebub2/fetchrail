#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
native="$repo/src-tauri/native"
revision=96d5fb3de135b86d7222c53f2352ca92827a156b
case "$(uname -m)" in x86_64) triplet=x64-linux ;; aarch64|arm64) triplet=arm64-linux ;; *) echo 'Build on a supported native x86_64 or aarch64 Linux runner.' >&2; exit 1 ;; esac
for tool in git curl cmake ninja perl pkg-config; do command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }; done
mkdir -p "$native/vendor-cache"
header="$native/vendor-cache/json.hpp"
if [[ ! -f "$header" ]]; then curl --fail --location --retry 3 https://raw.githubusercontent.com/nlohmann/json/v3.12.0/single_include/nlohmann/json.hpp -o "$header"; fi
printf '%s  %s\n' 4ff9ee8c7ca94f5b730590f73b01be5aa66a7b82a8271a6391170e42043b66866d4f7a89c8dc2feca88df4e40fc9052a96454b6aa315fcc6704dbf20c532f19e "$header" | sha512sum --check --status
vcpkg=${VCPKG_ROOT:-"$native/vcpkg"}
if [[ ! -d "$vcpkg/.git" ]]; then
  git clone --filter=blob:none https://github.com/microsoft/vcpkg.git "$vcpkg"
  git -C "$vcpkg" checkout --detach "$revision"
fi
[[ $(git -C "$vcpkg" rev-parse HEAD) == "$revision" ]] || { echo "Use vcpkg revision $revision (refusing to change an existing checkout)." >&2; exit 1; }
"$vcpkg/bootstrap-vcpkg.sh" -disableMetrics
"$vcpkg/vcpkg" install "libtorrent[core]:$triplet" --overlay-ports="$native/ports" --overlay-triplets="$native/triplets" --x-install-root="$native/installed-secure" --x-buildtrees-root="$native/buildtrees-secure" --x-packages-root="$native/packages-secure"
