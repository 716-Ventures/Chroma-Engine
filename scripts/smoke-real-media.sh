#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"

if [[ ! -x "${ENGINE}" ]]; then
  cargo build --manifest-path "${ROOT}/Cargo.toml" --release
fi

find_first_native_hls_media() {
  for dir in "/Volumes/Movies" "/Volumes/TV Shows" "/Volumes/TVShows"; do
    [[ -d "${dir}" ]] || continue
    while IFS= read -r file; do
      if "${ENGINE}" hls-plan "${file}" --segment-ms 4000 >/dev/null 2>&1; then
        printf '%s\n' "${file}"
        return 0
      fi
    done < <(find "${dir}" -type f \( -iname '*.mp4' -o -iname '*.m4v' -o -iname '*.mkv' \) -print)
  done
}

MEDIA_FILE="${1:-$(find_first_native_hls_media)}"
if [[ -z "${MEDIA_FILE}" ]]; then
  echo "No native-HLS-compatible MP4/M4V/MKV file found under the mounted media directories." >&2
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
