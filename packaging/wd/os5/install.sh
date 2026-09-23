#!/bin/sh
set -eu

source_path=${1%/}
destination_path=${2%/}
case "$destination_path" in */Nas_Prog/chromaserver) destination_path=${destination_path%/chromaserver} ;; esac
case "$source_path" in */chromaserver) ;; *) exit 1 ;; esac
case "$destination_path" in */Nas_Prog) ;; *) exit 1 ;; esac
[ -d "$destination_path" ] || exit 1
[ ! -e "$destination_path/chromaserver" ] || exit 1
[ ! -L "$destination_path/chromaserver" ] || exit 1
mv "$source_path" "$destination_path"
