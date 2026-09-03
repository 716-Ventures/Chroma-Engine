#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"
DEFAULT_MEDIA="/Volumes/TVShows/Is It Wrong to Try to Pick Up Girls in a Dungeon!/Season 5/Is.It.Wrong.to.Try.to.Pick.Up.Girls.in.a.Dungeon.S05E10.720p.HEVC.x265-MeGusta.mkv"
MEDIA_FILE="${1:-${DEFAULT_MEDIA}}"

cargo build --manifest-path "${ROOT}/Cargo.toml" --release >/dev/null
if [[ ! -f "${MEDIA_FILE}" ]]; then
  echo "Native transcode smoke fixture not found: ${MEDIA_FILE}" >&2
  exit 1
fi

OUTPUT_DIR="$(mktemp -d /tmp/chroma-native-transcode.XXXXXX)"
"${ENGINE}" transcode-fmp4-segments "${MEDIA_FILE}" "${OUTPUT_DIR}" \
  --init-output "${OUTPUT_DIR}/init.mp4" \
  --count 2 \
  --video-mode h264 \
  --video-bitrate 3000000 >"${OUTPUT_DIR}/result.json"

test -s "${OUTPUT_DIR}/init.mp4"
test -s "${OUTPUT_DIR}/seg-00000.m4s"
test -s "${OUTPUT_DIR}/seg-00001.m4s"
test -s "${OUTPUT_DIR}/index.m3u8"
grep -q '#EXT-X-MAP:URI="init.mp4"' "${OUTPUT_DIR}/index.m3u8"
grep -q '"codecSessionReuses": 1' "${OUTPUT_DIR}/result.json"
grep -q '"videoDecoderSessionsCreated": 1' "${OUTPUT_DIR}/result.json"
grep -q '"videoEncoderSessionsCreated": 1' "${OUTPUT_DIR}/result.json"

echo "retained native transcode smoke passed: ${OUTPUT_DIR}"
