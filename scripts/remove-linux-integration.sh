#!/usr/bin/env bash
set -euo pipefail
executable=${1:-fetchrail}
"$executable" --remove-integration
data=${XDG_DATA_HOME:-"$HOME/.local/share"}
[[ $data == /* ]] || data="$HOME/.local/share"
desktop="$data/applications/com.rrmtools.braid.desktop"
# Only the per-user desktop entry owned by this installer is removed; download data stays.
if [[ -f "$desktop" ]] && grep -q '^Name=Fetchrail$' "$desktop"; then rm -f -- "$desktop"; fi
echo 'Fetchrail browser and startup integration removed. Saved downloads and portable binaries were preserved.'
