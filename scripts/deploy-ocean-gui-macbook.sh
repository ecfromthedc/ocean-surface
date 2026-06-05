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
bundle_output="$("$repo_root/scripts/bundle-ocean-gui-macos-app.sh" "${profile_arg[@]}")"
printf '%s\n' "$bundle_output"
app_dir="$(printf '%s\n' "$bundle_output" | awk '/Ocean GUI\.app$/ { path = $0 } END { print path }')"
remote_app_dir='Applications/Ocean GUI.app'

if [[ -z "$app_dir" || "$app_dir" == "/" || "$app_dir" != *"/Ocean GUI.app" || ! -d "$app_dir/Contents/MacOS" ]]; then
  printf 'Refusing to deploy invalid app bundle path: %s\n' "${app_dir:-<empty>}" >&2
  exit 65
fi

remote_home="$(ssh "$remote" 'printf "%s\n" "$HOME"')"
remote_app_abs="$remote_home/$remote_app_dir"
remote_stage_abs="$remote_home/Applications/OceanGUI.deploying"

if [[ ! -e /tmp/ocean-surface ]]; then
  ln -s "$repo_root" /tmp/ocean-surface
fi

ssh "$remote" 'set -e; pgrep -x OceanGUI >/dev/null && pkill -x OceanGUI || true; mkdir -p "$HOME/Applications" /tmp/ocean-surface; rm -rf "$HOME/Applications/Papyrus.app" "$HOME/Applications/Ocean GUI.app" "$HOME/Applications/OceanGUI.app" "$HOME/Applications/OceanGUI.deploying" "$HOME/Applications/OceanGui.app" "$HOME/Applications/ocean-gui.app"'

rsync -a --delete "$app_dir/" "$remote:$remote_stage_abs/"
ssh "$remote" 'set -e; rm -rf "$HOME/Applications/Ocean GUI.app"; mv "$HOME/Applications/OceanGUI.deploying" "$HOME/Applications/Ocean GUI.app"'

ssh "$remote" 'set -e; codesign --verify --deep --strict "$HOME/Applications/Ocean GUI.app"'

if ((launch)); then
  ssh "$remote" "set -e; pgrep -x OceanGUI >/dev/null && pkill -x OceanGUI || true; open -n \"\$HOME/Applications/Ocean GUI.app\" --args --workspace \"$remote_workspace\""
fi

printf 'Deployed %s to %s:%s\n' "$app_dir" "$remote" "$remote_app_dir"
