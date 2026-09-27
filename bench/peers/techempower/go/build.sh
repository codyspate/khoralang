#!/bin/sh
# Build the Go server as a static binary. Module cache and build cache live on
# /general so a rebuild does not refetch.
set -e
export PATH=/general/toolchains/go/bin:$PATH
export GOTOOLCHAIN=local
export GOMODCACHE=/general/toolchains/gocache/mod
export GOCACHE=/general/toolchains/gocache/build
cd /general/khoralang/bench/peers/techempower/go
if [ ! -f go.mod ]; then
  go mod init techempower
  go get github.com/jackc/pgx/v5@v5.7.2
fi
go mod tidy
CGO_ENABLED=0 go build -p 2 -trimpath -ldflags "-s -w" -o techempower .
ls -la techempower
