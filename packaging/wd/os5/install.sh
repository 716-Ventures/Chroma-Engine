#!/bin/sh
set -eu

printf 'chromaserver install.enter argc=%s source=%s destination=%s cwd=%s\n' \
    "$#" "${1-}" "${2-}" "$(pwd -P)" >&2
[ "$#" -eq 2 ] || exit 1

destination=${2%/}
case "$destination" in
    */Nas_Prog) parent=$destination ;;
    */Nas_Prog/chromaserver) parent=${destination%/chromaserver} ;;
    *) echo 'chromaserver install: invalid destination' >&2; exit 1 ;;
esac
[ -d "$parent" ] && [ ! -L "$parent" ] || exit 1
parent=$(cd -P "$parent" && pwd -P)
case "$parent" in */Nas_Prog) ;; *) exit 1 ;; esac

source_path=${1%/}
[ -d "$source_path" ] && [ ! -L "$source_path" ] || exit 1
source_path=$(cd -P "$source_path" && pwd -P)
case "$source_path" in
    "$parent/_install/chromaserver") ;;
    "$parent/_install")
        if [ -d "$source_path/chromaserver" ]; then
            source_path=$source_path/chromaserver
        fi
        ;;
    *) echo 'chromaserver install: invalid stage' >&2; exit 1 ;;
esac

app_dir=$parent/chromaserver
data_dir=$parent/chromaserver-data
[ ! -L "$app_dir" ] && [ ! -L "$data_dir" ] || exit 1
umask 077
mkdir -p "$data_dir"
chmod 700 "$data_dir"
hook_log=$data_dir/install-hooks.log
if [ -f "$hook_log" ] && [ "$(wc -c < "$hook_log")" -gt 65536 ]; then
    mv "$hook_log" "$data_dir/install-hooks.previous.log"
fi
log() { printf 'install %s\n' "$*" >> "$hook_log"; }
trap 'result=$?; log "exit=$result"; exit "$result"' 0
log "argc=$# source=$1 destination=$2 cwd=$(pwd -P) resolved_source=$source_path app=$app_dir"

for entry in install.sh init.sh preinst.sh start.sh stop.sh clean.sh remove.sh; do
    [ -f "$source_path/$entry" ] && [ -x "$source_path/$entry" ] || {
        log "missing-hook=$entry"; exit 1;
    }
done
[ -f "$source_path/apkg.rc" ] && [ -f "$source_path/apkg.xml" ] || exit 1
[ -f "$source_path/index.php" ] || exit 1
[ -x "$source_path/chroma-server" ] || exit 1
[ -x "$source_path/resources/bin/chroma-engine" ] || exit 1
[ -f "$source_path/resources/admin/index.html" ] || exit 1
log payload-validated

mkdir -p "$app_dir"
cp -a "$source_path/." "$app_dir/"
[ -x "$app_dir/chroma-server" ] && [ -x "$app_dir/resources/bin/chroma-engine" ] || exit 1
[ -f "$app_dir/resources/admin/index.html" ] && [ -f "$app_dir/index.php" ] || exit 1
log payload-installed
