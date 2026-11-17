# init detection fixtures

Synthetic checkout trees for `tests/init_detect.rs` (plan P4 Task 3). Nothing here is real
project data: every env-file value is a marker, and every app id is `com.example.*` or a
`looky`-shaped placeholder.

- `kmp/` — a Kotlin Multiplatform checkout with an Android app module and an `AGENTS.md`.
  Its `CLAUDE.md` symlink is created by the test at run time (git on Windows may not keep
  symlinks).
- `rn/` — a React Native checkout: a Groovy Android app with two ids, an Xcode project with an
  app id, a `$(…)` id and test-target ids, and a `Podfile`.
- `cargo/`, `node/`, `empty/` — one toolchain each, and nothing at all.
- `deep/` — a tree past the walk's bounds: an id at depth 4, a skipped `node_modules`, and a
  symlink loop the test creates.
