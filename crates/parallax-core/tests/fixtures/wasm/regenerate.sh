#!/bin/sh
# Compiles the WebAssembly modules the tests read (checked in, so tests need no wasm toolchain).
# Needs: rustup target add wasm32-unknown-unknown
#   counter-v1.wasm           v1 with the name section (function names)
#   counter-v1-stripped.wasm  the same code without the name and producers sections
#   counter-v1-debug.wasm     the same code with DWARF debug info
#   counter-v2.wasm           v2: a changed loop body and string, a new export
set -e
cd "$(dirname "$0")"
build() { # <source> <output> <extra rustc flags>
    rustc --edition 2024 --crate-name counter --crate-type cdylib --target wasm32-unknown-unknown -C opt-level=s -C panic=abort \
        -C link-arg=--no-entry $3 -o "$2" "$1"
}
build counter-v1.rs counter-v1.wasm "-C strip=debuginfo"
build counter-v1.rs counter-v1-stripped.wasm "-C strip=symbols"
build counter-v1.rs counter-v1-debug.wasm "-g"
build counter-v2.rs counter-v2.wasm "-C strip=debuginfo"
