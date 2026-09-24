#!/bin/sh
set -eu

web_dir=/var/www/apps/chromaserver
web_alias=/var/www/chromaserver
app_dir=${1:-$(cd "$(dirname "$0")" && pwd)}
app_dir=${app_dir%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
if [ -L "$web_alias" ] && [ "$(readlink "$web_alias")" = "$web_dir" ]; then
    rm "$web_alias"
fi
if [ -L "$web_dir/index.php" ] &&
        [ "$(readlink "$web_dir/index.php")" = "$app_dir/index.php" ]; then
    rm "$web_dir/index.php"
    rmdir "$web_dir" 2>/dev/null || true
fi
hook_log=${app_dir%/chromaserver}/chromaserver-data/install-hooks.log
[ ! -e "$hook_log" ] || printf 'clean completed app=%s\n' "$app_dir" >> "$hook_log"
