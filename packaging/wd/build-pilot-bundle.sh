#!/bin/sh
# Assemble off-device ARMv7 build outputs for a controlled EX2 Ultra load test.
# This is not a WD My Cloud OS 5 .bin installer.
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 ENGINE_BINARY SERVER_BINARY SERVER_REPOSITORY" >&2
    exit 2
fi

engine_binary=$1
server_binary=$2
server_repository=$3
engine_repository=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
output_directory="$engine_repository/target/wd-ex2-ultra-armv7-pilot"

for required_file in "$engine_binary" "$server_binary" \
    "$server_repository/apps/admin-spa/dist/index.html" \
    "$engine_repository/LICENSE" \
    "$engine_repository/THIRD_PARTY_NOTICES.md" \
    "$engine_repository/THIRD_PARTY_LICENSES.md" \
    "$engine_repository/packaging/license-fallbacks.json" \
    "$engine_repository/third_party/rusty_aac/LICENSE"; do
    if [ ! -f "$required_file" ]; then
        echo "missing required file: $required_file" >&2
        exit 1
    fi
done

if [ -e "$output_directory" ]; then
    echo "output already exists: $output_directory" >&2
    exit 1
fi

mkdir -p "$output_directory/resources/bin" "$output_directory/resources/admin"
cp "$server_binary" "$output_directory/chroma-server"
cp "$engine_binary" "$output_directory/resources/bin/chroma-engine"
cp -R "$server_repository/apps/admin-spa/dist/." "$output_directory/resources/admin/"
cp "$engine_repository/LICENSE" \
    "$engine_repository/THIRD_PARTY_NOTICES.md" \
    "$engine_repository/THIRD_PARTY_LICENSES.md" \
    "$engine_repository/packaging/license-fallbacks.json" \
    "$output_directory/resources/bin/"
cp "$engine_repository/third_party/rusty_aac/LICENSE" \
    "$output_directory/resources/bin/rusty-aac-LICENSE"
cp "$engine_repository/packaging/wd/README.md" "$output_directory/README.md"

(
    cd "$output_directory"
    shasum -a 256 chroma-server resources/bin/chroma-engine > SHA256SUMS
    printf 'engine_ref=%s\nserver_ref=%s\ntarget=armv7-unknown-linux-gnueabihf\nglibc_baseline=2.31\n' \
        "$(git -C "$engine_repository" rev-parse HEAD)" \
        "$(git -C "$server_repository" rev-parse HEAD)" > BUILD-PROVENANCE.txt
)

tar -C "$(dirname "$output_directory")" -czf "$output_directory.tar.gz" \
    "$(basename "$output_directory")"
echo "$output_directory.tar.gz"
