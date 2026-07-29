#!/bin/sh -e
# Build the WASM package for the static web page into web/pkg/.
cd "$(dirname "$0")/.."
wasm-pack build crates/printa-ble-web --target web --out-dir ../../web/pkg
echo "Serve with: python3 -m http.server 8080 -d web"
