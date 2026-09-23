#!/bin/sh
set -eu

app_dir=${1%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
[ ! -L "$app_dir" ] || exit 1
rm -r "$app_dir"
# Deliberately preserve the sibling chromaserver-data directory.
