# Checking hostler: hostler doctor

`hostler doctor` checks this machine, the service, and — inside a git checkout — that project. It
changes nothing without `--fix`.

```sh
hostler doctor          # one line per check, its fix, and who acts
hostler doctor --fix    # the safe fixes, each followed by a re-check
hostler doctor --json   # Envelope<DoctorReport>
```

The human output is one line per check, `ok  |warn|FAIL|skip  <id>  <message>`, then the fix:
`  fix: <command> (doctor --fix runs it)` for the agent, or `  for the person: …` (a hidden
prompt's command and text). Then what `--fix` did — `fixed:`, `still failing:` or `refused:` — and
the summary `N problems, M warnings; K fixed`.

Exit codes: **0** no problem remains (warnings do not count), **1** at least one problem remains,
**78** doctor could not run at all (no home it can use); with `--json` that is an error envelope.

Doctor reads only: it never registers a checkout, never reads an env file, and never writes a
project file except the hostler block with `--fix`.

## The checks

| id | What it checks | A problem means |
|---|---|---|
| `service.registered` | The OS registration exists and is current. | Install it: a service an agent starts runs inside the agent's sandbox and ends with it, and the older registration lacks the current definition. |
| `service.reachable` | The service answers, and how it fails when it does not. | It is not running (safe fix: `hostler service start`), it is `sandbox_denied` (a fix for the person), or it does not answer (see `hostler guide recovery`). |
| `service.contract` | The running service's contract against this hostler's. | A version difference, with the direction in `details.direction`; nothing is stopped here or by `--fix` unless the rule below allows it. |
| `service.binary` | The running service's binary version, and the binary the OS registration runs. | This hostler is older than the service, or the registration runs another binary (`hostler service install` is the person's), or the service is older and may be restarted. |
| `service.linux_session` | Linux only: `KillUserProcesses` and linger, against the registration. | With logind ending user processes and no registration, the service ends with the session (install it); without linger, it stops at your last logout (a person runs `loginctl enable-linger <user>`). |
| `secrets.database` | The secrets database's state. | Locked or missing: a person unlocks it (`hostler unlock`). |
| `project.config` | `hostler.toml` parses and validates (this project). | Absent, unreadable, or invalid; a TODO purpose is a warning. |
| `project.config.<line>` | One problem of `hostler.toml`, by line; the message never quotes the line. | Fix that line by hand: init never rewrites an existing file. |
| `project.approved` | The project is approved for its secrets. | A person runs `hostler project approve`. |
| `project.docs` | The hostler block is current in the agent docs. | Missing or out of date (safe fix: the block only), broken markers or a symlink (fix by hand; doctor never writes through a symlink). |
| `project.device_rules` | `[agents] device_rules` points at a heading that exists. | The prompt and the setting are `init.device_rules`' work. |
| `secrets.required.<NAME>` | A declared secret has a value. | Ask the person with `hostler secret request <NAME> --reason <purpose>`; the person's own route is in `details.person_fix`. |
| `accounts.required.<NAME>` | A declared account exists with all its fields. | A person sets each field with `hostler account set <NAME> --field <field>`. |
| `tools.adb` | adb was found, and no foreign adb server holds the device bridge. | `hostler devices tools --detect`, or stop the foreign adb server. |
| `tools.emulator` | The Android emulator binary was found. | `hostler devices tools --detect`; skipped when no Android app is declared. |
| `tools.xcrun` | macOS: Xcode's `xcrun` is installed. | iOS simulators are not seen: a person runs `xcode-select --install`. |
| `tools.ideviceinfo` | `ideviceinfo` (libimobiledevice) was found. | Physical iPhones are not seen: `hostler devices tools --detect` after installing it. |
| `tools.agent_device` | agent-device's version against the one hostler targets. | Older than the target: upgrade it. |
| `tools.problem.<n>` | One line of the device tools' own problem list. | Do what it says, then `hostler devices tools`. |
| `claims.workspace` | This workspace's TTL holds older than 2 hours, and this project's reservations whose checkout path is gone. | `hostler release <id>` for each one, only when no live agent of the workspace still uses it. |
| `workspace.registered` | This checkout has a workspace id. | Not registered yet (a warning): it is registered on its first claim. |

Every check that cannot run — no service, no checkout, an unreadable registration, another OS —
reports `skip` with the reason, rather than guessing.

## What `--fix` runs

In check order, once each, in-process (never through a shell):

- `hostler service install` for `service.registered` and `service.linux_session` (registration
  maintenance), and for `service.contract` when this hostler is newer, in the default home;
- `hostler service start` for `service.reachable`, and for `service.contract` under a custom
  `HOSTLER_HOME`;
- `hostler service restart` for `service.binary`, when this hostler is newer and the registration
  runs this binary (or there is no registration: a custom `HOSTLER_HOME`, or none installed). When
  the binary the registration runs cannot be read, a restart could start the older one again, so
  doctor restarts nothing and `hostler service install` is the person's;
- the hostler block for `project.docs`, and only the block: never `hostler.toml`.

Each fix is re-checked: the report says `fixed`, `still failing` or `refused`, and the summary
counts the fixed ones. A fix already made moot by an earlier one is not run.

## What `--fix` never does

- It never forces anything. Every stop goes probe → the rule → one `service.shutdown` with
  `if_idle: true, refuse_if_unlocked: true`; a `ServiceBusy` is a refusal, never retried with less.
- It never stops a service that is newer than this hostler, whose version cannot be compared, or
  whose secrets database is unlocked (or cannot say, as on an older service). An unlocked database
  is a person's live work.
- It never registers over a registration it cannot read, and never registers a development build:
  for a development build the install is listed as a step to type (`hostler service install`),
  never as one `--fix` runs.
- It never runs a person's step: no `project approve`, no `unlock`, no `secret set` or
  `secret import`, no `account set`, no `loginctl enable-linger`.
- It never releases a claim, never writes `hostler.toml`, never reads an env file, and never
  writes through a symlink or outside the checkout.
- It never takes a resource from another workspace.

## The 20 second bound

The whole command, `--fix` included, has one 20 second deadline. A fix that changes the service or
its registration starts only with 8 seconds left, and then runs to its own bounded end — so doctor
exceeds the 20 seconds only by that one change in flight, never by a lookup left hanging. A fix
that ran out of time is reported as `still failing` or `refused`, with the reason.

## When doctor reports a sandbox denial

`service.reachable` is `FAIL` with `details.reason = "sandbox_denied"`, names the directory when
there is one, and carries a fix for the person: allow hostler's socket in the agent's sandbox, or
run hostler outside the sandbox. `hostler guide sandbox` has both clients' settings.
