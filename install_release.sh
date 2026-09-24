#!/bin/bash

pushd target/release
cp -f bo-db bo-import bo-import-websocket bo-update /usr/local/bin/
popd
