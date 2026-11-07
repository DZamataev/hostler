#!/usr/bin/env bash
# Build a public slice of a commit for the GitHub mirror.
#
# The slice is <commit>'s tree without docs/superpowers/, committed with github/main as its
# parent, and tagged <tag>. Nothing is pushed unless --push is given, and the origin (GitLab)
# remote is never used.
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: scripts/publish-slice.sh <commit> <tag> [--push]

Builds a commit from <commit>'s tree without docs/superpowers/, parented on github/main,
tags it <tag>, and prints what it would push. --push pushes github main and the tag.
EOF
}

if [[ $# -lt 2 || $# -gt 3 ]]; then
    usage
    exit 2
fi

commit_arg=$1
tag=$2
push=0
if [[ $# -eq 3 ]]; then
    if [[ $3 != --push ]]; then
        usage
        exit 2
    fi
    push=1
fi

if [[ -z $tag || $tag == -* ]]; then
    echo "error: invalid tag '$tag'" >&2
    exit 2
fi

if ! commit=$(git rev-parse --verify --quiet "${commit_arg}^{commit}"); then
    echo "error: '$commit_arg' is not a commit in this repository" >&2
    exit 1
fi

if [[ -n $(git status --porcelain) ]]; then
    echo "error: the working tree is dirty; commit or stash changes before building a slice" >&2
    exit 1
fi

if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
    echo "error: tag '$tag' already exists" >&2
    exit 1
fi

if ! git remote get-url github >/dev/null 2>&1; then
    echo "error: the github remote is not configured" >&2
    exit 1
fi

if ! parent=$(git rev-parse --verify --quiet 'github/main^{commit}'); then
    echo "error: github/main is not available locally; fetch the github remote first" >&2
    exit 1
fi

tree=$(git rev-parse "${commit}^{tree}")
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/hostler-slice.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT

export GIT_INDEX_FILE="$tmp_dir/index"
git read-tree "$tree"
git rm -r --cached --quiet --ignore-unmatch docs/superpowers
slice_tree=$(git write-tree)
unset GIT_INDEX_FILE

if git ls-tree -r --name-only "$slice_tree" | grep -q '^docs/superpowers/'; then
    echo "error: docs/superpowers/ is still in the slice tree" >&2
    exit 1
fi

slice=$(git commit-tree "$slice_tree" -p "$parent" -m "Release $tag")
git tag -a -m "Release $tag" "$tag" "$slice"

echo "Slice $tag"
echo "  commit:  $slice"
echo "  tag:     $tag (annotated)"
echo "  parent:  github/main ($parent)"
echo "  source:  $commit_arg ($commit)"
echo "  tree:    $slice_tree (no docs/superpowers/)"
echo "  files:   $(git ls-tree -r --name-only "$slice" | wc -l | tr -d ' ')"
echo
echo "Would push:"
echo "  git push github $slice:refs/heads/main"
echo "  git push github refs/tags/$tag"

if (( push )); then
    git push github "$slice:refs/heads/main"
    git push github "refs/tags/$tag"
fi
