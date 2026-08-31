#!/bin/sh
# What an ordinary HTTP client gets from `std::net::http`.
#
# `curl` rather than a hand-written socket test, deliberately. The failures this
# is here to catch are the ones where the server is self-consistent and wrong:
# a reader that stops at the first short recv answers its own test suite
# perfectly and returns 400 to a real client, which is how that bug reached a
# reviewer rather than a test.
#
# Uses the link shortener, because it is the reference application with a body,
# a path parameter, a query string and a redirect.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
# `khora build` puts a package's program in the package's own `build/`, named
# after the package and given the host's executable extension -- so this one is
# `build/link_shortener.exe` on Windows and `build/link_shortener` elsewhere.
built() {
    [ -x "$1.exe" ] && printf '%s\n' "$1.exe" || printf '%s\n' "$1"
}

khora="./target/debug/khora.exe"
[ -x "$khora" ] || khora="./target/debug/khora"
port=18960
base="http://127.0.0.1:$port"

command -v curl > /dev/null || { echo "curl is needed for this check"; exit 1; }

# `KHORA_PROFILE` reaches this the same way it reaches any other build, so
#
#     KHORA_PROFILE=release sh scripts/http_conformance.sh
#
# asks the same questions of an optimized server. Worth doing after a change to
# code generation: an optimizer is what turns a latent assumption into a wrong
# answer, and a server is the program here with the most of them.
"$khora" build examples/link_shortener > /dev/null

# The shortener persists to `$LINKS_FILE`, defaulting to `./links.txt` in the
# working directory rather than beside its source. Pointed somewhere disposable
# so a conformance run neither reads a previous one's links nor leaves any.
store=$(mktemp -d)/links.txt

PORT=$port LINKS_FILE=$store "$(built ./examples/link_shortener/build/link_shortener)" > /dev/null 2>&1 &
server=$!
# shellcheck disable=SC2064
trap "kill $server 2>/dev/null || true; rm -f '$store'" EXIT

ready=0
attempt=0
while [ "$attempt" -lt 50 ]; do
    if curl -s -o /dev/null "$base/health"; then ready=1; break; fi
    attempt=$((attempt + 1))
done
[ "$ready" = 1 ] || { echo "the server never came up"; exit 1; }

failures=0
check() {
    what=$1
    expected=$2
    actual=$3
    if [ "$actual" = "$expected" ]; then
        printf '  ok    %s\n' "$what"
    else
        printf '  FAIL  %s\n        expected %s\n        got      %s\n' \
            "$what" "$expected" "$actual"
        failures=$((failures + 1))
    fi
}

# A plain GET, and the status line an ordinary client parses.
check "GET /health is 200" "200" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$base/health")"

# A POST with a body and a Content-Length, which is the shape that used to be
# read as empty and answered with a 400.
created=$(curl -s -X POST "$base/links" -d '{"url":"https://example.test/a/long/path"}')
check "POST with a body is accepted" "1" \
    "$(printf '%s' "$created" | grep -c '"code"')"

code=$(printf '%s' "$created" | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
[ -n "$code" ] || { echo "  FAIL  the response carried no code: $created"; exit 1; }

# A path parameter, and a redirect an ordinary client follows.
check "GET /l/:code redirects" "302" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$base/l/$code")"
check "the redirect points at the original" "https://example.test/a/long/path" \
    "$(curl -s -o /dev/null -w '%{redirect_url}' "$base/l/$code")"

# A query string with a percent escape and an ampersand, round-tripped through
# the parser that decodes it.
escaped=$(curl -s -X POST "$base/links" -d '{"url":"https://example.test/x%20y?a=1&b=2"}')
escaped_code=$(printf '%s' "$escaped" | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
check "a percent-encoded target survives" "https://example.test/x%20y?a=1&b=2" \
    "$(curl -s -o /dev/null -w '%{redirect_url}' "$base/l/$escaped_code")"

# An unknown route, and an unknown code, are different answers.
check "an unknown route is 404" "404" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$base/nothing-here")"
check "an unknown code is 404" "404" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$base/l/zzzzzz")"

# Three requests down one connection: curl reuses by default, so this fails if
# keep-alive framing is wrong even though each request on its own would pass.
# One `-o` per URL, or curl sends the second and third bodies to stdout and the
# check measures its own mistake.
check "three requests on one connection" "200200200" \
    "$(curl -s -w '%{http_code}' \
        -o /dev/null "$base/health" \
        -o /dev/null "$base/health" \
        -o /dev/null "$base/health")"

# The client asking to close, which the server must honour rather than ignore.
check "Connection: close is honoured" "close" \
    "$(curl -s -o /dev/null -D - -H 'Connection: close' "$base/health" \
        | tr -d '\r' | grep -i '^connection:' | cut -d' ' -f2)"

# A request larger than one read but under the limit, which is what a browser
# with a long cookie sends.
long=$(printf 'x%.0s' $(seq 1 3000))
check "a 3 KB header is read whole" "200" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H "X-Filler: $long" "$base/health")"

# Over the limit is a refusal rather than a crash or a hang.
#
# **This one used to flake, and finding out why fixed a real defect.** The
# server answers 413 as soon as the buffer is full, while the client is still
# writing the ninth kilobyte — and a bare `closesocket` sends an RST, which
# discards the response the client had not read yet. curl reported `000`, under
# load, sometimes. `std::net::socket::shut` shuts the writing half and drains
# before closing now.
huge=$(printf 'x%.0s' $(seq 1 9000))
check "a 9 KB header is refused, not fatal" "413" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H "X-Filler: $huge" "$base/health")"

# The server is still answering after all of that.
check "the server survived" "200" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$base/health")"

[ "$failures" = 0 ] || { printf '\n%s conformance failure(s)\n' "$failures"; exit 1; }
printf '  %s\n' "all conformance checks passed"
