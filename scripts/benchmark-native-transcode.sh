#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"
DEFAULT_MEDIA="/Volumes/TVShows/Is It Wrong to Try to Pick Up Girls in a Dungeon!/Season 5/Is.It.Wrong.to.Try.to.Pick.Up.Girls.in.a.Dungeon.S05E10.720p.HEVC.x265-MeGusta.mkv"
MEDIA_FILE="${1:-${DEFAULT_MEDIA}}"

cargo build --manifest-path "${ROOT}/Cargo.toml" --release >/dev/null
if [[ ! -f "${MEDIA_FILE}" ]]; then
  echo "Native transcode benchmark fixture not found: ${MEDIA_FILE}" >&2
  exit 1
fi

BASELINE_DIR="$(mktemp -d /tmp/chroma-transcode-baseline.XXXXXX)"
RETAINED_DIR="$(mktemp -d /tmp/chroma-transcode-retained.XXXXXX)"

echo "one-shot sessions (two process/session/index lifecycles)"
/usr/bin/time -lp sh -c '
  "$1" transcode-fmp4-segment "$2" "$3/seg-00000.m4s" --index 0 --video-mode h264 --video-bitrate 3000000 >/dev/null
  "$1" transcode-fmp4-segment "$2" "$3/seg-00001.m4s" --index 1 --video-mode h264 --video-bitrate 3000000 >/dev/null
' sh "${ENGINE}" "${MEDIA_FILE}" "${BASELINE_DIR}"

echo "retained session (one source/index/codec lifecycle)"
/usr/bin/time -lp "${ENGINE}" transcode-fmp4-segments "${MEDIA_FILE}" "${RETAINED_DIR}" \
  --count 2 \
  --video-mode h264 \
  --video-bitrate 3000000 >"${RETAINED_DIR}/result.json"

grep '"codecSession' "${RETAINED_DIR}/result.json"
echo "baseline artifacts: ${BASELINE_DIR}"
echo "retained artifacts: ${RETAINED_DIR}"
