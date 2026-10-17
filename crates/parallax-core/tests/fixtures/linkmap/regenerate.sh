#!/bin/sh
# Rebuilds the linker map fixtures from src/ with GCC and the GNU and LLVM linkers.
# v1: main, control and the CAN driver (from an archive); v2: control_step grown
# and a diagnostics module added.
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
CFLAGS="-Os -ffreestanding -fno-pic -fno-asynchronous-unwind-tables -ffunction-sections -fdata-sections -nostdlib -static -no-pie"
build() { # name extra-cflags extra-objects
    gcc $CFLAGS $2 -c src/main.c -o "$tmp/main.o"
    gcc $CFLAGS $2 -c src/control.c -o "$tmp/control.o"
    gcc $CFLAGS -c src/can.c -o "$tmp/can.o"
    gcc $CFLAGS -c src/diag.c -o "$tmp/diag.o"
    rm -f "$tmp/libdrivers.a" && ar rcs "$tmp/libdrivers.a" "$tmp/can.o"
    (cd "$tmp" && gcc $CFLAGS -T "$OLDPWD/src/ecu.ld" -Wl,-Map=gnu.map -Wl,--gc-sections main.o control.o $3 libdrivers.a -o ecu.elf)
    (cd "$tmp" && gcc $CFLAGS -fuse-ld=lld -T "$OLDPWD/src/ecu.ld" -Wl,-Map=lld.map -Wl,--gc-sections main.o control.o $3 libdrivers.a -o ecu-lld.elf)
    cp "$tmp/gnu.map" "ecu-$1-gnu.map"
    cp "$tmp/lld.map" "ecu-$1-lld.map"
}
build v1 "" ""
build v2 "-DBIGGER_CONTROL -DWITH_DIAG" "diag.o"
rm -r "$tmp"
