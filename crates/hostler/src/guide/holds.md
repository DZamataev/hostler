# Holds and takeover

The screen, each project's test accounts and the registered devices are exclusive: one workspace
uses a resource at a time. hostler hands one out as a **claim**, and there are two kinds of claim:

- a **lease** — `hostler run --screen …`, `--account NAME` or `--device DEVICE` takes the resource
  for one command and releases it when the command ends;
- a **hold** — `hostler hold …` keeps the resource after the command that took it has ended,
  until its TTL runs out or somebody releases it.

Use a lease for one command. Use a hold when you know you need the resource for several commands,
or when you must take it now and command it in a moment; every run you start in that workspace
joins the hold instead of queueing. The device-specific rules (onboarding, `--boot`, agent-device
sessions, telemetry) are in `hostler guide devices`.

Claims belong to a workspace (one checkout). A command that names a resource from outside a git
checkout is refused (`not_in_project`): the refusal is exit 78, for `run`, `hold`, `release` and
`hold renew` alike.

## The hold commands

```sh
hostler hold screen                      # the desktop session, TTL 30 minutes
hostler hold account qa-user             # one of this project's test accounts
hostler hold device pixel-9              # a registered phone, emulator or simulator
hostler hold device auto:ios:simulator --boot   # boot it headless if it is shut down
hostler hold screen --ttl 90             # minutes; more than 0, at most 1440
hostler release <id>                     # free it now
hostler hold renew <id>                  # extend the TTL to now + the hold's TTL
hostler hold screen --json               # the contract Envelope<Claim>
```

`hostler hold` prints one line:

```
[hostler] Holding screen as <id> until 14:30. Release it with: hostler release <id>
```

A hold that joined this workspace's live claim adds ` (joined this workspace's hold <id>)`, or
` (joined this workspace's lease <id>)`.

`hostler hold device ALIAS` prints the same line with `device ALIAS`. With `--json` the claim's
resource is the catalog's current view, so after `--boot` a booted emulator's `serial` and
`agent_device_id` are its `emulator-N`.

**TTL.** The default is 30 minutes. Every `hostler hold renew <id>` and every linked run's start and
end move the expiry to `now + the hold's TTL`. When the TTL passes, the hold is released and its
history says `expired`. A hold does not expire while a claim linked to it lives.

**Process-bound holds** have no TTL and live exactly as long as one process:

```sh
hostler hold screen --while-parent      # hostler's own parent (a test runner or a shell)
hostler hold screen --while-pid 4242    # an ancestor of this hostler command
```

`--while-pid` is refused unless the pid is an ancestor of the hostler process that is not pid 1 and
not a service manager or desktop shell (`launchd`, `systemd`, `init`, `services.exe`,
`explorer.exe`, …). `--while-parent` names the direct parent. The refusal says so; it is
`bad_request`, exit 2.

## Sharing inside one workspace

A run or hold started in the workspace that already claims a resource **joins** that claim instead
of queueing:

- an **account** and a **device** are shared: two `run --account qa-user` commands (or two
  `run --device pixel-9` commands) in one workspace run at once, and a `hostler hold account
  qa-user` (or `hostler hold device pixel-9`) beside them joins them;
- the **screen** is shared only by a hold (`hostler hold screen`, then any number of
  `run --screen` under it) and by re-entrant nesting — a `hostler run` started inside a command
  that already holds the resource. A second `run --screen` beside another screen lease of the same
  workspace waits.

A joined run is its own claim with its own PID and process group, linked to the claim it joined.
That is what makes takeover and expiry exact: every process that uses the resource is known. The
resource is free for other workspaces only when every claim of this workspace on it has ended.

Joins can keep another workspace waiting: while this workspace keeps joining and renewing, an
earlier request from another workspace stays in the queue. That is FIFO, not a bug; release the
hold when the work is done.

`--distinct` never joins. It takes a resource this workspace does not claim yet — for a test
runner that needs N separate slots (e2e's `DeviceProvider` uses `--distinct --while-parent`). With
an exact selector (`screen`, `account NAME`, a device alias) for a resource this workspace already
claims it is refused at once (`bad_request`).

## Waiting

Waiting is part of the contract. A run or hold that cannot get its resource prints, on stderr:

```
[hostler] Waiting for screen: held by /work/a (hold h1, until 14:30, 2 min). You are 1 of 1. Waited 12 s of 5 min — this is waiting, not hanging; Ctrl+C withdraws the request.
```

- `{held by}` is the holding workspace's path; a foreign claim shows as `{source}: {description}`.
  Behind your own workspace's claim it reads `held by this workspace's {what} ({held})`.
- `{what}` is `run of {command}, PID {pid}`, `hold {id}, until {HH:MM}`,
  `hold {id} while PID {pid} lives`, or `being taken over by {path}`, plus `, and {n} joined
  run(s)` when the claim has linked runs; the last `{held}` is how long it has been held.
- A device with no blocker is waiting because it is not registered or not connected: `no
  registered device matches and is free (see hostler devices list; without --boot only booted
  emulators and simulators count)` or `it is not registered or not connected (see hostler devices
  list)`. A device claim that just ended is `checking for agent-device sessions on {alias}` until a
  good sessions read; see `hostler guide devices`.
- When the holder is another workspace, a second line says how to take it over:
  `[hostler] To take it over: {takeover} --force --confirmation {token}. An agent uses --force
  only with the operator's explicit permission.` For a run, `{takeover}` is `rerun with` — add
  both flags to the same command. For a hold it is the `hostler hold …` command that would take
  it (`hostler release ID` never waits, so it carries no hint of its own). A wait behind your own
  workspace's claim has no such line.
- The line repeats every 30 seconds and on every change until the resource is granted.
- `--max-wait MIN` bounds the whole run or hold: one deadline from the first wait, covering every
  claim and the heavy slot. Fractions are fine (`1.5`); `0` means no limit. The default is **5**
  minutes when any claim flag is given. `--max-wait` and `--no-wait` belong to a run that asks for
  something: they need `--heavy`, `--screen`, `--account` or `--device`, and without one they are
  a usage error (exit 2).
- `--no-wait` prints your position and exits **75** at once. On a device whose only blocker is the
  sessions check it waits up to one read bound (at most 6 seconds) before answering Busy.
- Ctrl+C or SIGTERM withdraws the request and releases what this run was already granted
  (exit 130 / 143).

## When the command ends

Every `hostler run` that took a resource ends with one stderr line per resource:

```
[hostler] screen: released.
[hostler] account qa-user: still held by your hold h1 until 14:30; hostler release h1 frees it.
[hostler] account qa-user: released (your hold h1 ended while the command ran).
[hostler] account qa-user: released; this workspace's run c2 still uses it.
[hostler] device pixel-9: released; free.
[hostler] device pixel-9: released; still reserved by this workspace (hostler devices unreserve pixel-9 frees it).
[hostler] device pixel-9: shut down in 2 min unless claimed (hostler booted it).
[hostler] screen: taken over by /work/b while the command ran.
[hostler] screen: the service did not answer; the claim ends when this process is gone.
[hostler] screen: held by the enclosing run.
```

- *still held* — this run joined a hold that is still alive;
- *your hold ended while the command ran* — the joined hold expired or was released;
- *still uses it* — this run joined another run's lease (account or device sharing);
- *still reserved* — the workspace has a `hostler devices reserve` on the device; `hostler devices
  unreserve ALIAS` frees it;
- *shut down in 2 min* — hostler booted the device; a hold or a live session keeps it;
- *taken over by {path}* — another workspace took the resource over (the history row of the seized
  claim records the taker's workspace path);
- *the service did not answer* — the release call failed after its retry budget; the claim is
  reaped once this process is gone;
- *held by the enclosing run* — this run was re-entrant: an ancestor already holds it.

## Releasing

`hostler release <id>` frees a hold of your own workspace. Another workspace's hold needs the
operator's decision (`--force --confirmation <token>`, below); hostler then takes it over, stopping
every run that uses it, and releases it, so the resource is free and the seizure is recorded. An
agent uses --force only with the operator's explicit permission.

A **lease** is ended by its own run. `hostler release <id>` without `--force` points at the command
that stops the run. `hostler release <id> --force --confirmation <token>` ends the claim **without
stopping the run** — the operator's assertion, for a run hostler cannot stop or verify (below).
It prints `[hostler] Ended claim <id> on screen without stopping its run; recorded as a forced
release.` With `--json`, that release answers `Envelope<SeizureRecord>`; every other release
answers `Envelope<ReleaseOutcome>` (`parent` names the live hold it was joined to, `seized_by` the
taker of a claim that already ended `Seized`). Releasing an already ended claim prints
`[hostler] Claim <id> was already ended.` An agent uses --force only with the operator's explicit
permission.

A device hold is released the same way; a device hostler booted is shut down 2 minutes later
unless another claim or a live session keeps it. A reservation is not a claim: `hostler devices
unreserve ALIAS` frees it.

## Takeover

A live holder's resource moves only through a recorded takeover:

```sh
hostler run --screen --force --confirmation screen -- ./drive-the-gui
hostler run --account qa-user --force --confirmation qa-user -- ./login-test
hostler run --device pixel-9 --force --confirmation pixel-9 -- ./gradlew connectedAndroidTest
hostler hold screen --force --confirmation screen
hostler hold device pixel-9 --force --confirmation pixel-9
hostler release <id> --force --confirmation screen
```

`--confirmation` must be the resource's display token: `screen`, the account's name, or the
device's alias. One `--confirmation` takes over only the resource it names; other resources the
same run asked for keep
waiting. `--force` without `--confirmation` is a usage error. An agent uses --force only with the
operator's explicit permission.

`--force` with `auto` exits 2: there is no alias to confirm, so name the device. A device takeover
also closes the holder's agent-device sessions (never the taker's own); see `hostler guide devices`.
An agent uses --force only with the operator's explicit permission.

**An agent uses --force only with the operator's explicit permission.** Ask the operator, name the
workspace and the command you would stop, and wait for a yes; never infer it.

The takeover stops **every** live process that uses the resource for the holding workspace:

- each of its leases, joined runs included (the run's process group, or its wrapper when no child
  has registered yet);
- a process-bound hold's bound process;
- a stoppable foreign claim, through its own stopper (P3b: an agent-device session);
- every nested run — on any resource, heavy entries included — whose holder descends from one of
  those (a nested run has its own process group, so the outer group's signal would miss it).

TTL holds and reservations without processes move without stopping anything. Stranded claims (the
holder is dead, or the workspace path is gone and nothing is linked to it) are reaped by the
service without a takeover.

The stops are the taker's work: Unix sends SIGTERM to the group, waits a 10-second grace, sends
SIGKILL and waits 5 seconds; Windows terminates the job at once. The claim moves only when every
target is verifiably gone; a Windows job state the service cannot see counts as unknown and blocks
the commit, so the holder keeps the resource (`seizure_failed`). Every seizure is recorded: the
resource, from whom (workspace path, kind, PID), by whom (workspace path, PID), when, whether it
was stranded, and whether it was forced.

What a takeover prints:

```
[hostler] Stopping 1 of 2: the run of claim c1 (PID 4242) …
[hostler] Took screen over from /work/a (2 process(es) stopped); recorded.
[hostler] Took screen over from /work/a (2 process(es) stopped); recorded as forced: the service could not verify that the holder stopped.
```

- the `Stopping k of n` line comes before each target is signalled; the `Stopping` names are
  `the run of claim {id} (PID {pid})`, `heavy run {entry} (PID {pid})`, `PID {pid}, the bound
  process of hold {id}`, or `{source}'s claim`;
- a commit that passed only on the taker's own evidence that a Windows job is gone is recorded as
  `forced` and says the service could not verify that the holder stopped;
- **stops are not atomic.** When target k cannot be stopped, targets 1…k−1 are already stopped and
  the refusal says so:
  `[hostler] 1 of 2 processes were stopped before PID 9 could not be; screen stays with its
  holder.` The resource stays with its holder, with fewer runs. If every target was stopped but
  the commit still refused: `[hostler] 2 of 2 processes were stopped, but the takeover was
  refused; screen stays with its holder.`
- **Ctrl+C during the stops** aborts the plan at once (the service drops it within 2 seconds): a
  target whose stop had begun was signalled, the rest were not, and the holder keeps the resource:
  `[hostler] Interrupted: 1 of 3 processes were stopped; screen stays with its holder.` When a
  stop had begun, the line adds `; the stop of {name} had begun (SIGTERM sent), so it may still
  end` on Unix, or `(its job was being terminated)` instead of `(SIGTERM sent)` on Windows. The
  exit is 130 (143 for SIGTERM). The holder's command may be gone even though its claim stays
  until it releases or is reaped.

While a takeover is in progress the resource is `seizing`: nothing is granted on it and a new
request queues behind the plan. A taker never targets itself: a `run --force` whose own ancestry
contains a target is refused rather than signalling itself, and a nested run refused because the
run it comes from is being taken over prints
`[hostler] The run this command comes from is being taken over; not running.` and exits 78; that
is `taken_over`, not a service fault. An agent uses --force only with the operator's explicit
permission.

### Limits of takeover

- A process that escaped its group or job (`setsid`, a Windows breakaway job) is not tracked and
  not stopped.
- On Unix, a group whose members run with mixed privileges is partly killed: the remainder makes
  the stop `StillAlive` and the holder keeps the resource.
- On Windows a run in **another logon session** cannot be opened or terminated from here: the
  takeover refuses with `seizure_failed` naming the session, and nothing moves. A person can end a
  stuck claim with `hostler release <id> --force --confirmation <token>`: it records a forced
  release without stopping anything. An agent uses --force only with the operator's explicit
  permission.
- Windows job liveness is the service's best evidence, not a guarantee. An unopenable job counts
  as dead for reaping, so a queue never sticks: a lease whose wrapper and direct child died in
  another logon session is reaped while job members still run, and the resource can be granted
  again while they live. For a takeover commit the same state is `Unknown` and blocks instead.
  A wrapper that died while the service was down leaves no job name once its last handle closed,
  so its job reads as gone once the launcher has exited.
- A workspace path that cannot be read (an unmounted volume, a hanging mount) counts as existing:
  its claim is not stranded and is not reaped. A path known to be gone strands only claims with
  no live process — a TTL hold or a reservation — and only after the path has been gone for two
  minutes in a row (a moved checkout re-resolves first). A live lease, or a process-bound hold
  whose process lives, is never stranded by its path.

## Deadlocks

Waiting requests that can never be granted are refused instead of hanging. The engine computes the
wait-for cycles across claims and the heavy queue. Only a **victim** is refused — a request whose
processes hold something the cycle waits for; requests that merely queue behind the cycle keep
waiting. The refusal is `claim_deadlock`, names the cycle, and its recovery is `hostler status`.
`run` and `hold` both exit **78**. Its message ends:
`…; if a nested run was started in the background, take every resource in the outer run`.

**Known false positive:** a parent that starts a nested `hostler run` **in the background** and
goes on with its own work is counted as waiting for it, so a cycle can be reported while the parent
would have finished anyway. The refusal says so, and the workaround is in the message: take every
resource in the outer run, so the nested run needs nothing new.

## Workspaces

Every checkout has a stable workspace id in its own git dir: `<git dir>/hostler-workspace`, written
only by the service. Claims belong to that id, so they survive a restart and a `git worktree move`.

- A **copied** checkout (a `cp -R`) is a new, unapproved workspace: it gets a new id and its own
  project. A copied worktree, which shares the original's git dir, is refused until one of them is
  repaired (`git worktree repair`).
- A **moved** main checkout keeps its id, and its project follows it: the approval and the KeePass
  group link stay. A different repository at the old path is a new, unapproved project.
- A project created before P3a has no stored filesystem identity yet; it gets one at its next
  resolve. Until then it binds to whatever directory sits at its recorded path at the first command
  after the upgrade — move a repository before running anything in its old place.

`hostler workspace` prints this checkout's id, path and project; `hostler workspace list` prints
every recorded workspace; `hostler status` includes this checkout's id and path when it is
registered, and never registers it itself.

## Exit and error codes

`hold` and `release` exit like `run`: 0 granted or released; **75** timed out, `--no-wait`,
withdrawn by an operator, or `queue_busy`; **78** every claim refusal — a refused takeover, a
deadlock, a missing or locked account, a project not approved, not in a git checkout, a claim this
workspace does not hold, a takeover that needs the operator's decision, no service at all (fail
closed, as for `run`), or a service of another contract version (`contract_mismatch`); **2** a usage error; 130/143 interrupted; 1 anything else (an internal
failure; a disconnected service is 78). `run` with a claim flag: the child's code; 78 refused
before running; 75 queue; 130/143 interrupted; 127 cannot run. `hostler devices` commands use
0/1/2/78 only: 2 for a usage problem, 78 for every device refusal (an unknown or unregistered
device, a not-booted device, an alias taken, a failed boot, a missing tool, a person required, no
service, a contract mismatch), 1 otherwise.

Errors print `[hostler] <message>` and `[hostler] To fix: <command>`. In `--json`, `code` is
stable and `recovery`, when present, holds that command. The table says what to do: the `recovery`
command where the code has one, otherwise the human step.

| Code | When | What to do |
|---|---|---|
| `service_unavailable` | The service could not be reached or started; every claim command fails closed. | `hostler service start` |
| `not_in_project` | The directory is not a git checkout. | run from the project |
| `unknown_resource` | Not `screen`, an account of this project, or a registered device. | `hostler status` |
| `account_missing` | `--account`/`hold account` names an account this project does not have. | `hostler account set NAME --field FIELD` (a person) |
| `secrets_locked` | The accounts live in the secrets database and it is locked or missing. | `hostler unlock` (a person) |
| `project_not_approved` | This checkout is not approved for the secrets database. | `hostler project approve` (a person) |
| `resource_in_use` | `account remove` while the account is claimed, or `devices remove` while the device is claimed or waited for. | `hostler status` |
| `device_not_found` | No known device matches the alias or id. | `hostler devices list --all` |
| `device_not_registered` | A named device is not registered. | physical: `ask the operator to register it: hostler devices add {name} (a person types the model)`; virtual: `hostler devices add {name}` |
| `device_not_ready` | The device is not booted. | `rerun with --boot` |
| `alias_taken` | Another device holds the alias, ignoring case. | `choose another alias, or: hostler devices rename {holder} <other>` |
| `boot_failed` | A `--boot` did not finish. | `hostler devices show {alias}; emulator output: {log path}` |
| `tool_missing` | adb or agent-device cannot be found. | `hostler devices tools --detect`, or the agent-device install/path fix in `hostler guide devices` |
| `permission_denied` / `not_interactive` | A physical `add`/`rename`/`remove`, or a `forget-sessions`, without a person at a terminal. | the command it prints (a person runs it) |
| `force_required` | The resource is another workspace's; its recovery is `null` and the message names the takeover command. An agent uses --force only with the operator's explicit permission. | the command it prints |
| `claim_not_held` | The claim id is not this workspace's, or it was seized or reaped. | `hostler status` |
| `claim_deadlock` | The wait would never end (above). | take every resource in the outer run |
| `seizure_failed` | A takeover target could not be stopped or verified; the holder keeps the resource. | `hostler status`; a person may force-release the claim |
| `taken_over` | The run this command comes from is being taken over; nothing ran. | not a fault; ask the taker |
| `queue_timeout` | A hold's `--max-wait` ran out; the request was withdrawn. | retry, or `hostler status` for the holder |
| `queue_busy` | A `hold --no-wait` that could not get the resource, or 512 claims and waiting requests live on this machine. | `hostler status`; wait or release some |
| `withdrawn` | The request was withdrawn by an operator, or interrupted. | — |
| `bad_request` | A usage problem (a bad `--confirmation`, `--while-pid`, `--distinct` on a claim you hold). | fix the call |
| `contract_mismatch` | The running service speaks another contract version. | `hostler service restart` |

## JSON

Every read command takes `--json` and prints the contract's `Envelope` exactly; use it instead of
parsing human text. `hostler hold` and `hostler release` print `Envelope<Claim>` and
`Envelope<ReleaseOutcome>` (`Envelope<SeizureRecord>` for a forced lease release). `hostler hold
device` answers `Envelope<Claim>`; the `hostler devices` envelopes are listed in `hostler guide
devices`.
