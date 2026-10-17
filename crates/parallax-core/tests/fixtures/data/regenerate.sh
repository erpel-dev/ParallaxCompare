#!/bin/sh
# compose.yaml and Cargo.toml are hand-written; the variants are made by real tools:
#   compose-reformatted.yaml  PyYAML: anchors expanded, keys sorted, flow style, comments gone
#   compose-changed.yaml      one port changed
#   Cargo-reordered.toml      tomllib + tomli_w: keys in reverse order, inline tables expanded
set -e
cd "$(dirname "$0")"
python3 -c '
import yaml
d = yaml.safe_load(open("compose.yaml"))
open("compose-reformatted.yaml", "w").write(yaml.safe_dump(d, default_flow_style=True, width=60))
'
sed 's/"8080:80"   # public/"8081:80"/' compose.yaml > compose-changed.yaml
python3 -c '
import tomllib, tomli_w
def rev(v):
    return {k: rev(v[k]) for k in sorted(v, reverse=True)} if isinstance(v, dict) else v
d = tomllib.load(open("Cargo.toml", "rb"))
open("Cargo-reordered.toml", "wb").write(tomli_w.dumps(rev(d)).encode())
'
