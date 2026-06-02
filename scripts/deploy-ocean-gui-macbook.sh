#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s [--release] [--no-launch] [--remote user@host] [--workspace path]\n' "$0"
}

profile_arg=()
launch=1
remote="smathdaddy-macbook@100.69.141.9"
remote_workspace="/tmp/ocean-surface"

while (($# > 0)); do
  case "$1" in
    --release)
      profile_arg=(--release)
      ;;
    --no-launch)
      launch=0
      ;;
    --remote)
      if (($# < 2)); then
        printf 'Missing value for --remote\n' >&2
        exit 64
      fi
      remote="$2"
      shift
      ;;
    --workspace)
      if (($# < 2)); then
        printf 'Missing value for --workspace\n' >&2
        exit 64
      fi
      remote_workspace="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 64
      ;;
  esac
  shift
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_dir="$("$repo_root/scripts/bundle-ocean-gui-macos-app.sh" "${profile_arg[@]}" | sed -n '1p')"
remote_app_dir='Applications/Ocean GUI.app'
remote_app_rsync_dir='Applications/Ocean\ GUI.app'

if [[ ! -e /tmp/ocean-surface ]]; then
  ln -s "$repo_root" /tmp/ocean-surface
fi

ssh "$remote" 'set -e; mkdir -p "$HOME/Applications" /tmp/ocean-surface; rm -rf "$HOME/Applications/Papyrus.app" "$HOME/Applications/OceanGUI.app" "$HOME/Applications/OceanGui.app" "$HOME/Applications/ocean-gui.app"'

rsync -a --delete "$app_dir/" "$remote:$remote_app_rsync_dir/"

ssh "$remote" 'set -e; codesign --verify --deep --strict "$HOME/Applications/Ocean GUI.app"'

if ((launch)); then
  ssh "$remote" "set -e; pgrep -x OceanGUI >/dev/null && pkill -x OceanGUI || true; open -n \"\$HOME/Applications/Ocean GUI.app\" --args --workspace \"$remote_workspace\""
fi

printf 'Deployed %s to %s:%s\n' "$app_dir" "$remote" "$remote_app_dir"
