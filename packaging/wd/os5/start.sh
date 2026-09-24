#!/bin/sh
set -eu

app_dir=${1:-$(cd "$(dirname "$0")" && pwd)}
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
printf 'start entered app=%s\n' "$app_dir" >> "$data_dir/install-hooks.log"
pid_file=$data_dir/chroma-server.pid
[ ! -L "$data_dir/owner-setup-secret" ] || exit 1
rm -f "$data_dir/owner-setup-secret"
unset CHROMA_OWNER_SETUP_SECRET

if [ -f "$pid_file" ]; then
    old_pid=$(cat "$pid_file")
    case "$old_pid" in ''|*[!0-9]*) exit 1 ;; esac
    if kill -0 "$old_pid" 2>/dev/null; then
        old_exe=$(readlink "/proc/$old_pid/exe" 2>/dev/null || true)
        if [ "$old_exe" = "$app_dir/chroma-server" ]; then
            if curl --noproxy '*' --fail --silent --max-time 2 \
                    http://127.0.0.1:32410/ready > "$data_dir/ready-response.json"; then
                write_status server-ready
                printf 'start already-running pid=%s\n' "$old_pid" >> "$data_dir/install-hooks.log"
                exit 0
            fi
            write_status existing-server-not-ready
            exit 1
        fi
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
export CHROMA_OWNER_SETUP_ALLOW_PRIVATE_LAN=1
export TMPDIR="$data_dir/tmp"
ulimit -s 3000
cd "$app_dir"

if ! "$app_dir/resources/bin/chroma-engine" --help > "$data_dir/engine-load.log" 2>&1; then
    write_status engine-load-failed
    printf 'start engine-load-failed log=%s\n' "$data_dir/engine-load.log" \
        >> "$data_dir/install-hooks.log"
    exit 1
fi
write_status engine-loaded

log_file=$data_dir/chroma-server.log
if [ -f "$log_file" ] && [ "$(wc -c < "$log_file")" -gt 5242880 ]; then
    mv "$log_file" "$data_dir/chroma-server.previous.log"
fi
"$app_dir/chroma-server" </dev/null >> "$log_file" 2>&1 &
server_pid=$!
write_status server-launched
printf '%s\n' "$server_pid" > "$pid_file"
ready_attempts=${CHROMA_READY_ATTEMPTS:-30}
case "$ready_attempts" in ''|*[!0-9]*) exit 1 ;; esac
[ "$ready_attempts" -ge 1 ] && [ "$ready_attempts" -le 30 ] || exit 1
attempt=0
while [ "$attempt" -lt "$ready_attempts" ]; do
    if curl --noproxy '*' --fail --silent --max-time 2 \
            http://127.0.0.1:32410/ready > "$data_dir/ready-response.json"; then
        write_status server-ready
        printf 'start ready pid=%s seconds=%s\n' "$server_pid" "$attempt" \
            >> "$data_dir/install-hooks.log"
        exit 0
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
        write_status server-exited-early
        printf 'start server-exited-early log=%s\n' "$log_file" \
            >> "$data_dir/install-hooks.log"
        rm "$pid_file"
        exit 1
    fi
    attempt=$((attempt + 1))
    sleep 1
done
write_status server-not-ready
printf 'start server-not-ready pid=%s log=%s\n' "$server_pid" "$log_file" \
    >> "$data_dir/install-hooks.log"
kill -TERM "$server_pid" 2>/dev/null || true
rm "$pid_file"
exit 1
