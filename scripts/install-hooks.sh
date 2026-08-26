#!/bin/sh
# Installs a pre-push hook that refuses a push the baseline has not passed for.
#
#     sh scripts/install-hooks.sh
#
# **Opt-in, and pre-push rather than pre-commit.** A commit is cheap and
# frequent and often part of thinking; requiring a full baseline for one would
# be answered by `--no-verify` within a day, which is worse than no hook. A
# push is where a red tree stops being private, so that is where the question
# is asked. Roadmap 13.20.
#
# The hook runs `scripts/gate.sh`, which reads the receipt
# `scripts/baseline.sh` leaves. It does not run the baseline itself: a hook
# that takes twenty minutes is a hook that gets skipped.
#
# `git push --no-verify` still works, and that is on purpose. This exists to
# stop the accident, not to stop the decision.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

hooks=$(git rev-parse --git-path hooks)
mkdir -p "$hooks"
target="$hooks/pre-push"

if [ -f "$target" ] && ! grep -q 'khora-baseline-gate' "$target" 2>/dev/null; then
    printf 'install-hooks: %s already exists and is not ours.\n' "$target" >&2
    printf 'Move it aside, or add this line to it:\n\n' >&2
    printf '    sh scripts/gate.sh || exit 1\n' >&2
    exit 1
fi

cat > "$target" <<'HOOK'
#!/bin/sh
# khora-baseline-gate — installed by scripts/install-hooks.sh
#
# Refuses a push unless `scripts/baseline.sh` has passed for this exact tree.
# `git push --no-verify` bypasses it, deliberately: this is here to stop the
# accident, not the decision.
exec sh scripts/gate.sh
HOOK
chmod +x "$target" 2>/dev/null || true

printf 'installed %s\n' "$target"
printf '\n'
printf 'It refuses a push unless `sh scripts/baseline.sh` has passed for the\n'
printf 'tree being pushed. Bypass with `git push --no-verify`.\n'
