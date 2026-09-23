#!/bin/sh
set -eu

web_link=/var/www/chromaserver
app_dir=${1:-$(pwd)}
app_dir=${app_dir%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
if [ -L "$web_link" ] && [ "$(readlink "$web_link")" = "$app_dir" ]; then
    rm "$web_link"
fi
