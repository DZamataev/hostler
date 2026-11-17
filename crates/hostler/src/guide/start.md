# Start here

hostler is the resource coordinator for the agents working on this machine. Several agents work at
once and compete for what there is only one of: CPU during a native build, a test phone, the
desktop screen, a test account. hostler hands each of them out in turn, and keeps the values of
secrets and accounts out of every command line, log and error message.

It is one binary talking to one per-user background service. Clients start the service on demand;
you do not manage it.

## The first two commands

```sh
hostler status          # the heavy queue, every claimed resource, the secrets database
hostler doctor          # what is wrong here, and the command that fixes it
```

`hostler status` is a read: it never registers the checkout. `hostler doctor` checks the service,
its OS registration, the contract, the secrets database, the project's `hostler.toml` and agent
docs, the device tools, this workspace's claims and the sandbox. Each problem carries its fix and
who acts on it, and `hostler doctor --fix` applies the safe ones. Exit 0 means no problem
remains, 1 means one does, 78 means doctor could not run.

## Setting a project up

```sh
hostler init            # preview: what it found, the proposed hostler.toml, the steps
hostler init --write    # write hostler.toml (only when absent) and the agent-docs block
```

`hostler init` detects the toolchain and the app ids, lists the names in `.env` files (never their
values), and proposes `hostler.toml`; it prints the steps a person has to take and the ones you
take. `hostler guide init` is the whole flow.

## The work

```sh
hostler run --heavy -- ./gradlew assembleRelease
hostler run --device pixel-9 -- ./gradlew connectedAndroidTest
hostler run --secret API_KEY -- ./deploy
hostler run --screen -- ./drive-the-gui
```

- **A heavy build** runs alone on the machine: wrap native builds, dependency installs and
  `pod install`, never tests or linters. `hostler guide heavy`.
- **A device** is a phone, an emulator or a simulator, held for the command. `hostler guide
  devices`.
- **A secret** is injected by name, never on the command line. A missing one is requested from a
  person: `hostler secret request API_KEY --reason "<why you need it>"`. `hostler guide secrets`.
- **The screen** is this machine's desktop session; hold it before driving the real GUI.
  `hostler guide screen`.
- **A test account** is a project's shared login, its fields in the environment and masked like
  secrets: `hostler run --account qa-user -- ./login-test`. `hostler guide accounts`.

A hold keeps a resource across commands — `hostler hold screen`, `hostler hold device pixel-9`,
`hostler hold account qa-user` — and `hostler release <id>` gives it back. Waiting is bounded by
`--max-wait`; `--no-wait` answers at once. `hostler guide holds` covers waiting, takeover, the
exit codes and workspaces.

## `--force`

`--force` takes a resource from live work, frees a queue slot whose command may still run, or
stops a service with work in it. An agent uses --force only with the operator's explicit
permission. Never infer it: name the command and the work it interrupts, and wait for a yes.

## In an agent sandbox

An agent sandbox denies the hostler socket until it is allowed. A sandboxed command then answers
`sandbox_denied` — never "not running" — and starts nothing; a sandboxed `hostler run --heavy`
fails open and runs unqueued with one warning. `hostler guide sandbox` has Claude Code's setting
and Codex's rule file.

## The topics

| Topic | Read it for |
|---|---|
| `hostler guide start` | this page |
| `hostler guide heavy` | the machine-wide heavy queue |
| `hostler guide secrets` | secrets, masking, and the person-only commands |
| `hostler guide holds` | holds, leases, waiting, takeover, workspaces |
| `hostler guide devices` | phones, emulators and simulators |
| `hostler guide screen` | this machine's desktop session |
| `hostler guide accounts` | a project's test accounts |
| `hostler guide init` | setting a project up |
| `hostler guide doctor` | the checks and the safe fixes |
| `hostler guide sandbox` | agent sandboxes |
| `hostler guide recovery` | every error code and what resolves it |

Every read command takes `--json` and prints the contract's `Envelope`, with a stable `code` and a
`recovery` command on errors. Use it instead of parsing human text.
