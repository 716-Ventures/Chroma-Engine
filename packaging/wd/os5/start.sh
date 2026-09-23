#!/bin/sh
set -eu

app_dir=${1:-$(pwd)}
app_dir=${app_dir%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
status_file=$app_dir/startup-status.txt
write_status() {
    printf '%s\n' "$1" > "$status_file"
    chmod 644 "$status_file"
}
write_status start-hook-entered
data_dir=${app_dir%/chromaserver}/chromaserver-data
[ ! -L "$data_dir" ] || exit 1
umask 077
mkdir -p "$data_dir/tmp"
chmod 700 "$data_dir" "$data_dir/tmp"
pid_file=$data_dir/chroma-server.pid

if [ -f "$pid_file" ]; then
    old_pid=$(cat "$pid_file")
    case "$old_pid" in ''|*[!0-9]*) exit 1 ;; esac
    if kill -0 "$old_pid" 2>/dev/null; then
        old_exe=$(readlink "/proc/$old_pid/exe" 2>/dev/null || true)
        [ "$old_exe" = "$app_dir/chroma-server" ] && exit 0
        exit 1
    fi
    rm "$pid_file"
fi

export CHROMA_DATA_DIR="$data_dir"
export CHROMA_BUNDLE_RESOURCES="$app_dir/resources"
export CHROMA_ADMIN_STATIC="$app_dir/resources/admin"
export CHROMA_NAS_PROFILE=small
export CHROMA_RESOURCE_POLICY=small-nas
export CHROMA_LAN_ACCESS=1
export TMPDIR="$data_dir/tmp"
ulimit -s 3000
cd "$app_dir"

if ! "$app_dir/resources/bin/chroma-engine" --help >/dev/null 2>&1; then
    write_status engine-load-failed
    exit 1
fi
write_status engine-loaded

log_file=$data_dir/chroma-server.log
if [ -f "$log_file" ] && [ "$(wc -c < "$log_file")" -gt 5242880 ]; then
    mv "$log_file" "$data_dir/chroma-server.previous.log"
fi
"$app_dir/chroma-server" >> "$log_file" 2>&1 &
server_pid=$!
write_status server-launched
printf '%s\n' "$server_pid" > "$pid_file"
sleep 2
if ! kill -0 "$server_pid" 2>/dev/null; then
    write_status server-exited-early
    rm "$pid_file"
    exit 1
fi
write_status server-process-alive
