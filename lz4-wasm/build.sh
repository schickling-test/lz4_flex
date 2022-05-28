#! /bin/bash

wasm-pack build --out-dir "pkg-web" --scope "schickling-tmp" --target web --release
cp package-web.json pkg-web/package.json

wasm-pack build --out-dir "pkg-node" --scope "schickling-tmp" --target bundler --release
cp package-node.json pkg-node/package.json