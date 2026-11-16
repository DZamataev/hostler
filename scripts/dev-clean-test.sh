#!/usr/bin/env bash
# Exercises scripts/dev-clean.sh in a throwaway repository, without cargo and without touching
# this repository's checkouts: build output a process uses survives --apply until the process is
# gone, and only a test service (a program inside a checkout) is stopped. See AGENTS.md,
# "Disk hygiene".
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
src=$here/dev-clean.sh
if [[ ! -x $src ]]; then
    echo "cannot find scripts/dev-clean.sh next to this script" >&2
    exit 1
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/dev-clean-test.XXXXXX")
tmp=$(cd "$tmp" && pwd -P)
pids=()
cleanup() {
    local pid
    for pid in "${pids[@]+"${pids[@]}"}"; do
        kill -TERM "$pid" 2>/dev/null || true
    done
    for pid in "${pids[@]+"${pids[@]}"}"; do
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$tmp"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Runs dev-clean.sh with the throwaway repository as its root (it finds the root from its cwd).
apply() { (cd "$repo" && "$@"); }

# The report line about `path`, or nothing.
line_about() { awk -v p="$1" 'index($0, p) { print; exit }' "$2"; }

# True once `pid` is gone (reaping it); at most 10 s.
gone() {
    local pid=$1 waited=0 stat
    while ((waited < 100)); do
        stat=$(ps -o stat= -p "$pid" 2>/dev/null | tr -d ' ')
        if [[ -z $stat || $stat == Z* ]]; then
            wait "$pid" 2>/dev/null || true
            return 0
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    return 1
}

repo=$tmp/repo
wt=$tmp/wt
outside=$tmp/outside
mkdir -p "$repo" "$outside"

git init -q -b main "$repo"
git -C "$repo" -c user.name=dev-clean-test -c user.email=dev-clean-test@example.invalid \
    commit -q --allow-empty -m init
git -C "$repo" worktree add -q "$wt" -b side
mkdir -p "$wt/tmp"
[[ $(cd "$repo" && git rev-parse --show-toplevel) == "$repo" ]] ||
    fail "dev-clean.sh would not take the throwaway repository as its root"

# A checkout whose target/ holds a process that runs from it with its working folder outside
# the checkout (CRB-1: the executable, not the cwd, is inside).
mkdir -p "$wt/target"
cp /bin/sleep "$wt/target/sleeper"
if command -v codesign >/dev/null 2>&1; then
    # macOS kills a copy of a platform binary that keeps Apple's signature; an ad-hoc one runs.
    codesign --force --sign - "$wt/target/sleeper" >/dev/null 2>&1 ||
        fail "cannot sign the copied binary $wt/target/sleeper"
fi
(cd "$outside" && exec "$wt/target/sleeper" 300) &
sleeper=$!
pids+=("$sleeper")
sleep 0.2
kill -0 "$sleeper" || fail "the sleeper did not start"

# Fail closed: macOS without lsof cannot inspect processes at all, so nothing is deleted (Linux
# has /proc and needs no lsof).
if [[ $(uname -s) == Darwin ]]; then
    out=$tmp/apply-0.log
    if apply env PATH=/usr/bin:/bin "$src" --apply >"$out" 2>&1; then
        fail "CRB-1: --apply succeeded with no process inspection"
    fi
    [[ -d $wt/target ]] || fail "CRB-1: deleted build output with no process inspection"
    grep -q "cannot inspect" "$out" || fail "CRB-1: did not say why nothing was deleted: $out"
    echo "ok: CRB-1 nothing is deleted when processes cannot be inspected"
fi

out=$tmp/apply-1.log
apply "$src" --apply >"$out" 2>&1 ||
    fail "dev-clean.sh --apply failed while a process used the checkout: $out"
[[ -d $wt/target ]] ||
    fail "CRB-1: deleted the build output of a checkout a process runs a program from"
kill -0 "$sleeper" || fail "CRB-1: stopped a process it does not own"
line=$(line_about "$wt" "$out")
case $line in
    *"in use"*) echo "ok: CRB-1 a checkout whose program is running is in use" ;;
    *) fail "CRB-1: $wt was not reported in use: ${line:-no line}" ;;
esac
# A test service: a program inside the checkout, argv `service run`, cwd under $TMPDIR. One
# outside every checkout must be reported as not a test service and survive (CRB-2).
printf '#!/bin/sh\nwhile :; do sleep 0.2; done\n' >"$wt/tmp/hostler"
cp "$wt/tmp/hostler" "$outside/hostler"
chmod +x "$wt/tmp/hostler" "$outside/hostler"
(cd "$outside" && exec "$wt/tmp/hostler" service run) &
inside=$!
pids+=("$inside")
(cd "$outside" && exec "$outside/hostler" service run) &
outsider=$!
pids+=("$outsider")
sleep 0.2
kill -0 "$inside" || fail "the service inside the checkout did not start"
kill -0 "$outsider" || fail "the service outside the checkouts did not start"

# -1: every age counts, so the check does not depend on how long the run takes.
out=$tmp/apply-2.log
apply env DEV_CLEAN_TEST_SERVICE_AGE_S=-1 "$src" --apply >"$out" 2>&1 ||
    fail "dev-clean.sh --apply failed while test services ran: $out"
gone "$inside" || fail "CRB-2: did not stop the test service of $wt"
kill -0 "$outsider" || fail "CRB-2: stopped a service whose program is outside every checkout"
line=$(line_about "pid $outsider," "$out")
case $line in
    *"not a test service"*) echo "ok: CRB-2 a service outside the checkouts is not a test service" ;;
    *) fail "CRB-2: $outsider was not reported as not a test service: ${line:-no line}" ;;
esac
line=$(line_about "pid $inside," "$out")
case $line in
    *"test service of $wt"*) echo "ok: CRB-2 a service running a program from a checkout is a test service" ;;
    *) fail "CRB-2: $inside was not reported as a test service: ${line:-no line}" ;;
esac
[[ -d $wt/target ]] || fail "CRB-1: deleted build output that the sleeper still uses"

kill "$sleeper" 2>/dev/null || true
wait "$sleeper" 2>/dev/null || true

# The same, for a process whose program and working folder are outside the checkout and that only
# holds an open file in it.
: >"$wt/target/open"
(cd "$outside" && exec /bin/sleep 300 3<"$wt/target/open") &
holder=$!
pids+=("$holder")
sleep 0.2
out=$tmp/apply-1b.log
apply "$src" --apply >"$out" 2>&1 ||
    fail "dev-clean.sh --apply failed while a process held a file in the checkout: $out"
[[ -d $wt/target ]] ||
    fail "CRB-1: deleted build output a process holds an open file in"
line=$(line_about "$wt" "$out")
case $line in
    *"in use"*) echo "ok: CRB-1 a checkout with an open file is in use" ;;
    *) fail "CRB-1: $wt was not reported in use: ${line:-no line}" ;;
esac
kill "$holder" 2>/dev/null || true
wait "$holder" 2>/dev/null || true

# Once nothing uses it, the same build output goes.
out=$tmp/apply-3.log
apply "$src" --apply >"$out" 2>&1 ||
    fail "dev-clean.sh --apply failed with nothing using the checkout: $out"
[[ ! -d $wt/target ]] || fail "did not delete the idle build output $wt/target"
[[ ! -e $wt/target/sleeper ]] || fail "did not delete the idle build output's contents"
grep -qF "deleted $wt/target" "$out" || fail "did not report deleting $wt/target: $out"
kill -0 "$outsider" || fail "stopped a service outside every checkout"
echo "ok: an idle checkout's build output is deleted"

echo "all dev-clean.sh checks passed"
