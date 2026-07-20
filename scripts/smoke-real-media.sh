#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"

if [[ ! -x "${ENGINE}" ]]; then
  cargo build --manifest-path "${ROOT}/Cargo.toml" --release
fi

find_first_media() {
  for dir in "/Volumes/Movies" "/Volumes/TV Shows" "/Volumes/TVShows"; do
    [[ -d "${dir}" ]] || continue
    local found
    found="$(find "${dir}" -type f \( -iname '*.mp4' -o -iname '*.m4v' -o -iname '*.mkv' \) -print -quit)"
    if [[ -n "${found}" ]]; then
      printf '%s\n' "${found}"
      return 0
    fi
  done
}

MEDIA_FILE="${1:-$(find_first_media)}"
if [[ -z "${MEDIA_FILE}" ]]; then
  echo "No MP4/M4V/MKV file found under /Volumes/Movies or /Volumes/TV Shows." >&2
  exit 1
fi

echo "smoke media: ${MEDIA_FILE}" >&2
"${ENGINE}" probe "${MEDIA_FILE}" >/tmp/chroma-engine-probe.json
"${ENGINE}" plan "${MEDIA_FILE}" --target browser >/tmp/chroma-engine-plan.json
"${ENGINE}" manifest "${MEDIA_FILE}" --target-ms 4000 >/tmp/chroma-engine-manifest.json
"${ENGINE}" hls-plan "${MEDIA_FILE}" --segment-ms 4000 >/tmp/chroma-engine-hls-plan.json

echo "wrote:"
echo "  /tmp/chroma-engine-probe.json"
echo "  /tmp/chroma-engine-plan.json"
echo "  /tmp/chroma-engine-manifest.json"
echo "  /tmp/chroma-engine-hls-plan.json"
