#!/bin/sh
set -eu

app_dir=${1%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
[ ! -L "$app_dir" ] || exit 1
data_dir=${app_dir%/chromaserver}/chromaserver-data
[ ! -L "$data_dir" ] || exit 1
if [ -d "$data_dir" ]; then
    umask 077
    : > "$data_dir/.upgrade-preserve-data"
    hook_log=$data_dir/install-hooks.log
    [ ! -e "$hook_log" ] || printf 'preinst entered app=%s\n' "$app_dir" >> "$hook_log"
fi
