#!/bin/bash

pushd target/release
cp -f bo-db bo-import bo-import-websocket bo-update bo-webservice /usr/local/bin/
popd
