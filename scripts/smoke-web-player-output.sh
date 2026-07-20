#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"

if [[ ! -x "${ENGINE}" ]]; then
  cargo build --manifest-path "${ROOT}/Cargo.toml" --release
fi

has_h264_aac() {
  local file="$1"
  local probe
  probe="$("${ENGINE}" probe "${file}")"
  grep -q '"family": "h264"' <<<"${probe}" && grep -q '"family": "aac"' <<<"${probe}"
}

find_first_h264_aac_mp4() {
  for dir in "/Volumes/Movies" "/Volumes/TV Shows" "/Volumes/TVShows"; do
    [[ -d "${dir}" ]] || continue
    while IFS= read -r file; do
      if has_h264_aac "${file}"; then
        printf '%s\n' "${file}"
        return 0
      fi
    done < <(find "${dir}" -type f \( -iname '*.mp4' -o -iname '*.m4v' \) -print)
  done
}

MEDIA_FILE="${1:-$(find_first_h264_aac_mp4)}"
if [[ -z "${MEDIA_FILE}" ]]; then
  echo "No H.264/AAC MP4 found under /Volumes/Movies or /Volumes/TV Shows." >&2
  exit 1
fi

if ! has_h264_aac "${MEDIA_FILE}"; then
  echo "Selected file is not an H.264/AAC MP4: ${MEDIA_FILE}" >&2
  exit 1
fi

OUT_DIR="$(mktemp -d /tmp/chroma-engine-web-player-output.XXXXXX)"
INIT="${OUT_DIR}/init.mp4"
SEGMENT="${OUT_DIR}/seg-00000.m4s"

"${ENGINE}" hls-fmp4-init "${MEDIA_FILE}" "${INIT}" >/dev/null
"${ENGINE}" hls-fmp4-segment --index 0 "${MEDIA_FILE}" "${SEGMENT}" >/dev/null

[[ -s "${INIT}" ]] || { echo "init segment is empty" >&2; exit 1; }
[[ -s "${SEGMENT}" ]] || { echo "media segment is empty" >&2; exit 1; }
grep -a -q "ftyp" "${INIT}" || { echo "init segment missing ftyp" >&2; exit 1; }
grep -a -q "moov" "${INIT}" || { echo "init segment missing moov" >&2; exit 1; }
grep -a -q "moof" "${SEGMENT}" || { echo "media segment missing moof" >&2; exit 1; }
grep -a -q "mdat" "${SEGMENT}" || { echo "media segment missing mdat" >&2; exit 1; }

echo "smoke media: ${MEDIA_FILE}"
echo "wrote:"
echo "  ${INIT}"
echo "  ${SEGMENT}"
