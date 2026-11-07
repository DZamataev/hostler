# Secrets

Secrets belong to a project (a git checkout) and live in one KeePass database on this machine.
You read and use them by name; no hostler command prints a value.

## Never ask for a value in chat

Do not ask a person to paste a secret value into chat, a file, a command line or an environment
variable you set yourself. Ask for the secret by name instead:

```sh
hostler secret request API_KEY --reason "needs the payments API for the integration test"
```

A person answers it with `hostler secret set API_KEY` (values are typed at a hidden prompt) or
declines; the command above returns the answer. If a value does appear in a conversation, do not
repeat it, quote it, write it to a file or pass it to a command — ask the person to run
`hostler secret set NAME` again.

## Reading the list

```sh
hostler secret list            # NAME, STATE, SOURCE, PURPOSE
hostler secret list --json     # the contract Envelope<SecretList>
```

`STATE` is `set` or `missing`; `SOURCE` is `own`, `shared from <project>` or `-` when the name is
only declared and not stored. `PURPOSE` comes from `hostler.toml`. Names declared in
`hostler.toml`, stored in the project's KeePass group and shared in from another project are all
listed, so a `missing` row means the project expects the name but has no stored value (for a shared
name, the source project's value is missing). There is no command that shows a value — this list is
the only inspection tool.

```sh
hostler secret status          # locked / unlocked / missing, database path, pending requests
hostler secret status --json   # the contract Envelope<SecretsStatus>
```

The status lists `hostler.kdbx.prev-*` files kept after a save conflict — `preserved_files` in
`--json` (see KeePassXC below).

## Storing a value

`hostler secret set NAME` reads the value from a hidden terminal prompt, or from stdin when stdin
is not a terminal (one trailing `\n` or `\r\n` is stripped). The value is at most 64 KiB and must
be valid UTF-8; a longer input is refused, never cut.

`hostler secret import FILE [--overwrite] [--delete]` reads an `.env`-style file — at most
**1 MiB** — into the current project. Any problem prints `FILE:LINE: message` and stores nothing:
the import is all-or-nothing. A file over 1 MiB, or one that cannot be read, is refused with a
fixed message that does not repeat the path (the path may have been a pasted value). On success it
prints the stored names. `--delete` removes the file; otherwise a person is offered the deletion,
and a non-interactive run only warns that the file still holds the values.

## Requesting a secret

```sh
hostler secret request NAME --reason "<why you need it>" [--max-wait MIN] [--json]
hostler secret requests [--json]
hostler secret decline <id|NAME> [--json]
```

- `--reason` is required, 1–500 bytes, one line of why the code needs the value. It is shown to
  the person, so control characters are replaced with `�`.
- `--max-wait` is minutes; fractions are fine (`0.5`), `0` means no limit. The default is **10**
  minutes. The first status line appears at once, then every 30 seconds and on every change.
- The waiter re-sends the same request id every 5 seconds, on every `secrets.*` event and after a
  reconnect. Events are only hints (the service may coalesce them), so a missed event only delays
  the answer; the poll still finds it.
- `hostler secret requests` lists the pending requests machine-wide: an 8-character id, project,
  NAME, age and the sanitized reason. Use `--json` instead of parsing text.
- `hostler secret decline` answers one request. A valid secret NAME (for example `API_KEY`)
  declines every pending request for it in the current project. Anything else is a request id of
  8–36 characters from `0-9`, `a`–`f` and `-`; a prefix is enough. Ids are printed in lower case,
  and an upper-case id-looking string that is also a valid NAME is treated as a NAME.

The waiting line has this exact shape (`{…}` is replaced with the real values; `{limit}` is a
duration such as `42 s` or `10 min`, or `no limit`):

```
[hostler] Waiting for a person to provide {NAME} for project {project} (request {short_id}). Waited {waited} of {limit} — this is waiting, not hanging; Ctrl+C withdraws the request. A person runs: hostler secret set {NAME}   (or declines: hostler secret decline {short_id})
```

While the database is locked, the last part becomes:

```
The database is locked: a person runs hostler unlock, then hostler secret set {NAME}   (or declines: hostler secret decline {short_id})
```

Exit codes:

| Exit | Meaning |
|---|---|
| **0** | The value is stored now; the request is answered. An already-set secret answers `stored` at once. |
| **1** | A person declined the request, or the command failed (locked or unapproved project at the first call, a lost service after 10 s of reconnecting). |
| **75** | The `--max-wait` deadline passed, or an operator (or the reaper, for a dead requester) withdrew the request. |
| **130** | Ctrl+C; the request was withdrawn. |
| **2** | Usage error (a missing `--reason`, an invalid NAME, a bad `--max-wait`). |

With `--json`, the final answer is one `Envelope<SecretRequestInfo>` on stdout; status and stop
lines stay on stderr. A lost connection is retried for up to 10 seconds, and those reconnects never
start a service (a person may have stopped it); after that the command exits 1 and says the request
may still be pending: check `hostler secret requests`. If a stop cannot withdraw the request in its
2-second budget, the message says so and the request ends once the service sees this process gone.

## Injecting a secret into a command

```sh
hostler run --secret API_KEY -- ./deploy
hostler run --secret API_KEY=SERVICE_TOKEN -- ./deploy
hostler run --secret-file TLS_CERT -- env
hostler run --heavy --secret API_KEY -- ./gradlew integrationTest
```

- `--secret NAME[=VAR]` puts the value into the environment variable `VAR` (default: the `env`
  field in `hostler.toml`, else `NAME`).
- `--secret-file NAME[=VAR]` writes the value to a private file named `secret-NAME` in the run's
  private directory and puts the file's **full path** into `VAR`. The directory is readable only
  by you (0700/0600 on Unix, a user-only DACL on Windows), and the files are removed when the
  command ends, including on Ctrl+C. A later run and the service sweep what a killed wrapper
  leaves behind (once the wrapper and command are dead).
- Both flags are repeatable and can be combined; two secrets may not set the same environment
  variable. `--secret` and `--secret-file` compose with `--heavy` (same queue, same exit codes).
- Prefer `--secret-file` for tools that log their environment: an environment variable is visible
  in process listings and diagnostics, a path is not the value.

The command's stdin is inherited. Its stdout and stderr are pipes to the wrapper, so the command
sees pipes, not a terminal; output is streamed through the masker (below). A `run` with any secret
flag **fails closed**: if the project, the database or the service is not ready, or materialization
fails, hostler prints the error and exits with exit 78 without starting the command. A malformed
`--secret`/`--secret-file` value is a usage error (exit 2) with a fixed text; a value passed in its
place — including one that starts with `-` — is never echoed.

## Masking and its limits

Every injected value is replaced with `***` in the command's stdout and stderr, and in the command
string hostler records for the heavy queue and the materialization history. Matching is on raw
bytes, leftmost first, longest value first.

It is not a data-loss-prevention system:

- Encoded or reformatted values are not masked: base64, hex, URL-encoding, JSON escaping, a value
  split across stdout and stderr, or a tool that normalizes it.
- A value the command reads itself (its own config, a different variable) is not one of the
  injected values and is not masked.
- Output that ends in the first bytes of a value is held back until more output or EOF, so it is
  not printed early and then completed. After the command exits there is a short drain grace
  (2 seconds); output that arrives later from a background process is discarded (a held prefix is
  written as `***`). A background process can therefore lose output; keep output in the command
  itself.
- Values shorter than **4 bytes** are refused (`secret_too_short`), because masking them would
  garble ordinary text. A value is at most 64 KiB, must be UTF-8, and may not contain control
  characters other than tab, newline and carriage return.

## Nested runs

Every secret run sets `HOSTLER_SECRET_RUN=1` in its command's environment. A `hostler run` started
inside that command (a build script that wraps another command, for example) records
`<command inside a secret run>` instead of its own command in the heavy queue and its history: the
nested command line may already hold an inherited value, and the nested run cannot mask what it
never saw. The outer secret run still masks the nested command's stdout and stderr.

## Exit codes and error codes

`hostler run` with a secret flag:

| Exit | Meaning |
|---|---|
| **78** | Refused before running: any secrets problem (see the error codes below), no service, or a contract mismatch. The command never started. |
| child's code | The command ran; its exit status passes through. |
| **75** | `--no-wait`, the heavy-queue deadline, an operator withdrawal, or `queue_busy`; the command never started. |
| **127** | The command could not be started (`cannot run …`; the program name is masked). |
| **130** | Interrupted. |
| **2** | Usage error, including a malformed `--secret NAME[=ENV]`. Any parse error in a `run` that names `--secret` or `--secret-file` prints a fixed text and exits 2; the argument is not echoed. |

`hostler unlock`, `lock`, `project …`, `secret …` and `guide` exit **0** on success, **1** on any
error, **2** on a usage error. `hostler secret request` has its own table above.

Errors print `[hostler] <message>` and, when the error has one, `[hostler] To fix: <command>`. In
`--json`, `code` is stable and `recovery`, when present, holds that exact command. The table says
what to do: the `recovery` command where the code has one, otherwise the human step.

| Code | When | What to do |
|---|---|---|
| `secrets_locked` | The database is locked or missing. Answered at once, without waiting. | `hostler unlock` |
| `project_not_approved` | The checkout has no approval yet. | `hostler project approve` |
| `secret_missing` | The project has no such secret. | `hostler secret request NAME --reason "<why you need it>"` |
| `secret_not_in_project` | The secret belongs to another project and is not shared here. | `hostler secret share NAME --to <project>` |
| `secrets_conflict` | `hostler.kdbx` changed outside hostler and cannot be read with the key hostler holds, a value edited outside breaks the value rules (`details.reason = "invalid_value"`), or the group holds two entries with one name. Nothing was written. | `hostler unlock`; for a bad value `hostler secret set NAME`; for two entries, remove one in KeePassXC |
| `unlock_failed` | Wrong master password or key file. | `hostler unlock` |
| `secret_exists` | `secret import` would replace a stored name. | add `--overwrite` |
| `secret_too_short` | The value is under 4 bytes (an empty value counts). | store a longer value |
| `bad_request` | A malformed name, a value over 64 KiB or with control characters, stdin over 64 KiB, an import with more than 1000 entries, a request reason over 500 bytes, and similar usage errors. | fix the call; names match `^[A-Z][A-Z0-9_]{0,63}$` |
| `queue_busy` | 256 secret requests are already pending on this machine. | answer the pending ones: `hostler secret requests` |
| `not_interactive` | A person-only command needs a real terminal and got a pipe or no terminal. | the same command, `hostler <cmd>`, in a terminal |
| `not_in_project` | The directory is not a usable git checkout: not inside one, its `.git`/`commondir` does not resolve, or its path is not valid UTF-8. | run from the project |
| `config_invalid` | `hostler.toml` has a problem; the message names the line, never a value. | fix the reported line |
| `service_unavailable` | The service could not be reached or started. | `hostler service run` (while it is stopping: `hostler service start`) |
| `contract_mismatch` | The running service speaks another contract version. | `hostler service restart` |
| `internal` | An unexpected hostler failure, or an import file over 1 MiB or unreadable (fixed text; the path is not repeated). | report it; do not retry blindly |

Errors never contain a value, the master password or key-file bytes.

## Person-only commands

These need a person at a real terminal: stdin and stderr must be terminals, and the controlling
terminal (Unix `/dev/tty`, the Windows console) must be open. An agent's piped shell is never
enough.

- `hostler unlock` and creating the database
- `hostler project approve`
- `hostler secret share` and `hostler secret unshare`
- the delete prompt after `hostler secret import` (the import itself is not person-only)

Their refusal and `--help` end with: **A person runs this; an agent asks the operator.**

`lock`, `secret status`, `secret list`, `secret request`, `secret requests`, `secret set` from
stdin, `secret remove` and `secret import` are not person-only. `hostler secret set NAME` reads the
value from the hidden prompt when a person runs it in a terminal, otherwise from stdin (one
trailing newline is stripped, at most 64 KiB). Stdin is for a person's own pipelines, such as a
password-manager CLI; an agent never pipes a value — it asks with `hostler secret request`.

## Unlock, lock, and the service

A person unlocks the database once per service start with `hostler unlock`; the master password
is typed at a hidden prompt, never passed as an argument or read from stdin as data.
`hostler lock` locks it now. The service keeps the open database in memory; its key is zeroized
when it locks or stops. If the service is not run by the OS service manager — a custom
`HOSTLER_HOME`, or hostler is not registered — then after it unlocks, `unlock` notes on stderr that
the service can end with the login session and lock the database again; for the real home,
`hostler service install` keeps it running.

`hostler service restart` (and `hostler service stop`) **locks the database again**. After a
restart a person runs `hostler unlock` before agents can use secrets; pending requests are kept in
SQLite and survive the restart. `hostler service restart` says so on stderr when the database is
locked.

## KeePassXC and other editors

The database is a KeePassXC-compatible KDBX 4.1 file, `hostler.kdbx` in hostler's state directory
(`hostler secret status` prints the exact path). Each project is a top-level group; renaming the
group in KeePassXC keeps the link, because it is stored by group UUID. A group moved to the
Recycle Bin is treated as gone.

Before every read and write hostler checks the file's fingerprint; a change made in KeePassXC is
re-read with the key hostler already holds. hostler **never replaces a file it has not verified**:
a save writes a temp file, moves the outside version aside, verifies it, and only then places its
own; a conflict leaves the outside version in place and reports `secrets_conflict`. A file kept
after a conflict is named `hostler.kdbx.prev-<uuid>`; `hostler secret status` lists those paths and
you can open them in KeePassXC to recover a change. hostler saves KDBX 4.1, which KeePassXC reads.

Residual risk: an editor that rewrites `hostler.kdbx` in place (instead of saving through a temp
file and rename) can still lose a change made at the instant hostler saves. KeePassXC's default
safe save does not; save and let hostler re-read before making another change.

## Memory

Values are zeroized where hostler owns them: `SecretValue` and the open database key are wiped on
drop, the service's core dumps are disabled, and CLI commands that hold a value disable core dumps
on Linux. Copies inside the socket library's codec buffers and the operating system's socket
buffers are not erased — a same-user process that can read hostler's memory is outside the threat
model below.

## `hostler.toml`

Declare the secrets a project expects and how the command should see them:

```toml
[project]
name = "my-app"

[secrets.API_KEY]
purpose = "the payments API"
env = "PAYMENTS_API_KEY"
```

`purpose` is required (one line, at most 200 characters). `env` is optional; `--secret NAME` then
uses `PAYMENTS_API_KEY`. Names match `^[A-Z][A-Z0-9_]{0,63}$`; environment variable names match
`^[A-Za-z_][A-Za-z0-9_]{0,127}$` and must be unique. `[project].name` is 1–64 characters. An
unknown top-level table is not used yet and warns:
`[hostler] hostler.toml: [<table>] is not used by this hostler version`. A problem is reported
with its line number and never with the line's text, because the file may hold a value pasted in
by mistake.

## The threat model

> hostler protects against accidental exposure and against agents that follow this guide: values never appear in any hostler output, log, error, event, history row, Debug print or panic message, and no command prints a value. It does not protect against a malicious process of your own OS user: such a process can talk to the socket, run `hostler run --secret X -- sh -c 'echo $X | base64'`, or read hostler's memory.
