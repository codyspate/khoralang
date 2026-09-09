#!/bin/sh
# What this repository says about itself, against what the tree actually is.
#
# The drift this catches is not carelessness. A fact kept in one place is
# corrected once; a fact stated in six places is corrected in the one somebody
# happened to be reading. `docs/positioning.md` went on publishing the
# withdrawn throughput figures after `README.md`, `bench/README.md`,
# `docs/errata.md` and the website had all been corrected -- errata 77 -- and
# nothing could have noticed, because nothing compared them.
#
# **The split that makes this checkable is between a document that makes a live
# claim and one that records history.** `README.md` says what is true now, and
# goes stale. `docs/errata.md` says what was believed in August, and cannot.
# Only the first kind is read here, which is why "1,545 tests" may stand in an
# errata entry and may not stand in the README.
#
# **Two severities, for the reason `useless-allow` is a warn.** A withdrawn
# measurement reappearing is unambiguous and fails. A count in prose is not:
# "the three examples that answer on a port" is a true statement about a subset
# and reads identically to a stale total. Those are printed for a human and
# fail nothing, because a check that cries wolf gets switched off, and the one
# finding that mattered goes with it.
#
#     sh scripts/check-claims.sh
#
# Reports everything rather than stopping at the first: a sweep that exits on
# one finding is the same sweep run five times.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# Findings go to files rather than to shell variables. Every check below reads
# its input through a pipe, and a pipe is a subshell -- a counter incremented
# inside one is discarded when it exits, so a script written the obvious way
# reports its findings and then exits 0. This is the same class of mistake as
# the `| grep` that put three commits on a red baseline.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
: >"$work/fail"
: >"$work/review"

# Documents that make a live claim. Everything not listed is history and is not
# read: `docs/errata.md`, `docs/roadmap.md`, `CHANGELOG.md`,
# `docs/release-readiness.md` and `website/content/versions/` all describe a
# moment that has passed, and a number in one of them is evidence rather than
# an assertion.
live_files() {
	for f in README.md CONTRIBUTING.md khora.toml SECURITY.md \
		docs/vision.md docs/positioning.md bench/README.md; do
		[ -f "$f" ] && echo "$f"
	done
	[ -d website/content/docs ] && find website/content/docs -name '*.md'
	true
}

# ---------------------------------------------------------------------------
# Fails: a figure that was published and withdrawn
# ---------------------------------------------------------------------------
#
# Errata 77: every throughput number this project published before September
# 2026 was two to twelve times too high, because the load generator reported
# one connection's rate multiplied by the number of connections. They were
# removed from the README and the site and left in `docs/errata.md`, which is
# where a withdrawn number belongs.
#
# `bench/README.md` is exempt because it is the measurement's home and carries
# the account of its own correction; quoting the old claim is what that
# paragraph is for.
withdrawn='538,000|560,000|1,400,000|10x Go|6x Kestrel'

live_files | grep -v '^bench/README.md$' |
	xargs grep -nE "$withdrawn" 2>/dev/null |
	while IFS= read -r hit; do
		[ -z "$hit" ] && continue
		printf 'withdrawn figure still published: %s\n  errata 77 withdrew it. `bench/README.md` has what replaced it.\n\n' "$hit"
	done >>"$work/fail"

# ---------------------------------------------------------------------------
# Fails: a measurement quoted away from its home
# ---------------------------------------------------------------------------
#
# `bench/README.md` owns the throughput table, because it is the file that also
# carries the machine, the method and the caveats -- and a number quoted away
# from those is the thing the README warns against in its own last line on the
# subject. Anywhere else may cite a figure, and it has to be one that is in the
# table, or that figure rounded: the site writes "about 174,000" for 174,201 on
# purpose, and rounding a measurement is not drifting from it.
if [ -f bench/README.md ]; then
	grep -oE '\b[0-9]{2,3},[0-9]{3}\b' bench/README.md | sort -u >"$work/canonical"
	# The same figures to three significant digits, which is what a rounded
	# citation looks like.
	while IFS= read -r n; do
		printf '%s\n' "$n" | tr -d ','
	done <"$work/canonical" | while IFS= read -r plain; do
		printf '%s\n' "$(( (plain + 500) / 1000 * 1000 ))"
	done | sed 's/\(.*\)\([0-9]\{3\}\)$/\1,\2/' >>"$work/canonical"

	live_files | grep -v '^bench/README.md$' |
		xargs grep -nE '[0-9]{2,3},[0-9]{3} ?(req/s|requests)' 2>/dev/null |
		while IFS= read -r hit; do
			[ -z "$hit" ] && continue
			said=$(printf '%s' "$hit" | grep -oE '\b[0-9]{2,3},[0-9]{3}\b' | head -1)
			grep -qxF "$said" "$work/canonical" && continue
			printf 'throughput figure not in the canonical table: %s\n  `bench/README.md` is where the number lives, with the machine and the method.\n\n' "$hit"
		done >>"$work/fail"
fi

# ---------------------------------------------------------------------------
# Fails: a fact with no canonical home asserted in two of them
# ---------------------------------------------------------------------------
#
# A test count cannot be derived without building, so nothing here can say what
# it should be. What it can say is that two live documents must not both assert
# one, because that is the shape every drift in this repository has had: the
# README said 1,376, `docs/release-readiness.md` said 2,107, and both were
# written carefully.
hits=$(live_files | xargs grep -nEi '[0-9],[0-9]{3} (rust )?tests' 2>/dev/null || true)
if [ -n "$hits" ]; then
	homes=$(printf '%s\n' "$hits" | cut -d: -f1 | sort -u | wc -l | tr -d ' ')
	if [ "$homes" -gt 1 ]; then
		{
			printf 'a test count is asserted in %s live documents:\n' "$homes"
			printf '%s\n' "$hits" | sed 's/^/  /'
			printf '  Keep it in one and have the others name the command that prints it.\n\n'
		} >>"$work/fail"
	fi
fi

# ---------------------------------------------------------------------------
# Review: counts the tree could settle
# ---------------------------------------------------------------------------
#
# Advisory, per the note at the top. Read them; most will be a subset claim
# that is correct as written.
numbers='no|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty'

word_for() {
	set -- 'no one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty' "$1"
	if [ "$2" -le 20 ] 2>/dev/null; then
		printf '%s\n' "$1" | cut -d' ' -f$(( $2 + 1 ))
	else
		printf '%s\n' "$2"
	fi
}

count_claim() {
	noun=$1
	truth=$2
	derived=$3
	right=$(word_for "$truth")

	live_files | xargs grep -nEio "(${numbers}|[0-9]+) (${noun})" 2>/dev/null |
		while IFS= read -r hit; do
			[ -z "$hit" ] && continue
			said=$(printf '%s' "$hit" | sed 's/.*://' | awk '{print tolower($1)}')
			[ "$said" = "$right" ] && continue
			[ "$said" = "$truth" ] && continue
			printf '%s\n  the tree has %s (%s), from %s\n\n' "$hit" "$right" "$truth" "$derived"
		done >>"$work/review"
}

count_claim 'examples|reference applications' \
	"$(find examples -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" 'examples/*/'
count_claim 'benchmarks' \
	"$(find bench -mindepth 2 -maxdepth 2 -name khora.toml | wc -l | tr -d ' ')" 'bench/*/khora.toml'
count_claim 'lints' \
	"$(find crates/khora-lint/src -name '*.rs' -exec cat {} + |
		grep -oE '"[a-z]+(-[a-z]+)+"' | sort -u | wc -l | tr -d ' ')" 'khora-lint sources'
count_claim 'workspace crates' \
	"$(find crates -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" 'crates/*/'

# ---------------------------------------------------------------------------

if [ -s "$work/review" ]; then
	printf 'Counts to look at -- a subset claim reads like a stale total, so these\nfail nothing:\n\n'
	cat "$work/review"
fi

if [ -s "$work/fail" ]; then
	printf '%s\n' '--------------------------------------------------------------------'
	cat "$work/fail" >&2
	count=$(grep -c '^[a-z]' "$work/fail" || true)
	printf 'check-claims: %s live document(s) disagree with the tree.\n' "$count" >&2
	exit 1
fi

echo "check-claims: no live document contradicts the tree"
