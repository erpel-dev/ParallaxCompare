#!/bin/sh
# Rebuilds the DWARF fixtures: a small C program built with -g (DWARF 5) by the
# system GCC. v2 has a longer history array and a new variable.
set -e
cd "$(dirname "$0")"
gcc -g -O0 -o sensors src/main.c src/sensors.c
gcc -g -O0 -DHISTORY=16 -DDIAG -o sensors-v2 src/main.c src/sensors.c
