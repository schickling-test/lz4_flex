#! /bin/bash

pushd pkg-web
npm publish
popd

pushd pkg-node
npm publish
popd
