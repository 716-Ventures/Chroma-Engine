#!/bin/sh
set -eu

app_dir=${1%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
data_dir=${app_dir%/chromaserver}/chromaserver-data
[ ! -L "$data_dir" ] || exit 1
umask 077
mkdir -p "$data_dir/tmp"
chmod 700 "$data_dir" "$data_dir/tmp"

web_link=/var/www/chromaserver
if [ -L "$web_link" ]; then
    [ "$(readlink "$web_link")" = "$app_dir" ] || exit 1
elif [ -e "$web_link" ]; then
    exit 1
else
    ln -s "$app_dir" "$web_link"
fi
printf 'init-hook-complete\n' > "$app_dir/startup-status.txt"
chmod 644 "$app_dir/startup-status.txt"
