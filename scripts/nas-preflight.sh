#!/bin/sh
# Read-only, BusyBox-compatible NAS inventory. Never prints shares, serials,
# mount sources, network addresses, process arguments, or environment variables.

set -u

print_value() {
    printf '%s: %s\n' "$1" "$2"
}

command_value() {
    if command -v "$2" >/dev/null 2>&1; then
        value=$($2 $3 2>/dev/null) || value=unavailable
    else
        value=unavailable
    fi
    print_value "$1" "$value"
}

print_value preflight_version 1
command_value machine uname '-m'
command_value kernel uname '-r'
command_value libc getconf 'GNU_LIBC_VERSION'

if [ -r /etc/os-release ]; then
    os_id=$(sed -n 's/^ID=//p' /etc/os-release | sed -n '1p' | tr -d '"')
    os_version=$(sed -n 's/^VERSION_ID=//p' /etc/os-release | sed -n '1p' | tr -d '"')
    print_value os_id "${os_id:-unknown}"
    print_value os_version "${os_version:-unknown}"
else
    print_value os_id unavailable
    print_value os_version unavailable
fi

for firmware_file in /etc/version /etc/fw_version /etc/firmware-version; do
    if [ -r "$firmware_file" ]; then
        firmware=$(sed -n '1p' "$firmware_file" | cut -c 1-80)
        print_value firmware "${firmware:-unknown}"
        break
    fi
done

if command -v readelf >/dev/null 2>&1; then
    print_value userspace_elf "$(readelf -h /bin/sh 2>/dev/null | sed -n '/Class:/s/.*Class:[[:space:]]*//p' | sed -n '1p')"
elif command -v file >/dev/null 2>&1; then
    case "$(file -L /bin/sh 2>/dev/null)" in
        *32-bit*) print_value userspace_elf ELF32 ;;
        *64-bit*) print_value userspace_elf ELF64 ;;
        *) print_value userspace_elf unavailable ;;
    esac
else
    print_value userspace_elf unavailable
fi

if [ -r /proc/meminfo ]; then
    awk '/^(MemTotal|MemAvailable|SwapTotal|SwapFree):/ {print tolower($1) " " $2 " kB"}' /proc/meminfo | tr -d ':'
else
    print_value memory unavailable
fi

if command -v df >/dev/null 2>&1; then
    df -Pk / 2>/dev/null | awk 'NR==2 {print "root_storage_kib: total=" $2 " available=" $4}'
fi

if [ -r /proc/mounts ]; then
    awk '{print $3}' /proc/mounts | sort -u | awk '{print "mount_type: " $0}'
fi

if [ -d /sys/fs/cgroup ]; then
    if [ -r /sys/fs/cgroup/cgroup.controllers ]; then
        print_value cgroup_version 2
    else
        print_value cgroup_version 1_or_unavailable
    fi
else
    print_value cgroup_version unavailable
fi

for runtime in docker podman; do
    if command -v "$runtime" >/dev/null 2>&1; then
        print_value "${runtime}_binary" present
    else
        print_value "${runtime}_binary" absent
    fi
done

if [ -d /dev/dri ]; then
    render_count=$(find /dev/dri -maxdepth 1 -name 'renderD*' -type c 2>/dev/null | wc -l | tr -d ' ')
    print_value render_nodes "$render_count"
else
    print_value render_nodes 0
fi

if [ -r /proc/net/tcp ] || [ -r /proc/net/udp ]; then
    for table in tcp tcp6 udp udp6; do
        if [ -r "/proc/net/$table" ]; then
            awk -v kind="$table" 'NR>1 {split($2, local, ":"); if (kind ~ /^udp/ || $4 == "0A") print kind "_port: " sprintf("%d", "0x" local[length(local)])}' "/proc/net/$table" | sort -u
        fi
    done
else
    print_value listeners unavailable
fi
