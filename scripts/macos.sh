#!/usr/bin/env bash
# The macOS app's headless commands (plan P5, Global Constraints "Headless commands").
# Every subcommand runs from the repository root, prints each command before it runs it, and
# stops at the first failure. Build output stays inside this checkout (AGENTS.md):
# tmp/target-swift for SwiftPM, tmp/target-xcode for xcodebuild.
#
#   scripts/macos.sh gen               generate macos/Hostler.xcodeproj from macos/project.yml
#   scripts/macos.sh test              swift test of the HostlerKit package
#   scripts/macos.sh build             gen, the Debug app build, the hosted app tests
#   scripts/macos.sh app               gen, the Release build, copied to tmp/app/Hostler.app
#   scripts/macos.sh install           app, then copied to /Applications/Hostler.app (not running)
#   scripts/macos.sh contract [--write] check (or rewrite) the generated Swift contract (Task 3)
#   scripts/macos.sh icon              regenerate AppIcon.icns and the status item template
#
# Every xcodebuild build is ad-hoc signed (CODE_SIGN_IDENTITY=- with no team), never
# CODE_SIGNING_ALLOWED=NO: a linker-signed bundle does not bind its Info.plist and cannot post
# notifications (P5 Task 0 facts §3, lead ruling P5-L5).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

project=macos/Hostler.xcodeproj
derived=tmp/target-xcode
signing=(CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=)

run() {
    printf '+ %s\n' "$*" >&2
    "$@"
}

die() {
    printf 'scripts/macos.sh: %s\n' "$*" >&2
    exit 1
}

crate_version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' crates/hostler/Cargo.toml | head -n 1
}

app_version() {
    sed -n 's/^ *MARKETING_VERSION: "\(.*\)"$/\1/p' macos/project.yml | head -n 1
}

check_version() {
    local crate app
    crate="$(crate_version)"
    app="$(app_version)"
    [[ -n "$crate" ]] || die "cannot read the version from crates/hostler/Cargo.toml"
    [[ "$crate" == "$app" ]] ||
        die "macos/project.yml MARKETING_VERSION is $app but the crate is $crate; make them agree"
}

cmd_gen() {
    check_version
    run xcodegen generate --spec macos/project.yml --project macos
}

cmd_test() {
    run swift test --package-path macos --scratch-path tmp/target-swift
}

xcodebuild_app() {
    local configuration=$1 destination=$2 action=$3
    run xcodebuild -project "$project" -scheme Hostler -configuration "$configuration" \
        -destination "$destination" -derivedDataPath "$derived" "${signing[@]}" "$action"
}

cmd_build() {
    cmd_gen
    xcodebuild_app Debug 'platform=macOS,arch=arm64' build
    xcodebuild_app Debug 'platform=macOS,arch=arm64' test
}

# Debug-only code must not reach a Release build (lead rulings P5-L5, P5-L6; review R1-2): the
# launch hooks' variable names and the Debug menu's marker (DebugCommands.marker) are in every
# Debug build and in no Release build. Byte-wise (LC_ALL=C, -a): under a UTF-8 locale BSD grep
# does not match inside a binary.
debug_only_markers=(
    HOSTLER_APP_TEST_OPEN_POPOVER
    HOSTLER_APP_TEST_KEEP_POPOVER
    io.github.dzamataev.hostler.debug.reveal-popover
)

check_release_has_no_debug_hooks() {
    local dir=$1 marker
    for marker in "${debug_only_markers[@]}"; do
        if LC_ALL=C grep -rqaF "$marker" "$dir"; then
            die "the Release app contains the Debug-only $marker"
        fi
    done
}

cmd_app() {
    cmd_gen
    xcodebuild_app Release 'generic/platform=macOS' build
    check_release_has_no_debug_hooks "$derived/Build/Products/Release/Hostler.app/Contents/MacOS"
    run rm -rf tmp/app/Hostler.app
    run mkdir -p tmp/app
    run ditto "$derived/Build/Products/Release/Hostler.app" tmp/app/Hostler.app
    printf 'built tmp/app/Hostler.app\n'
}

cmd_install() {
    if pgrep -f '/Applications/Hostler.app/Contents/MacOS/Hostler' >/dev/null; then
        die "Hostler is running from /Applications; quit it (popover > Quit) and run this again"
    fi
    cmd_app
    run rm -rf /Applications/Hostler.app
    run ditto tmp/app/Hostler.app /Applications/Hostler.app
    printf 'installed /Applications/Hostler.app\n'
}

cmd_contract() {
    local write=0
    case "${1:-}" in
        "") ;;
        --write) write=1 ;;
        *) die "usage: scripts/macos.sh contract [--write]" ;;
    esac
    [[ -d crates/hostler/src/contract/swift ]] ||
        die "the Swift contract emitter does not exist yet (plan P5, Task 3)"
    if ((write)); then
        run env HOSTLER_WRITE_SWIFT=1 cargo test -p hostler --lib contract::swift
    else
        run cargo test -p hostler --lib contract::swift
    fi
}

cmd_icon() {
    local work=tmp/macos-icon
    run rm -rf "$work"
    run mkdir -p "$work"
    run swift scripts/macos-icon.swift "$work/AppIcon.iconset" \
        macos/Sources/Hostler/Resources/StatusItem/hostler-template.pdf
    run iconutil -c icns "$work/AppIcon.iconset" -o macos/AppBundle/AppIcon.icns
}

case "${1:-}" in
    gen) cmd_gen ;;
    test) cmd_test ;;
    build) cmd_build ;;
    app) cmd_app ;;
    install) cmd_install ;;
    contract) shift; cmd_contract "$@" ;;
    icon) cmd_icon ;;
    *) die "usage: scripts/macos.sh gen|test|build|app|install|contract [--write]|icon" ;;
esac
