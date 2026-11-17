# Recovery: error codes

Every hostler error is one object: a stable `code` (snake_case on the wire), a `message` written
for a person, `details`, and a `recovery` command when a command resolves it. Where a command takes
`--json`, it prints the same object in the envelope's `error` field. Run the recovery command; when
there is none, the message says who acts — often a person.

Exit codes are separate from codes: **2** a usage problem, **75** the queue or a wait gave up,
**78** a run, claim or device command refused (or hostler could not set itself up), **130** Ctrl+C,
and **1** anything else. One command has its own: `hostler service status` exits **3** when the
service is not running (its `--json` error is `service_unavailable` with `details.reason =
"not_running"`), **0** when it runs, and **1** for any other problem, `sandbox_denied` and an
outdated registration included.

## `service_unavailable` and its reasons

The client could not reach or start the service. `details.reason` says why, and every such error
carries its own recovery:

| `details.reason` | When it happens | The recovery the error carries |
|---|---|---|
| `not_running` | Nothing listens on the socket, or its path has no metadata. | `hostler service start` |
| `sandbox_denied` | This process may not reach the socket, or search a directory on its path: an agent sandbox, or a permissions problem. `details.dir` names the directory when known. | `allow hostler's socket in the agent's sandbox, or run hostler outside the sandbox; if no sandbox is involved, check the permissions of <dir>; see: hostler guide sandbox` |
| `timeout` | The connect, or a call, did not finish in time. | `hostler service start`; doctor's own calls carry no recovery, and their reason becomes a check message |
| `stopping` | The service is shutting down. | `hostler service start` |
| `io` | A read or a write on the connection failed. | `hostler service start` |
| `closed` | The service closed the connection. | `hostler service start` |
| `protocol` | An unreadable frame from the service, or an unreadable `service.status` answer. | `hostler service start`; for the status answer no recovery, and `hostler doctor` names the versions |
| `bad_socket_name` | The socket's path cannot be used as a socket name. | `hostler service start` |
| `foreign_owner` | The socket belongs to another user, and hostler refuses to use it. | `hostler service start` (check `HOSTLER_HOME` first) |
| `no_executable` | hostler cannot find its own binary. | `hostler service start` (run hostler from an installed path) |
| `spawn_failed` | Starting the service process failed. | `hostler service start` |
| `start_timeout` | No service answered within 5 seconds of a start. | `hostler service start`; `hostler service install` when the OS manager did the start (the registration may be from an older hostler) |
| `manager_failed` | The OS service manager check ended without an answer. | `hostler service start` |
| `manager_timeout` | The OS service manager did not answer within 5 seconds. | `hostler service status` |
| `manager_start_failed` | The OS service manager could not start the service. | `hostler service status` |
| `registration_outdated` | The OS registration is from an older hostler: it lacks `--managed` (and on Windows the headless conhost), so that service steps aside and never serves. | `hostler service install` |

The third column is the `recovery` string the error carries, so it can be run as printed. `hostler
service status` shows whether a service runs, its versions and its registration.

## `contract_mismatch`

The CLI and the running service speak different contract versions. `details` carries
`client_contract`, `service_contract`, `service_version`, and `direction` — this client against
the service:

- **`newer`**: this hostler is newer than the service. `hostler service start` (or `hostler doctor
  --fix`) replaces an older service that has no live work and whose secrets database is locked,
  and starts this hostler. With live work — a heavy task, a run holding a claim, a takeover — or
  an unlocked database, nothing is stopped and the recovery reads `when its work has finished:
  hostler service install …`; an install locks the database, so only with the person's agreement.
- **`older`**: this hostler is older than the service. Nothing replaces a newer service: use the
  binary the service runs (the recovery names its path), or `cargo install hostler`.
- **`unknown`**: a version that does not parse. The recovery names the binary the service runs.

A service whose contract matches but whose version differs is not a mismatch: `hostler doctor`
reports it as `service.binary`.

## Every code

| Code | When it happens | What resolves it |
|---|---|---|
| `service_unavailable` | The service could not be reached or started; see its reasons above. | the matching reason above |
| `contract_mismatch` | The service speaks another contract; see the section above. | `hostler service start`, or the newer binary |
| `service_busy` | A stop, restart, install or uninstall, or an automatic replacement, found live work: running heavy tasks, runs holding claims, a takeover in progress, or an unlocked secrets database. | `hostler status`; wait for the work to finish, then repeat the command. An explicit `--force` goes ahead anyway. An agent uses --force only with the operator's explicit permission. |
| `bad_request` | A malformed call: a bad name or field, a value passed as an argument, more than 16 fields on one account, an unknown topic or flag. | fix the call; exit 2 |
| `unknown_entry` | A heavy-queue or claim id is not known: it finished, was withdrawn, or never existed. | `hostler queue history`, or `hostler status` for claims |
| `queue_timeout` | A wait for a heavy slot or a claim ran out (`--max-wait`); the request was withdrawn. | `hostler queue`, then run again with a longer `--max-wait` |
| `queue_busy` | `--no-wait` and nothing is free right now, or 256 secret requests are already pending. | `hostler queue`, `hostler secret requests`; or drop `--no-wait` |
| `withdrawn` | The wait was withdrawn: Ctrl+C, or an operator withdrew a request that had not started. | run again; a task that started is not withdrawn |
| `capacity_out_of_range` | `hostler queue capacity set` was given a value outside 1–16. | `hostler queue capacity set <1-16>` |
| `force_required` | An operator action needs `--force`: a queue release, or a takeover of a live claim. The message names the exact command and the confirmation token. | the command in the message, with `--force`, once the operator has decided. An agent uses --force only with the operator's explicit permission. |
| `internal` | An unexpected failure, or a state directory hostler cannot use. | `hostler doctor`; report it, and do not retry blindly |
| `not_interactive` | A person-only command needs a terminal and got a pipe. | a person runs the same command in a terminal; an agent asks the operator |
| `not_in_project` | The directory is not a usable git checkout: not inside one, its `.git`/`commondir` does not resolve, or its path is not valid UTF-8. | run from the checkout |
| `config_invalid` | `hostler.toml` has a problem, reported by line and never with the line's text; or the hostler markers in the agent docs are broken. | `hostler init` and `hostler doctor` name the line; fix it by hand, since init never rewrites the file |
| `project_not_approved` | The project has no approval for its secrets. | a person runs `hostler project approve` |
| `secrets_locked` | The secrets database is locked, or there is none yet. | a person runs `hostler unlock` |
| `unlock_failed` | The master password or the key file is wrong. | `hostler unlock` again with the right one |
| `secrets_conflict` | `hostler.kdbx` changed outside hostler and cannot be read with the key hostler holds, a value edited outside breaks the value rules (`details.reason = "invalid_value"`), or a group holds two entries with one name. Nothing was written. | `hostler unlock`; for a bad value `hostler secret set NAME`; remove one of two entries in KeePassXC |
| `secret_missing` | The project declares no such secret, or it has no value. | `hostler secret request NAME --reason "<why you need it>"` |
| `secret_not_in_project` | The secret belongs to another project and is not shared here. | a person runs `hostler secret share NAME --to <project>` |
| `secret_exists` | A secret import would replace a stored name. | add `--overwrite`, once the person agrees |
| `secret_too_short` | A value under 4 bytes (an empty value counts); a shorter one would garble masking. | store a longer value |
| `request_timeout` | A secret request's wait ran out. This build reports that with the request's own state (`timed_out`) and exit 75; no command produces `request_timeout` yet. | `hostler secret requests`, then ask again if the person still has to answer |
| `unknown_resource` | A selector names a resource this hostler does not have (`screen` is the fixed one; accounts and devices are named). | `hostler status` |
| `resource_in_use` | Another workspace holds the resource. | `hostler status` names the holder; wait, or take it over with the operator's decision |
| `claim_not_held` | The claim id is not this workspace's: another workspace's, or already released. | `hostler status` |
| `seizure_failed` | A takeover did not happen: the holder's work could not be stopped, or the confirmation token did not match. | `hostler status`; the resource stays with its holder |
| `account_missing` | The project has no such account, or a run asked for one that is not set. | a person runs `hostler account set NAME --field <field>` |
| `claim_deadlock` | Waiting would never end: the resource is held by a process nested inside this very wait. | run the nested command inside the outer run, or take every resource in the outer run; `hostler status` |
| `taken_over` | This run, or the run it comes from, is being taken over. Every wrapper treats it as "not running", never as a service fault. | nothing: the takeover proceeds. Do not retry the same run; a new run takes the resource normally. Exit 78. |
| `device_not_found` | No registered device, and no connected device, matches the selector. | `hostler devices list --all` |
| `alias_taken` | The alias belongs to another device. | choose another alias, or `hostler devices rename <holder> <other>` |
| `device_not_registered` | The device is not registered; a physical phone is registered by a person. | `hostler devices add <device>`; a person types the model for a physical phone |
| `device_not_ready` | The emulator or simulator is shut down. | rerun with `--boot` |
| `boot_failed` | The AVD or simulator did not boot. | `hostler devices show <alias>`; the message names the emulator's log |
| `tool_missing` | adb or agent-device cannot be found. | `hostler devices tools --detect`; for agent-device, install it or point at a project's copy (the message has the command) |
| `permission_denied` | A person-only device command reached the service without a person's confirmation (the CLI's own prompt was skipped, as in a direct service call). | a person runs the command in a terminal; an agent asks the operator |
