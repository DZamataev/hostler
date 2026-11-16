#!/usr/bin/env bash
# Disk hygiene for the checkouts of this repository: report build output and hostler services
# that tests left running, and with --apply delete what nobody is using. See AGENTS.md,
# "Disk hygiene".
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: scripts/dev-clean.sh [--apply] [--keep <checkout>]...

Reports free disk space, the build output of every checkout (git worktree) of this
repository (target/, tmp/target-*, stray root target-* folders) and hostler services that
tests left running.

A checkout is in use while any process's working folder, executable or open file is inside
it. When that cannot be inspected (no lsof on macOS, no /proc on Linux, or no output at all)
--apply deletes no build output, says why, and exits 1.

--apply  deletes that build output in every checkout no process works in, except the
         checkouts named with --keep and the one this runs from, stops test services
         older than two hours, and prunes worktrees whose folders are gone.
         A test service runs a program from inside one of this repository's checkouts, has
         its working folder under $TMPDIR or /tmp, and is older than two hours. Any other
         hostler service is reported as not a test service and never stopped.
         Checkouts, branches and commits are never touched.

Tests only: DEV_CLEAN_TEST_SERVICE_AGE_S replaces the two-hour age rule.
EOF
}

apply=0
keep=$'\n'
while [[ $# -gt 0 ]]; do
    case $1 in
        --apply) apply=1 ;;
        --keep)
            if [[ $# -lt 2 || ! -d $2 ]]; then
                usage
                exit 2
            fi
            keep+="$(cd "$2" && pwd -P)"$'\n'
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
    shift
done

root=$(git rev-parse --show-toplevel)

human() { awk -v k="$1" 'BEGIN { printf "%5.1fG", k / 1048576 }'; }
free_space() { df -h "$root" | awk 'NR == 2 { print $4 " free on " $NF }'; }

# Every path a process works in, runs or holds open: its working folder, its executable and each
# open file, over all processes. One sweep. No output means the inspection is unavailable (no
# lsof, no /proc), and --apply must then delete nothing.
paths_in_use() {
    if command -v lsof >/dev/null 2>&1; then
        # lsof exits 1 when it cannot read some processes (normal, e.g. other users'): its
        # output is used as it comes.
        lsof -n -P -Fn 2>/dev/null | sed -n 's/^n//p' | grep '^/' | sort -u || true
    elif [[ -d /proc ]]; then
        local link
        for link in /proc/[0-9]*/cwd /proc/[0-9]*/exe /proc/[0-9]*/fd/*; do
            readlink "$link" 2>/dev/null || true
        done | grep '^/' | sort -u || true
    fi
}

cwd_of() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' || true
    else
        readlink "/proc/$1/cwd" 2>/dev/null || true
    fi
}

# The program a process runs: its executable, and the absolute paths on its command line (a
# script's interpreter keeps the script there, not in its executable).
program_paths_of() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -a -p "$1" -d txt -Fn 2>/dev/null | sed -n 's/^n//p' || true
    elif [[ -r /proc/$1/exe ]]; then
        readlink "/proc/$1/exe" 2>/dev/null || true
    fi
    ps -o args= -p "$1" 2>/dev/null | tr -s ' \t' '\n' | grep '^/' || true
}

# The checkout `$1` belongs to: the longest one that is it or contains it (a folder inside a
# nested checkout belongs to the nested one).
checkout_of() {
    local best='' wt
    for wt in "${worktrees[@]+"${worktrees[@]}"}"; do
        if [[ $1 == "$wt" || $1 == "$wt"/* ]] && ((${#wt} > ${#best})); then
            best=$wt
        fi
    done
    printf '%s' "$best"
}

# ps etime ([[dd-]hh:]mm:ss) in seconds.
seconds() {
    awk -v t="$1" 'BEGIN {
        d = 0
        if (index(t, "-")) { split(t, a, "-"); d = a[1]; t = a[2] }
        n = split(t, p, ":"); s = 0
        for (i = 1; i <= n; i++) s = s * 60 + p[i]
        print s + d * 86400
    }'
}

worktrees=()
while IFS= read -r wt; do
    worktrees+=("$wt")
done <<<"$(git worktree list --porcelain | sed -n 's/^worktree //p' |
    while IFS= read -r wt; do
        if [[ -d $wt ]]; then (cd "$wt" && pwd -P); fi
    done)"

inspection=$(paths_in_use)
inspect_ok=1
[[ -n $inspection ]] || inspect_ok=0

# A checkout is busy when a process works in it, runs a program from it or holds a file open in
# it; the longest checkout containing that path owns it, so a nested checkout does not pin its
# parent (the same rule as checkout_of).
busy=$'\n'
while IFS= read -r p; do
    [[ -n $p ]] || continue
    best=
    for wt in "${worktrees[@]+"${worktrees[@]}"}"; do
        if [[ $p == "$wt" || $p == "$wt"/* ]] && ((${#wt} > ${#best})); then
            best=$wt
        fi
    done
    if [[ -n $best ]]; then busy+="$best"$'\n'; fi
done <<<"$inspection"

build_dirs() {
    local d
    for d in "$1/target" "$1"/target-* "$1"/tmp/target-*; do
        if [[ -d $d ]]; then printf '%s\n' "$d"; fi
    done
}

echo "disk: $(free_space)"
echo "build output by checkout:"
total=0
empty=0
removable=
for wt in "${worktrees[@]+"${worktrees[@]}"}"; do
    dirs=$(build_dirs "$wt")
    if [[ -z $dirs ]]; then
        empty=$((empty + 1))
        continue
    fi
    kb=0
    while IFS= read -r d; do
        size=$(du -sk "$d" 2>/dev/null | cut -f1)
        kb=$((kb + ${size:-0}))
    done <<<"$dirs"
    total=$((total + kb))
    branch=$(git -C "$wt" branch --show-current 2>/dev/null)
    state=
    if ((inspect_ok == 0)); then
        state="unknown: cannot inspect processes"
    elif [[ $busy == *$'\n'"$wt"$'\n'* ]]; then
        state="in use"
    elif [[ $keep == *$'\n'"$wt"$'\n'* ]]; then
        state="kept"
    else
        state="idle"
        removable+="$dirs"$'\n'
    fi
    dirty=$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
    if [[ $dirty != 0 ]]; then state+=", $dirty uncommitted"; fi
    if git -C "$wt" merge-base --is-ancestor HEAD main 2>/dev/null; then
        state+=", merged into main"
    fi
    printf '  %s  %s  [%s]  %s\n' "$(human "$kb")" "$wt" "${branch:-detached}" "$state"
done
echo "  $(human "$total")  total ($empty checkouts have none)"

tmp_root=$(cd "${TMPDIR:-/tmp}" && pwd -P)
# Tests only: a lower age rule than two hours.
age_limit=${DEV_CLEAN_TEST_SERVICE_AGE_S:-7200}
stale=
count=0
found=0
echo "hostler services matching \`service run\`:"
for pid in $(pgrep -f 'hostler[^ /]* service run' || true); do
    found=1
    c=$(cwd_of "$pid")
    age=$(ps -o etime= -p "$pid" 2>/dev/null | tr -d ' ' || true)
    secs=0
    if [[ -n $age ]]; then secs=$(seconds "$age"); fi
    why=
    case $c in
        "$tmp_root"/* | /tmp/* | /private/tmp/*) ;;
        *) why="its working folder ${c:-(unknown)} is outside $tmp_root and /tmp" ;;
    esac
    checkout=
    while IFS= read -r p; do
        [[ -n $p ]] || continue
        owned=$(checkout_of "$p")
        if [[ -n $owned ]]; then
            checkout=$owned
            break
        fi
    done <<<"$(program_paths_of "$pid")"
    if [[ -z $checkout ]]; then
        why="${why:+$why; }its program is not inside a checkout of this repository"
    fi
    if [[ -n $why ]]; then
        printf '  pid %s, age %s: not a test service: %s\n' "$pid" "${age:-?}" "$why"
        continue
    fi
    count=$((count + 1))
    if ((secs > age_limit)); then
        stale+="$pid "
        printf '  pid %s, age %s: test service of %s, older than %ss: --apply stops it\n' \
            "$pid" "$age" "$checkout" "$age_limit"
    else
        printf '  pid %s, age %s: test service of %s, younger than %ss: kept\n' \
            "$pid" "$age" "$checkout" "$age_limit"
    fi
done
if ((found == 0)); then echo "  none"; fi
echo "test services left running: $count ($(wc -w <<<"$stale" | tr -d ' ') past the ${age_limit}s age rule)"

if ((apply == 0)); then
    if ((inspect_ok == 0)); then
        echo "warning: cannot inspect which processes use the checkouts (no lsof, no /proc): --apply would delete nothing"
    elif [[ -n $removable ]]; then
        echo "--apply would delete the build output of the idle checkouts above."
    fi
    exit 0
fi

if ((inspect_ok == 0)); then
    echo "deleted no build output: cannot inspect which processes use the checkouts (no lsof, no /proc)" >&2
else
    while IFS= read -r d; do
        [[ -n $d ]] || continue
        rm -rf "$d"
        echo "deleted $d"
    done <<<"$removable"
fi
if [[ -n $stale ]]; then
    # shellcheck disable=SC2086 # a list of pids
    kill -TERM $stale 2>/dev/null || true
    echo "stopped test services: $stale"
fi
git worktree prune
echo "disk: $(free_space)"
if ((inspect_ok == 0)); then exit 1; fi
