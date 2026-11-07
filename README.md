# hostler

The resource coordinator for parallel AI agents. Several agents work on one machine at once and
compete for things it has only one of — CPU during a native build, a test phone, the screen, a
test account. hostler hands those resources out in turn instead of letting the agents race.

**Status: pre-release.** The first slice is the machine-wide heavy queue. Devices, secrets,
screen control and the desktop app follow.

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
