# AGENTS.md

Working rules for agents in this repository. `CLAUDE.md` imports this file; edit this one.

## Disk hygiene

Builds here are big. One checkout's macOS `target/` grows to 6–13 GB. Its Docker caches for
Linux (`tmp/target-linux`) and Windows (`tmp/target-win`) take 4–12 GB each. On 2026-11-08 a
phase with a dozen worktrees had piled up about 100 GB. The disk filled, Docker went read-only,
and a full test run failed for reasons unrelated to the code. Free space is a precondition of
every run.

**Where build output may live.** Only inside the checkout that builds it:

- `target/` for macOS;
- `tmp/target-linux`, `tmp/target-win` or another `tmp/target-<name>` for Docker and cross
  builds.

Never put a `target-*` folder at a checkout's root: tools that checkpoint a worktree pick it
up, and one got 1.7 GB of build output into `.git` that way. Never point `CARGO_TARGET_DIR`
outside the checkout (for example `/private/tmp/<x>-target`), because the cleanup never finds
it there. Scratch checkouts for reviews and experiments go in the main checkout's `tmp/` as
`tmp/wt-<name>`, with their build output inside them.

**The tool.** `scripts/dev-clean.sh` reports:

- free space;
- the build output of every worktree of this repository: its size and branch, whether a process
  works in, runs a program from or holds a file open in it, whether it has uncommitted changes,
  and whether it is merged into `main`;
- hostler services matching `service run`, each as a test service or as "not a test service".

With `--apply` it also deletes the build output of every checkout no process uses. It keeps
the checkouts named with `--keep` and the checkout it runs from. It stops test services
older than two hours. It never removes a checkout, a branch or a commit. Build output is
always safe to delete; the cost is a rebuild of a few minutes.

A service is a test service only when it runs a program from inside one of this repository's
checkouts and its working folder is under `$TMPDIR` or `/tmp`; a service run from anywhere
else (the cargo or brew install, say) is reported as not a test service and never stopped.
When the tool cannot inspect the processes at all (no `lsof` on macOS, no `/proc` on Linux),
`--apply` deletes no build output and says why. `scripts/dev-clean-test.sh` checks both rules
in a throwaway repository.

**When to clean.** The agent running the phase (the lead) owns this. A coder cleans only its
own scratch and never touches another checkout.

1. **Before a heavy run** (the full suite, the three-platform run, a release build), run
   `scripts/dev-clean.sh`. If less than 40 GB is free, run it with `--apply` first.
   On a nearly full disk, a run that fails with odd I/O errors, Docker's "read-only file
   system" or timing failures proves nothing. Clean up and rerun it.
2. **When a task is merged and its review is closed**, delete that worktree's build output
   with `--apply`, passing `--keep` for checkouts still at work. The worktree itself stays
   for fix rounds.
3. **When a reviewer or scratch checkout is done**, remove it with `git worktree remove`.
4. **When a phase ships** (merged to `main` and released):
   - remove the phase's task worktrees and the lead's phase worktree with
     `git worktree remove`;
   - delete the merged task branches with `git branch -d`;
   - run `scripts/dev-clean.sh --apply`;
   - delete the binaries copied into the team's notes under `tmp/`, keeping the notes and
     logs.
5. **After a test run that was killed or timed out**, check the report for test services
   left running. Tests stop the services they start, but a killed run cannot, and `--apply`
   stops them.

**Never delete:**

- a checkout with uncommitted or unmerged work;
- anything a running agent uses;
- other projects' files in `/private/tmp` or `$TMPDIR`;
- `~/.cargo/registry`;
- the tools' own refs, such as `refs/t3/*`.
