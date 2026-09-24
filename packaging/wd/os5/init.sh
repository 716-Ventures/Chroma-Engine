#!/bin/sh
set -eu

app_dir=${1:-$(cd "$(dirname "$0")" && pwd -P)}
app_dir=${app_dir%/}
case "$app_dir" in */Nas_Prog/chromaserver) ;; *) exit 1 ;; esac
data_dir=${app_dir%/chromaserver}/chromaserver-data
[ ! -L "$data_dir" ] || exit 1
umask 077
mkdir -p "$data_dir/tmp"
chmod 700 "$data_dir" "$data_dir/tmp"

web_dir=/var/www/apps/chromaserver
web_alias=/var/www/chromaserver
[ -d /var/www/apps ] || exit 1
[ ! -L "$web_dir" ] || exit 1
mkdir -p "$web_dir"
chmod 755 "$web_dir"
if [ -L "$web_dir/index.php" ]; then
    [ "$(readlink "$web_dir/index.php")" = "$app_dir/index.php" ] || exit 1
elif [ -e "$web_dir/index.php" ]; then
    exit 1
else
    ln -s "$app_dir/index.php" "$web_dir/index.php"
fi
if [ -L "$web_alias" ]; then
    [ "$(readlink "$web_alias")" = "$web_dir" ] || exit 1
elif [ -e "$web_alias" ]; then
    exit 1
else
    ln -s "$web_dir" "$web_alias"
fi
printf 'init-hook-complete\n' > "$app_dir/startup-status.txt"
chmod 644 "$app_dir/startup-status.txt"
printf 'init completed app=%s web=%s alias=%s\n' "$app_dir" "$web_dir" "$web_alias" \
    >> "$data_dir/install-hooks.log"
