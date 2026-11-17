# hostler

The resource coordinator for parallel AI agents. Several agents work on one machine at once and
compete for things it has only one of — CPU during a native build, a test phone, the screen, a
test account. hostler hands those resources out in turn instead of letting the agents race.

**Status: pre-release.** The machine-wide heavy queue, per-project secrets, claims on the screen,
test accounts and devices (Android everywhere, iOS on macOS) are implemented. The desktop
app follows.

## Install from source

```sh
cargo install --path crates/hostler
```

The binary is `hostler`; it needs no daemon setup — clients start the per-user service on demand.

## Set a project up

```sh
hostler init              # preview: toolchain, app ids, the proposed hostler.toml, the steps
hostler init --write      # write hostler.toml (only when absent) and the agent-docs block
hostler doctor            # every check with its fix; --fix applies the safe ones
```

`hostler init` never rewrites an existing `hostler.toml`, never writes a user-level agent config
and never commits; it prints the person-only steps (approve the project, unlock the database,
import an env file, set an account) instead of running them, and places one marked block
(`<!-- hostler:begin -->` … `<!-- hostler:end -->`) in the project's agent docs. `hostler doctor`
exits 0 when no problem remains, 1 when one does, and 78 when it cannot run. `hostler guide init`
and `hostler guide doctor` are the full detail.

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

`hostler queue release --id <id> --force` frees a slot while its command may still run. An agent
uses --force only with the operator's explicit permission.

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

## Claims: the screen, test accounts and devices

The screen (this machine's desktop session), each project's test accounts and the registered
devices are exclusive, one workspace at a time. A claim is either a **lease** — `hostler run
--screen` / `--account` / `--device` holds the resource for one command and releases it when the
command ends — or a **hold**:

```sh
hostler hold screen                     # TTL 30 minutes: --ttl MIN, --while-parent, --while-pid PID
hostler hold account qa-user
hostler hold device pixel-9             # --boot for a shut-down AVD or simulator (headless)
hostler hold renew <id>
hostler release <id>
hostler run --screen -- ./drive-the-gui
hostler run --heavy --account qa-user -- ./integration
hostler run --device auto:android -- ./gradlew connectedAndroidTest
hostler account set qa-user --field session   # a person; the value at a hidden prompt or on stdin
```

Further runs and holds of the same workspace join its claim (accounts, devices, and the screen
under a hold); another workspace waits, with a line naming the holder, its command and your
position. `--max-wait` bounds the whole run (default 5 minutes when a claim is asked for);
`--no-wait` exits **75**; both need `--heavy` or a claim flag (otherwise exit **2**); Ctrl+C
withdraws. A run that took a resource ends with one summary line per resource.
Seizing a live holder is `--force --confirmation <screen|name|device-alias>`, which stops every
process using the resource first. An agent uses --force only with the operator's explicit permission.

Account fields arrive as `HOSTLER_ACCOUNT_NAME` and `HOSTLER_ACCOUNT_<FIELD>` and are masked like
secrets. There is no `account request` yet: a person sets accounts up. Claims belong to a workspace
(one checkout) and survive restarts and `git worktree move`. `hostler status` shows the heavy
queue, every resource and the secrets database; `hostler guide holds`, `hostler guide screen`,
`hostler guide accounts` and `hostler guide devices` are the full agent guides.

## Devices

Phones, emulators and simulators are resources too. A person registers a physical phone; agents may
add, rename and remove AVDs and simulators:

```sh
hostler devices tools --detect        # once, from the person's own shell
hostler devices list                  # registered and new physical devices; --all for more
hostler devices add SERIAL0001        # a person types the model
hostler devices add Pixel_9           # an AVD or simulator: an agent may
hostler run --device pixel-9 -- ./gradlew connectedAndroidTest
hostler hold device pixel-9           # keep it across commands
hostler hold device auto:ios:simulator --boot
hostler devices forget-sessions pixel-9   # a person, while agent-device cannot be read
```

`--device` takes an alias or id, or `auto`, `auto:<platform>`, `auto:<platform>:<kind>`; `auto`
takes this workspace's live claim or reservation, then the first free compatible device. A named
device that is not connected is waited for, never substituted. `--boot` boots an existing AVD or
simulator headless; hostler shuts it down 2 minutes after its last claim and session unless a hold
keeps it. The command gets `HOSTLER_DEVICE_ID`, `HOSTLER_DEVICE_ALIAS`, `HOSTLER_DEVICE_PLATFORM`,
`HOSTLER_LEASE_ID`, `ANDROID_SERIAL` (Android) or `HOSTLER_DEVICE_UDID` (iOS), and
`HOSTLER_AGENT_DEVICE_ID`.

An agent-device session counts as your workspace's use and never blocks it; it blocks every other
workspace until `agent-device close`. Inside a run, pass `--serial "$ANDROID_SERIAL"` or `--udid
"$HOSTLER_DEVICE_UDID"`. A run that ends with its session open prints the exact close command. A
hostler takeover closes the holder's sessions; the device alias is the `--confirmation` token, and
`--force` with `auto` exits 2. An agent uses --force only with the operator's explicit permission.

`hostler guide devices` covers onboarding, telemetry, installs, the two agent-device shapes, the
freshness gate after a claim ends, and the person-only `devices forget-sessions` escape when
agent-device cannot be read. `hostler status` shows the registered devices and a count of new ones.

## Service

```sh
hostler service status                # running, pid, versions, socket
hostler service start | stop | restart
hostler service install | uninstall   # register with launchd / systemd / Task Scheduler
```

`stop`, `restart`, `install` and `uninstall` refuse while heavy tasks or claim runs run, or a
takeover is in progress, unless `--force`. An agent uses --force only with the operator's explicit
permission. A service hostler starts itself gets a clean environment (never a run's injected
values), and `HOSTLER_HOME` must be an absolute path (a relative one is refused).

After upgrading the binary, `hostler service start` replaces an older service that has no live
work and a locked secrets database; an older client never stops a newer service, and a stop or a
restart locks the database (a person runs `hostler unlock` after it). `hostler doctor` names the
side that is old.

## Agent sandboxes

The service runs outside any agent's sandbox; a sandboxed agent needs the socket. In Claude Code,
add `sandbox.network.allowUnixSockets: ["<socket path>"]` to the settings; in Codex, install the
rule file `hostler guide sandbox` prints — Codex's `.rules` exec policy runs a matched
`hostler …` command outside the sandbox. A denial is `sandbox_denied`, never "not running", and
hostler starts nothing; a sandboxed `hostler run --heavy` then runs unqueued after one warning.

All read commands accept `--json` and print the contract's `Envelope`.

## Licence

MIT.
