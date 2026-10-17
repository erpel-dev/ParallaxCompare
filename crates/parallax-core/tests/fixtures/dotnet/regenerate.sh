#!/bin/sh
# Compiles the .NET assemblies the tests read (checked in, so tests need no .NET SDK).
# Needs no installed SDK: downloads the C# compiler (Roslyn), the .NET 8 runtime to run
# it and the reference assemblies from NuGet into a cache directory ($NUGET_CACHE).
#   Inventory-v1.dll          v1, deterministic build with a portable PDB
#   Inventory-v1-rebuilt.dll  the same source built again non-deterministically, the PDB
#                             elsewhere: new MVID, timestamp and debug directory, same metadata and IL
#   Inventory-v1-release.dll  v1 with -optimize+ (other IL)
#   Inventory-v2.dll          v2: a new version, a changed method body, a new method
#   native.dll                a native DLL built with clang and lld
set -e
cd "$(dirname "$0")"
cache=${NUGET_CACHE:-/tmp/parallax-nuget}
nuget() { # <package> <version>: unzips the package into $cache/<package>
    if [ ! -d "$cache/$1" ]; then
        mkdir -p "$cache/$1"
        curl -sSfL -o "$cache/$1.nupkg" "https://api.nuget.org/v3-flatcontainer/$1/$2/$1.$2.nupkg"
        unzip -q -o "$cache/$1.nupkg" -d "$cache/$1"
    fi
}
nuget microsoft.netcore.app.runtime.linux-x64 8.0.11
nuget microsoft.netcore.app.host.linux-x64 8.0.11
nuget microsoft.netcore.app.ref 8.0.11
nuget microsoft.net.compilers.toolset 4.11.0
# A .NET installation layout for the framework-dependent compiler.
root=$cache/dotnet
if [ ! -d "$root" ]; then
    mkdir -p "$root/host/fxr/8.0.11" "$root/shared/Microsoft.NETCore.App/8.0.11"
    rt=$cache/microsoft.netcore.app.runtime.linux-x64/runtimes/linux-x64
    cp "$rt"/lib/net8.0/* "$rt"/native/* "$root/shared/Microsoft.NETCore.App/8.0.11/"
    cp "$rt/native/libhostfxr.so" "$root/host/fxr/8.0.11/"
fi
bin=$cache/microsoft.net.compilers.toolset/tasks/netcore/bincore
if [ ! -x "$bin/csc" ]; then
    # The app host with the path of csc.dll in place of its placeholder.
    python3 - "$cache/microsoft.netcore.app.host.linux-x64/runtimes/linux-x64/native/apphost" "$bin/csc" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
at = data.index(b'c3ab8ff13720e8ad9047dd39466b3c8974e592c2fa383d4a3960714caef0c4f2')
open(sys.argv[2], 'wb').write(data[:at] + b'csc.dll'.ljust(1024, b'\0') + data[at + 1024:])
PY
    chmod +x "$bin/csc"
fi
refs=""
for r in "$cache"/microsoft.netcore.app.ref/ref/net8.0/*.dll; do refs="$refs -r:$r"; done
csc() { DOTNET_ROOT=$root "$bin/csc" -nologo -noconfig -nostdlib -t:library -langversion:latest $refs "$@"; }
out=$(mktemp -d)
build() { # <output> <source> <PDB folder> <flags>: the assembly is named Inventory whatever the file is called
    mkdir -p "$out/$3"
    csc -debug:portable -pdb:"$out/$3/Inventory.pdb" -pathmap:"$out=/build" -out:"$out/Inventory.dll" $4 "$2"
    cp "$out/Inventory.dll" "$1"
}
build Inventory-v1.dll Inventory-v1.cs a -deterministic
build Inventory-v1-rebuilt.dll Inventory-v1.cs b -deterministic-
build Inventory-v1-release.dll Inventory-v1.cs a "-deterministic -optimize+"
build Inventory-v2.dll Inventory-v2.cs a -deterministic
rm -rf "$out"
# A native DLL (no CLI header), for detection tests.
clang --target=x86_64-pc-windows-msvc -O2 -nostdlib -fuse-ld=lld -shared -Wl,/noentry -o native.dll native.c
rm -f native.lib native.exp
