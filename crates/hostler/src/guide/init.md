# Setting a project up: hostler init

`hostler init` is how a project starts using hostler. It reads the checkout, proposes
`hostler.toml`, places the hostler block in the project's agent docs, lists the steps that need a
person, and installs or starts the service. It previews by default; `--write` writes.

```sh
hostler init              # preview: nothing is written, started or stopped
hostler init --write      # writes hostler.toml (only when absent) and the docs block, and acts
hostler init --json       # Envelope<InitReport>
```

It runs for the git checkout around the current directory; outside one it answers
`not_in_project`. Preview and `--write` print the same report:

- the toolchains and app ids it found, and the `.env` files with a count of their names — names
  only, never a value;
- the state of `hostler.toml`: absent (with the proposed file), valid, or invalid with its
  problems by line;
- the docs changes, one per file;
- the service column (below);
- the steps, grouped `for you (agent)` and `for the person`.

Exit 0 means the run completed (or previewed) cleanly; exit 1 means it refused — an invalid
`hostler.toml`, broken hostler markers, `not_in_project`, or an internal error. A refused run
writes no project file (the service column is still reported, and `--write` may still install or
start the service).

## What `--write` writes

- **`hostler.toml`, only when the checkout has none.** An existing file is never rewritten, not
  even when it is invalid. The proposal carries the detected app ids under `[apps]`, one secret per
  `.env`-file name with a TODO purpose, the toolchain's heavy commands under `[heavy]`, and the
  agent docs under `[agents] docs`. Nothing commits.
- **The hostler block in the agent docs**, between its markers:
  `<!-- hostler:begin -->` … `<!-- hostler:end -->`. It goes into the files of `[agents] docs`, by
  default `AGENTS.md`, which is created when missing. The block is appended after one blank line,
  or replaced in place when it is already there; the file's line endings are kept, and a second
  `--write` changes nothing. A `CLAUDE.md` that is a symlink to `AGENTS.md`, or that imports it
  with `@AGENTS.md`, gets no second block (`covered by AGENTS.md`). A symlink is never written
  through, a read-only file is refused, and broken markers are a problem with their line.
- **The service**, in the column below.
- Nothing else: no user-level agent config, no file under `.claude/` or `.codex/`, no commit.

The block is short (at most 25 lines) and holds only placeholders:

- the heavy-build line, with this project's listed commands;
- the device line, the secret line and the screen line;
- the force rule, word for word: An agent uses --force only with the operator's explicit
  permission.
- the pointer to `hostler guide start`, `hostler doctor` and `hostler guide sandbox`;
- the device-handling rules line (below).

No secret name and no app id is written into it.

## The device-handling prompt

`init.device_rules` is an agent step: propose device-handling instructions for this project from
its own context — which apps may be installed or uninstalled on which devices, what must never be
done to a device, when to ask the developer — show them to the developer, and only after they
agree write them into the project's agent docs under a heading. Then set
`[agents] device_rules = "<file>#<heading>"` in `hostler.toml` and run `hostler init --write`
again. Until it is set, the block's last line points at `hostler init` and `hostler doctor` warns.

## The steps

Every step has a stable id, its text, and, when a command exists, the exact command with its
`argv`. A step that needs a person is printed for the person and never run.

- **Agent steps:**
  - `init.service`: the service column's action, as a command — install this hostler, install
    when the running service's work has finished, or run init again with the newer binary.
  - `init.config.apps`: detected app ids that an existing `hostler.toml` does not declare.
  - `init.secrets.<NAME>`: `hostler secret request <NAME> --reason "<purpose>"` for each declared
    secret that has no value.
  - `init.device_rules`, and `init.commit` (review and commit the two files).
- **Person steps:**
  - `init.approve` (`hostler project approve`) and `init.unlock` (`hostler unlock`);
  - `init.secrets.import.<env file>` (`hostler secret import <file>`, names only afterwards);
  - `init.accounts.<NAME>` (`hostler account set`, one `--field` at a time);
  - `init.devices` (a person registers this project's physical phones);
  - `init.sandbox` (both agent clients' sandbox settings, printed, and written by neither).

The project-dependent steps — `init.approve`, and the missing secret, account and device steps —
appear only when the service could read the project: an unapproved project shows `init.approve`
first. `init.unlock` is not one of them: it appears whenever the final service probe finds the
service stopped, or the secrets database locked or missing, even when the project could not be
read.

## The service column

One row, decided from the OS registration and the running service:

| State | What it means |
|---|---|
| `running` | The service answers with this hostler's contract. |
| `would install`, `installed and started` | There is no registration, or it is outdated: the preview, then `--write` through the OS manager. |
| `would start`, `started` | The registration is current and the service was down. |
| `would replace the older running service`, `replaced the older running service` | A current registration and an older, idle service with a locked database: the preview, then `--write` stops it atomically and starts this hostler. |
| `… was kept` | Nothing was stopped: live work (a heavy task, a claim run, a takeover), an unlocked or silent database, a service that is not older, or a version that cannot be compared. The message says which. |
| `not installed: …` | The install was refused: live work, an unlocked database, a newer service, or a custom `HOSTLER_HOME`. |
| `unreachable (sandbox_denied)` | Nothing was attempted; `hostler guide sandbox` has the settings. |

An automatic install never takes a development build (a binary under a checkout's `target/`): the
column says so, and `init.service` gives the command for whoever means it. A `HOSTLER_HOME` other
than the real home is never registered, because the OS registration belongs to the real home; the
service of a custom home is started by its clients instead.
