#!/bin/sh
# Application data is a sibling of the package and is never moved on upgrade.
if [ "$#" -gt 0 ]; then
    hook_log=${1%/}/../chromaserver-data/install-hooks.log
    [ ! -e "$hook_log" ] || printf 'preinst entered app=%s\n' "$1" >> "$hook_log"
fi
exit 0
