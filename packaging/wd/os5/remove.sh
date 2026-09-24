#!/bin/sh
set -eu

app_dir=${1%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
[ ! -L "$app_dir" ] || exit 1
hook_log=${app_dir%/chromaserver}/chromaserver-data/install-hooks.log
[ ! -e "$hook_log" ] || printf 'remove entered app=%s\n' "$app_dir" >> "$hook_log"
rm -r "$app_dir"
# Deliberately preserve the sibling chromaserver-data directory.
