#!/bin/sh
set -eu

app_dir=${1%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
[ ! -L "$app_dir" ] || exit 1
data_dir=${app_dir%/chromaserver}/chromaserver-data
[ ! -L "$data_dir" ] || exit 1
hook_log=$data_dir/install-hooks.log
[ ! -e "$hook_log" ] || printf 'remove entered app=%s\n' "$app_dir" >> "$hook_log"
if [ -f "$data_dir/.upgrade-preserve-data" ]; then
    rm "$data_dir/.upgrade-preserve-data"
    rm -r "$app_dir"
    exit 0
fi
rm -r "$app_dir"
if [ -d "$data_dir" ]; then
    backup_dir=${data_dir}-uninstalled-$(date +%Y%m%d-%H%M%S)-$$
    [ ! -e "$backup_dir" ] || exit 1
    mv "$data_dir" "$backup_dir"
fi
