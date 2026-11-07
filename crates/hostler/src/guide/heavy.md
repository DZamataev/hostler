# The heavy queue

Your machine runs one CPU-saturating task at a time. A native build, a dependency install or
`pod install` already keeps every core busy; running a second one makes both slower and makes
timings meaningless. hostler keeps one machine-wide queue for those tasks, across every project
and every git worktree.

Wrap a command when it is heavy:

```sh
hostler run --heavy -- ./gradlew assembleRelease
hostler run --heavy=deps -- yarn install
hostler run --heavy=ios-build --max-wait 90 -- bundle exec pod install
```

Do not wrap quick commands (tests, linters, a single file build). The point is scarce CPU, not
politeness.

## What the wrapper guarantees

- The command starts only after a slot in the machine-wide queue is free.
- The slot is held until your command exits. If the wrapper itself is killed, it stays taken
  while the command's process group still has a live process.
- While you wait, hostler prints where you are every 30 seconds and on every change, on stderr.
- When the queue is full, you wait; your work is not rejected.

Kinds are free labels for people and history. `--heavy` alone means `other`; an explicit kind must
match `^[a-z0-9-]{1,32}$` (for example `gradle`, `ios-build`, `deps`).

## When the slot is released

When your command exits, the slot is released even if it left background processes behind
(build daemons such as the Kotlin or Gradle daemons outlive builds for hours and must not hold the
queue). Do not background heavy work inside a wrapped command. If the wrapper itself is killed,
the slot stays taken while the command's process group still has a live process;
`hostler queue release --id <id> --force` (operator's permission) frees it.

## Waiting

- `--max-wait MIN` — minutes to wait; fractions are fine (`1.5`), `0` means no limit. Without the
  flag, `$HOSTLER_HEAVY_MAX_WAIT` is used, then **60** minutes.
- `--no-wait` — do not wait: exit **75** immediately when no slot is free.

Waiting status and the final exit code are the contract:

| Exit | Meaning |
|---|---|
| **75** | Timed out in the queue, refused by `--no-wait`, withdrawn by an operator, or the service reported `queue_busy`. |
| **130** | You interrupted the wait with Ctrl+C; the request was withdrawn. |
| other | The command's own exit status; when it dies from a signal `n`, the wrapper exits `128+n`. |

A withdrawal (Ctrl+C, timeout, `hostler queue withdraw`) only removes a request that has not
started. A task that is already running keeps running.

## Failing open

If the service cannot be reached, started or answered in time, the wrapper warns once on stderr
and runs the command **unqueued**:

```
[hostler] Warning: the hostler service is unavailable (...); running without the heavy queue.
```

The same happens if hostler cannot read its own process identity. A broken hostler never blocks
your work — but an unqueued run provides no protection against a parallel build. If you see the
warning, tell the operator.

Stop conditions (timeout, `--no-wait`, Ctrl+C, an operator withdrawal) still win over a recovered
connection: they are not treated as "service unavailable".

## Nesting

A command that holds a slot gets `HOSTLER_HEAVY_TOKEN=<entry id>` in its environment. A nested
`hostler run --heavy` whose holder is a descendant of the slot holder is recognised and runs
immediately, without a second entry — so `yarn deps` → `yarn pods` → `pod install` cannot deadlock
on itself. Do not unset or forge the token; you do not need to touch it.

## Inspecting and operating the queue

```sh
hostler queue                     # capacity, running tasks (PID, held time) and waiting tasks
hostler queue --json              # the contract Envelope<QueueSnapshot>
hostler queue history             # finished tasks, newest first
hostler queue history --since 1d --limit 50
hostler queue history --json
hostler queue withdraw --id <id>  # remove your request if it has not started
hostler queue capacity set 2      # 1–16, default 1; persistent
```

Snapshots marked `[rn-looky ledger]` are running heavy tasks rn-looky recorded; they count against
capacity but hostler does not own or stop them.

`hostler queue withdraw` is safe and always available to the agent that made the request.

### `--force` is the operator's decision

`hostler queue release --id <id> --force` frees a slot while the task may still be running (the
processes are **not** stopped). Without the flag, hostler refuses with a recovery command.

Agents use `--force` — on `queue release`, `service stop`, `service restart`, `service install` or
`service uninstall` — **only with the operator's explicit permission**. Never infer it. Ask, name
the task, and wait for a yes.

## The service

The queue lives in one per-user background service. You normally never manage it: the client
starts it on demand. When you do:

```sh
hostler service status            # running/not, pid, versions, socket
hostler service status --json
hostler service start
hostler service stop              # refused while heavy tasks run unless --force
hostler service restart
hostler service install           # register with the OS so it starts at logon
hostler service uninstall
```

`install` replaces an existing registration and `uninstall` removes it; both stop the service
first and refuse while heavy tasks run unless `--force`. They are for the real home only, not a
`HOSTLER_HOME` test directory.

After upgrading the hostler binary, run `hostler service restart` so the service runs the new
binary and contract. A service with a different contract version is reported by clients as
`contract_mismatch`; its recovery command is `hostler service restart`. Also restart when
`hostler service status` shows a version or contract different from the CLI.

The service keeps queue state in SQLite, so a restart does not lose tickets: waiting clients
reattach and keep their place, and a running task releases into the restarted service.

## Agent sandboxes

The service must run outside your sandbox, because it owns the machine-wide queue and outlives any
one agent. Register it with the OS once (`hostler service install`); it then starts at logon and
clients only talk to it.

A sandboxed agent needs permission for exactly two things: running the `hostler` binary, and
connecting to the service's local socket:

- macOS: `~/Library/Application Support/io.github.dzamataev.hostler/hostler.sock`
- Linux: `~/.local/share/hostler/hostler.sock`
- with a long home path, the socket moves to `/tmp/hostler-<uid>-<hash>/hostler.sock`
- Windows: the named pipe `\\.\pipe\hostler-<user>-<hash>`

If hostler cannot start or reach its service from inside the sandbox, heavy runs fall open with a
warning and the command runs unqueued. Tell the operator; the sandbox or the service registration
needs fixing.

## JSON

Every read command takes `--json` and prints the contract's `Envelope` exactly. Use it instead of
parsing human text. Errors carry a stable `code` and, where possible, a `recovery` command.
