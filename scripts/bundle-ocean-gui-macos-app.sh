#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s [--release]\n' "$0"
}

profile="debug"
while (($# > 0)); do
  case "$1" in
    --release)
      profile="release"
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
target_root="${CARGO_TARGET_DIR:-"$repo_root/target"}"
app_dir="$target_root/macos/Ocean GUI.app"
binary="$target_root/$profile/ocean-gui"
canvas_web_dir="$repo_root/crates/ocean-gui/canvas-web"
canvas_web_dist="$canvas_web_dir/dist"
stale_app_dirs=(
  "$target_root/macos/Papyrus.app"
  "$target_root/macos/OceanGUI.app"
  "$target_root/macos/OceanGui.app"
  "$target_root/macos/ocean-gui.app"
)

cargo_args=(build --package ocean-gui --bin ocean-gui)
if [[ "$profile" == "release" ]]; then
  cargo_args+=(--release)
fi

cd "$repo_root"
cargo "${cargo_args[@]}"

if [[ -f "$canvas_web_dir/package.json" ]]; then
  if ! command -v npm >/dev/null 2>&1; then
    printf 'npm is required to build Ocean Surface canvas assets: %s\n' "$canvas_web_dir" >&2
    exit 69
  fi

  (
    cd "$canvas_web_dir"
    if [[ -f package-lock.json ]]; then
      npm ci
    else
      npm install
    fi
    npm run build
    node scripts/inline-dist.mjs
  )
fi

for stale_app_dir in "${stale_app_dirs[@]}"; do
  if [[ "$stale_app_dir" != "$app_dir" ]]; then
    rm -rf "$stale_app_dir"
  fi
done

rm -rf "$app_dir"
mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
install -m 755 "$binary" "$app_dir/Contents/MacOS/OceanGUI"
cp "$repo_root/crates/ocean-gui/packaging/macos/Info.plist" "$app_dir/Contents/Info.plist"
cp -R "$repo_root/crates/ocean-gui/assets" "$app_dir/Contents/Resources/assets"

if [[ -d "$canvas_web_dist" ]]; then
  rm -rf "$app_dir/Contents/Resources/canvas-web"
  mkdir -p "$app_dir/Contents/Resources/canvas-web"
  cp -R "$canvas_web_dist/." "$app_dir/Contents/Resources/canvas-web/"
  if grep -R -E '(src|href)="/assets/' "$app_dir/Contents/Resources/canvas-web" >/dev/null; then
    printf 'Canvas bundle contains root-absolute asset URLs; use Vite base: "./" before packaging.\n' >&2
    exit 65
  fi
fi

/usr/bin/plutil -lint "$app_dir/Contents/Info.plist" >/dev/null

if [[ -x /usr/bin/codesign ]]; then
  /usr/bin/codesign --force --deep --sign - "$app_dir" >/dev/null
  /usr/bin/codesign --verify --deep --strict --verbose=2 "$app_dir" >/dev/null
fi

printf '%s\n' "$app_dir"
printf 'open "%s" --args --workspace "%s"\n' "$app_dir" "$repo_root"
