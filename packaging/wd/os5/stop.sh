#!/bin/sh
set -eu

app_dir=${1:-$(pwd)}
app_dir=${app_dir%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
pid_file=${app_dir%/chromaserver}/chromaserver-data/chroma-server.pid
[ -f "$pid_file" ] || exit 0
server_pid=$(cat "$pid_file")
case "$server_pid" in ''|*[!0-9]*) exit 1 ;; esac
if ! kill -0 "$server_pid" 2>/dev/null; then
    rm "$pid_file"
    exit 0
fi
running_exe=$(readlink "/proc/$server_pid/exe" 2>/dev/null || true)
case "$running_exe" in
    "$app_dir/chroma-server"|"$app_dir/chroma-server (deleted)") ;;
    *) exit 1 ;;
esac
kill -TERM "$server_pid"
attempt=0
while kill -0 "$server_pid" 2>/dev/null && [ "$attempt" -lt 20 ]; do
    sleep 1
    attempt=$((attempt + 1))
done
if kill -0 "$server_pid" 2>/dev/null; then
    echo "Chroma Server did not stop within 20 seconds" >&2
    exit 1
fi
rm "$pid_file"
