# hostler

The resource coordinator for parallel AI agents. Several agents work on one machine at once and
compete for things it has only one of — CPU during a native build, a test phone, the screen, a
test account. hostler hands those resources out in turn instead of letting the agents race.

**Status: pre-release.** The first slices are the machine-wide heavy queue and per-project secrets.
Devices, screen control and the desktop app follow.

## Install from source

```sh
cargo install --path crates/hostler
```

The binary is `hostler`; it needs no daemon setup — clients start the per-user service on demand.

## Heavy queue

Wrap commands that saturate the CPU; they run one at a time across every project and worktree on
the machine:

```sh
hostler run --heavy -- ./gradlew assembleRelease
hostler run --heavy=deps --max-wait 90 -- yarn install
hostler run --heavy=ios-build --no-wait -- pod install
```

`--max-wait` is minutes (default 60, `0` = no limit). `--no-wait` exits **75** instead of waiting.
Exit **75** also means the request timed out or was withdrawn before it started. While waiting,
hostler prints your position on stderr every 30 seconds. If the service is unavailable, the command
runs unqueued after one warning — a broken hostler never blocks work.

Inspect and operate the queue:

```sh
hostler queue                         # capacity, running and waiting tasks
hostler queue history --since 1d      # finished tasks, newest first
hostler queue withdraw --id <id>      # withdraw a request that has not started
hostler queue capacity set 2          # 1–16, default 1
hostler guide heavy                   # the full agent guide
```

`hostler queue release --id <id> --force` frees a slot while its command may still run. Agents use
`--force` only with the operator's explicit permission.

## Secrets

Each project's secrets live in one KeePass database in hostler's state directory. Values are
injected by name and never pass through a command line or hostler's output.

```sh
hostler unlock                        # a person types the master password once per service start
hostler project approve               # a person approves this checkout, once
hostler secret set API_KEY            # a person types the value (or pipes it on stdin)
hostler secret import .env.local      # all-or-nothing; up to 1 MiB; prints names only
hostler run --secret API_KEY -- ./deploy
hostler run --secret-file TLS_CERT -- env
```

`secret set` reads at most 64 KiB from stdin (one trailing newline is stripped); `secret import`
reads one `.env`-style file up to 1 MiB and stores nothing on any problem. An unreadable import file
or one over the limit is refused with a fixed message that does not repeat the path.

`--secret NAME[=VAR]` puts the value in an environment variable; `--secret-file` writes it to a
private file named `secret-NAME` in a per-run directory, puts the file's full path in the
variable, and removes the file after the command (prefer it for tools that log their environment).
Both are repeatable and compose with `--heavy`. The command's stdout and stderr are masked
(`***`). A secrets problem refuses before the command starts: exit **78**.

Agents ask for a secret instead of for a value, and wait for a person:

```sh
hostler secret request API_KEY --reason "needs the payments API for the integration test"
hostler secret requests               # pending requests machine-wide
hostler secret decline <id|NAME>      # answer a pending request
hostler secret list                   # declared and stored names, state and purpose
```

A request waits 10 minutes by default (`--max-wait 0` is no limit) and exits **75** on timeout,
**130** on Ctrl+C, and **1** when a person declines. While waiting it prints the exact command a
person should run, every 30 seconds and on every change.

`unlock`, `project approve`, `secret share`/`unshare` and import's delete prompt are person-only:
"A person runs this; an agent asks the operator." `hostler service restart` locks the database
again, so a person runs `hostler unlock` afterwards. `hostler guide secrets` is the full agent
guide.

The database is KeePassXC-compatible KDBX 4.1 (`hostler.kdbx`; `hostler secret status` prints the
path). Open it in KeePassXC to inspect or edit it — hostler re-reads outside changes and never
replaces a file it has not checked.

> hostler protects against accidental exposure and against agents that follow this guide: values never appear in any hostler output, log, error, event, history row, Debug print or panic message, and no command prints a value. It does not protect against a malicious process of your own OS user: such a process can talk to the socket, run `hostler run --secret X -- sh -c 'echo $X | base64'`, or read hostler's memory.

## Service

```sh
hostler service status                # running, pid, versions, socket
hostler service start | stop | restart
hostler service install | uninstall   # register with launchd / systemd / Task Scheduler
```

`stop`, `restart`, `install` and `uninstall` refuse while heavy tasks run unless `--force`. After
upgrading the binary, run `hostler service restart` so the service matches the CLI's contract.

All read commands accept `--json` and print the contract's `Envelope`.

## Licence

MIT.
