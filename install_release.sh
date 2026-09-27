#!/bin/bash

pushd target/release
cp -f bo-db bo-import bo-import-websocket bo-update bo-servicelog-stats bo-webservice /usr/local/bin/
popd
