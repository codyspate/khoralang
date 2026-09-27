#!/bin/sh
set -e
cd /general/khoralang/bench/peers/techempower/node
npm install --no-audit --no-fund --omit=dev 2>&1 | tail -3
ls node_modules | head
