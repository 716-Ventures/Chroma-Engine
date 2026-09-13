#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${ROOT}/target/release/chroma-engine"
MEDIA_FILE="${1:?Usage: benchmark-native-transcode.sh MEDIA_FILE}"

cargo build --manifest-path "${ROOT}/Cargo.toml" --release >/dev/null
if [[ ! -f "${MEDIA_FILE}" ]]; then
  echo "Native transcode benchmark fixture not found: ${MEDIA_FILE}" >&2
  exit 1
fi

exec python3 "${ROOT}/scripts/benchmark-session.py" \
  --engine "${ENGINE}" --media "${MEDIA_FILE}" --count 2 --video-mode h264
