# Devices

Phones, emulators and simulators are resources like the screen: hostler discovers them, lets a
person register a physical one, and hands each out as a **claim**. Android works on every platform,
iOS on macOS.

hostler only **reads** from a device. Its only state-changing calls are booting an existing AVD or
simulator for `--boot`, shutting down one hostler booted itself after an identity check, and closing
an agent-device session in a takeover the operator permitted. It never installs, uninstalls, clears,
reboots or locks a device, changes its settings, creates or deletes an AVD or simulator, or runs
`adb tcpip|usb|root|remount|reboot|kill-server|pair|connect`.

## Onboarding: find the tools

Run this once from the person's own shell, not an agent's (an agent's `PATH` may differ):

```sh
hostler devices tools --detect
```

It records the **absolute** agent-device launcher, its package script, Node, and the caller's
`ANDROID_HOME`, `ANDROID_SDK_ROOT`, `ANDROID_SDK_HOME`, `ANDROID_USER_HOME`, `ANDROID_AVD_HOME`,
`ANDROID_EMULATOR_HOME`, `ANDROID_ADB_SERVER_PORT` and `ADB_SERVER_SOCKET` as settings. It prefers
global installs: PATH entries inside a registered workspace or ending in `node_modules/.bin` are
skipped and it says so (`[hostler] skipped … (project-local): prefers global installs`); relative
entries are ignored. Volta, asdf, fnm and nvm installs are found. A path-valued Android variable
must be absolute; a relative one is ignored with `[hostler] ignored ANDROID_HOME=… (not an absolute
path)`, and a name that is now unset clears the earlier setting. The values it recorded are printed
on stderr (`[hostler] recorded agent-device: …`).

`--adb PATH`, `--agent-device PATH` and `--node PATH` override the search; relative paths are made
absolute first. An `--agent-device` path that does not lead to the package script is refused
(exit 2) with the fix: `install it globally (npm i -g agent-device@0.21.36), or point at a project's
copy: hostler devices tools --agent-device
<checkout>/node_modules/agent-device/bin/agent-device.mjs`. A project-local copy needs that explicit
flag and prints `[hostler] warning: this path goes away with its checkout`; the warning is not
printed for a path outside a checkout and outside `node_modules`. `hostler devices tools` with no flag is a read of the current status and
changes nothing.

A missing adb or agent-device is a `problem:` in `devices list` and `devices tools` (the contract's
`tool_missing`), with the fix: `hostler devices tools --detect, or set ANDROID_HOME` for adb, and
the install/path fix above for agent-device.

## Listing and registering

```sh
hostler devices list                    # registered devices and new physical ones
hostler devices list --all              # also unregistered AVDs/simulators and unidentified transports
hostler devices show pixel-9            # facts, apps, sessions, forget records
hostler devices add SERIAL0001          # a person; types the model
hostler devices add Pixel_9 --alias avd-9
hostler devices rename pixel-9 left
hostler devices note pixel-9 "screen locked"
hostler devices remove pixel-9
hostler devices reserve pixel-9         # this workspace, across days
hostler devices unreserve pixel-9
hostler devices installs --since 1d
```

`list` shows one line per registered device:

```
pixel-9 — Pixel 9 (android physical): usb; 86% charging · wifi (lab) · vpn; free
```

The parts are the alias, model, platform/kind, connection (`usb`, `wifi`, `wired`, `network`,
`booted`, `booting`, `shut down`, `not connected`, `shutting down`), telemetry, and the standing:
`free`, `held by {path} ({what})`, `reserved by {path}`, `testing in progress: agent-device session
{s} in {workspace}`, `checking for agent-device sessions`, `closed until agent-device can be read
({reason})`, `being taken over by {path}`, or `N waiting`.

**New physical devices are shown by default**, marked `new: a person registers it with hostler
devices add SERIAL0001`; an unregistered AVD or simulator appears only with `--all`, marked `new:
hostler devices add Pixel_9`. An unauthorized or offline phone is an unidentified transport: it
appears only with `--all`, with its state and hint (`accept the prompt on the phone`); without
`--all`, one line says `N transport(s) cannot be identified: hostler devices list --all`. Tool
problems and `agent-device sessions unknown: …` are printed last as `problem: …`.

**A person registers a phone.** `hostler devices add`, `rename` and `remove` of a physical device
need a person at a terminal: they print the model, transport and OS, then ask the person to type
the model. Without a terminal the refusal is exit 78, with the runnable command as its recovery
and `A person runs this; an agent asks the operator.` **Agents may add, rename and remove AVDs and
simulators themselves.** A person types the model; an agent asks the operator.

An alias matches `^[a-z0-9][a-z0-9._-]{0,31}$` and is unique ignoring case. The default is the
model, AVD or simulator name slugged (`pixel-9`, `pixel_9`, `iphone-17-pro`), with `-2`, `-3` … on
a collision. An alias never changes by itself once registered: it is the `--confirmation` token and
appears in scripts. `remove` clears it. An alias that looks like a device id or transport
(`emulator-5554`, `host:5555`, a serial) is refused.

`remove` removes the device from the registry only; it touches nothing on the device and is refused
(`resource_in_use`) while a claim or a waiting request is on it. `devices note` stores a fact about
the hardware; `devices show` prints it. `devices show` also lists the device's `forget-sessions`
records.

`devices reserve` gives this workspace precedence and blocks other workspaces; it is advisory
toward tools that do not go through hostler, and it is not a claim. `auto` prefers the workspace's
reservation before the first free device.

## Using a device

Take a device for one command, or keep it:

```sh
hostler run --device pixel-9 -- ./gradlew connectedAndroidTest
hostler run --device auto:android -- adb shell getprop ro.product.model
hostler hold device pixel-9             # keep it after the command; TTL 30 minutes
hostler hold device auto:ios:simulator --boot
```

`--device` takes an alias or id, or `auto`, `auto:<platform>`, `auto:<platform>:<kind>`:

- `auto` / `auto:android` / `auto:ios` — any free device of that platform;
- `auto:android:physical`, `auto:android:emulator`, `auto:ios:physical`, `auto:ios:simulator` — a
  kind. `auto:android:simulator` and `auto:ios:emulator` are usage errors.

`auto` takes the workspace's live lease or hold on a candidate, then its reservation, then the
first free compatible device. A **named** device that is held elsewhere, or not connected, is
waited for, never substituted. One `--device` per run; it composes with `--screen`, `--account`,
`--heavy` and `--secret*`.

`--boot` boots an existing AVD or simulator **headless** when it is not running; it never boots a
physical device. A granted device that is not ready without `--boot` is released and refused:
`device_not_ready` (`{alias} is not booted`), recovery `rerun with --boot`, exit **78**. A named
device that loses its transport at spawn is waited for 5 seconds, then refused with `{alias} is not
connected now; the command was not run` (recovery `hostler devices list (is {alias} attached?)`,
exit **78**).

### What the command gets

```sh
HOSTLER_DEVICE_ID          # the hardware id: ro.serialno, the AVD name, or the UDID
HOSTLER_DEVICE_ALIAS
HOSTLER_DEVICE_PLATFORM    # android | ios
HOSTLER_LEASE_ID           # the claim id
ANDROID_SERIAL             # Android: the transport now (USB before Wi-Fi)
HOSTLER_DEVICE_UDID        # iOS
HOSTLER_AGENT_DEVICE_ID    # the adb transport serial or the UDID
```

`ANDROID_SERIAL` and the `HOSTLER_DEVICE_*` names are set from a fresh resolve right before the
spawn, so a USB → Wi-Fi switch during the run is followed. They are reserved: `--secret` and
`--account` refuse an injection under `ANDROID_SERIAL`, any Android SDK name, or any `HOSTLER_*`
name.

### When the run ends

Every `run --device` ends with one device line:

```
[hostler] device pixel-9: released; free.
[hostler] device pixel-9: released; still reserved by this workspace (hostler devices unreserve pixel-9 frees it).
[hostler] device pixel-9: released; this workspace's run c2 still uses it.
[hostler] device pixel-9: still held by your hold h1 until 14:30; hostler release h1 frees it.
[hostler] device pixel-9: taken over by /work/b while the command ran.
[hostler] device pixel-9: held by the enclosing run.
[hostler] device pixel-9: shut down in 2 min unless claimed (hostler booted it).
```

A hostler-booted emulator or simulator is shut down **2 minutes after its last claim and session**,
counted only while agent-device's status reads fine: while it cannot be read, or before the first
read after a service start, a session may live on it, so it is not shut down.
That holds even when a person attaches Android Studio or opens it in Simulator.app, because the
device is still hostler's. To keep it, use `hostler hold device ALIAS`; `hostler devices unreserve
ALIAS` frees a reservation. Ctrl+C during a boot releases the claim; the service finishes the boot
and the shutdown rule cleans it up.

## agent-device sessions

agent-device takes a device by flags only. Inside a hostler run it needs the exported id:

```sh
agent-device open com.example.app --serial "$ANDROID_SERIAL"      # Android
agent-device open com.example.app --udid "$HOSTLER_DEVICE_UDID"   # iOS
```

`ANDROID_SERIAL` alone is ambiguous with several devices; pass `--serial`.

There are two supported shapes:

1. **A hold around the whole session:**

   ```sh
   hostler hold device pixel-9
   agent-device open com.example.app --serial "$ANDROID_SERIAL"
   # … drive the app …
   agent-device close --session <session> --platform android --serial "$ANDROID_SERIAL"
   hostler release <hold-id>
   ```

2. **One hostler run per command:**

   ```sh
   hostler run --device pixel-9 -- agent-device open com.example.app --serial "$ANDROID_SERIAL"
   ```

A live session counts as **your workspace's** use of the device. It never blocks your workspace;
it blocks every other workspace until it is closed. An agent-device session opened outside hostler
shows up in hostler's view within about 5 seconds, so hold the device before opening one.
agent-device lets **one session hold a device**, so close yours before another session or an e2e
slot needs it. A `--distinct` hold skips a device that carries your own session; it never joins,
and with an exact selector on a device your workspace already claims it is refused.

At the end of a `run --device`, a release, or an expired or reaped hold, every live session that is
yours or was first seen inside that claim's window prints:

```
[hostler] agent-device session s1 is still open on pixel-9: it blocks other workspaces until closed: agent-device close --session s1 --platform android --serial SERIAL0001
```

The run end and `release` print it on stderr; an expiry or reap writes it to the log and the claim's
history. When no good status read that started after the claim's end completes in time (the status
cannot be read, or the read is late), the end first prints `agent-device sessions unknown:
{reason}`, then the lines of the last sessions known.

### After a claim ends

A device claim that has just ended is not granted to **another** workspace until hostler has
completed a good sessions read that started after the end. Until then the waiting line says
`checking for agent-device sessions on {alias}`; the ending workspace itself is not delayed.
`--no-wait` on that blocker waits up to one read bound (at most 6 seconds) before answering Busy.
`--force` cannot end this wait. An agent uses --force only with the operator's explicit permission.

### Testing in progress and takeovers

A device with another workspace's live session shows `testing in progress: agent-device session {s}
in {workspace}` in `devices list` and `hostler status`. Taking such a device over with `--force
--confirmation <alias>` stops the holder's runs **and closes the holder's agent-device sessions**
(never the taker's own), then moves the claim. The takeover needs the operator's decision. An agent
uses --force only with the operator's explicit permission. A person may also use the app's button.
Every seizure is recorded (who, from whom, when).

### When agent-device cannot run

When `~/.agent-device/device-claims` exists but agent-device cannot be read, hostler fails closed:
a device that had a live session at the last good read stays blocked, and `devices list`
and `hostler status` show `testing in progress: agent-device session {s} in {workspace} (last known
{age} ago; agent-device status unreadable: {reason})`; a device that had none stays grantable. After
30 seconds every device grant line and `hostler status` say `agent-device sessions unknown:
{reason}`. A device stays closed to other workspaces after a claim end and after a service start
until a **good** read completes.

Fix the path with `hostler devices tools --detect`, or `hostler devices tools --agent-device
<path>`; volta, asdf, fnm and nvm installs are found. If agent-device is no longer used on this
machine, remove `~/.agent-device/device-claims` (a person's decision). While the status is
unreadable, the checking line names the fix:

```
checking for agent-device sessions on pixel-9 (agent-device status unreadable: node not found; fix: hostler devices tools --detect, or hostler devices tools --agent-device <path>; a person may release this device: hostler devices forget-sessions pixel-9)
```

**A person can release one device** from this wait:

```sh
hostler devices forget-sessions pixel-9
```

It is person-only: it prints the kept sessions and the checking state with their ages, and asks the
person to type the alias. It applies only while the status is unreadable; otherwise it refuses with
`nothing to forget: agent-device status reads fine`, or `nothing to forget: {alias} is not closed`
when the device is not closed. It **stops nothing** and closes no session; it marks the device's
last end as checked and drops its kept sessions for that device only. It is recorded (who, when,
which sessions) and `devices show` lists it. The next claim end or good read applies the normal
rules again. `--force` does not override the sessions wait. An agent uses --force only with
the operator's explicit permission.

**The old-daemon trap.** A still-running agent-device 0.21.0 daemon blocks the 0.21.36 CLI in the
default state dir (`Daemon registration ownership could not be verified …`). Stop that daemon, or
run it in a separate `AGENT_DEVICE_STATE_DIR`. Target **0.21.36** or newer; an older version only
warns, and whatever `device status --json` returns is parsed.

## Telemetry and installed apps

`devices list`, `devices show` and `--json` show battery level, charging, Wi-Fi (and its name),
mobile data and VPN. **Charging means connected to power** (Android `status` 2, or 4 or 5 while
plugged in; iOS `ExternalConnected` or `BatteryIsCharging`), so a full phone on power reads
`charging`. A reading hostler cannot get is `unknown`, never guessed. An emulator shows `emulated`,
never a real reading. Telemetry is read every minute for an Android device and every 2 minutes for
an iOS one. A claimed device is left alone for the first 30 seconds of the claim and then read
every 5 minutes, so a test is not disturbed.

iOS battery comes from `ideviceinfo` only for a **registered, wired, devicectl-paired** phone that
has a lockdown pair record on this host. Without one the battery is `unknown` with the hint `pair it
in Finder or Xcode`; hostler never pairs, and never reads a pair record. iOS network is `unknown`;
simulators are `unknown`. **Wi-Fi names** appear in `devices list`, `devices show` and `--json` only:
never in `hostler status`, a log, history or an error.

Installed apps come from the last snapshot. Android times are the device's own exact
`firstInstallTime` / `lastUpdateTime`. iOS has no install time, so it is labelled `first seen`,
`changed between {t1} and {t2}`, or `around {claim}` — never a guessed time. A device's apps are
read every 30 minutes (every 10 minutes for a claimed iOS device), when a claim on an iOS device
opens, and when any claim on a device ends. A failed read at a claim's start or end is retried after
10 seconds, then less often, at most every 5 minutes; an end read is retried only while it can
still count as that claim's, 30 seconds. A change that happened
under a claim is attributed to it with the workspace path, branch, commit and command; a change
outside hostler says `not through hostler`. `hostler devices installs [DEVICE] [--since 1d] [--limit
N]` lists them. A simulator that a `--boot` claim starts gets its claim-open snapshot once it is
booted; an iOS change in the moments before that snapshot reads `around {claim}`.

## What hostler never does

It never installs, uninstalls, clears app data, reboots, locks, changes settings, or creates or
deletes an AVD or simulator. It never runs `adb tcpip|usb|root|remount|reboot|kill-server|pair` or
`adb connect`. Every tool call is matched against an exact argument allowlist; the `adb shell` calls
are hostler's own read-only scripts. Booting and shutting down only what it booted, and closing a
session in a permitted takeover, are the only writes.

## Platforms and the service

macOS has Android and iOS; Linux and Windows have Android only. On Windows the person prompt reads
from the console, and an OpenSSH session without a console is refused. `--device` works on every
platform.

On Linux the systemd unit has `KillMode=process`, so `hostler service restart` leaves the adb server
and hostler-booted emulators running. An older unit without that line is still current and serves;
`hostler service status` then prints a note, and `hostler service install` rewrites it once (the
install stops the service again under the old unit, which stops them too). The adb server and
hostler-booted emulators outlive a service restart on every platform.

**Another adb version owning the server.** hostler asks the adb server its version over the socket
before it runs any adb client, and never kills another version's server. When another version owns
it, adb is `foreign` and no adb call runs: `another adb (version N) owns the adb server; point
hostler at it: hostler devices tools --adb <path>`. hostler asks the server again every 10 s, and at
once on `hostler devices tools` or `devices list`, so it notices within seconds once the other
server is gone and uses adb again.

## Status

`hostler status` lists every **registered** device with its connection, telemetry and standing; an
unregistered physical device is not a resource line, but one line says `1 new device: hostler
devices list shows it` (or `N new devices: hostler devices list shows them`). Tool problems and `agent-device sessions unknown: …` come last as
`problem: …`. `hostler status --json` stays the contract's `Envelope<Overview>` and carries no
device fields; read devices with `hostler devices list --json`.

## Exit codes, errors and JSON

`hostler devices` commands use **0** (ok), **1** (a prompt answered wrongly; `error.code` is
`bad_request`), **2** (usage, a relative tool path, `nothing to forget …`) and **78** (every device
refusal, the terminal refusal, service unavailable, contract mismatch). A `run`/`hold device`
refusal is **78**; a queue outcome **75**; a signal 130/143; a usage error 2.

| Code | When | What to do |
|---|---|---|
| `device_not_found` | No known device matches the alias or id. | `hostler devices list --all` |
| `device_not_registered` | A named device is not registered. | physical: `ask the operator to register it: hostler devices add {name} (a person types the model)`; virtual: `hostler devices add {name}` |
| `device_not_ready` | The device is not booted. | `rerun with --boot` |
| `alias_taken` | Another device holds the alias, ignoring case. | `choose another alias, or: hostler devices rename {holder} <other>` |
| `boot_failed` | A `--boot` did not finish. | `hostler devices show {alias}; emulator output: {log path}` |
| `tool_missing` | The tools status reports a missing adb or agent-device. | adb: `hostler devices tools --detect, or set ANDROID_HOME`; agent-device: `install it globally (npm i -g agent-device@0.21.36), or point at a project's copy: hostler devices tools --agent-device <checkout>/node_modules/agent-device/bin/agent-device.mjs` |
| `permission_denied` / `not_interactive` | A physical `add`/`rename`/`remove`, or a `forget-sessions`, without a person at a terminal. | the command it prints (a person runs it) |
| `resource_in_use` | `remove` while the device is claimed or waited for. | `hostler status` |
| `force_required` | A takeover needs the operator's decision. | the takeover command it prints. An agent uses --force only with the operator's explicit permission. |
| `service_unavailable` | No service; device claims fail closed. | `hostler service start` |
| `contract_mismatch` | The running service speaks another contract version. | `hostler service start` (replaces an idle, locked older service), or the newer binary the error names |

Every read has `--json` = the contract's `Envelope<…>`: `devices list` → `DevicesSnapshot`, `show`
→ `DeviceDetail` (including `forgets`), `add`/`rename`/`note` → `DeviceInfo`, `remove` → the
service's `{removed}` answer, `reserve` → `Claim`, `unreserve` → `()` (data `null`), `installs` →
`[InstallEvent]`, `forget-sessions` → `ForgetRecord`, `tools` → `ToolStatus`; `hold device` →
`Envelope<Claim>`. `run` prints no envelope.

`--force` with `auto` exits 2: there is no alias to confirm, so name the device; `--confirmation` is the device's alias. An agent uses
--force only with the operator's explicit permission.

The e2e provider uses `hostler hold device auto:<platform> --distinct --while-parent`; see
`integrations/e2e/README.md`.
