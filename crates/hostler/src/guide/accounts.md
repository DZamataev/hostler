# Test accounts

A test account is one entry in the project's KeePass group: the entry's title is the account name,
it carries the `hostler-account` tag, and each value (`session`, `env`, `profile`, …) is one
protected custom field. hostler hands the fields to one command at a time; no command prints a
value. Setting an account up is a person's job — an agent asks for it by name.

## A person sets an account up

```sh
hostler account set qa-user --field session    # hidden prompt, or stdin when piped
hostler account set qa-user --field env
hostler account list                           # NAME, FIELDS, HOLDER — never values
hostler account list --json                    # Envelope<[AccountInfo]>: a bare array
```

`--json` answers an array of `{name, fields, holder}`; `holder` is `null` or the workspace path,
claim kind and PID of the run or hold using the account. The value comes from the hidden prompt or
from stdin; it is never an argument. Any argument after the flags is a usage error (exit 2) with a
fixed text, and nothing is echoed.

The entry is a normal KeePassXC entry: edit it there and hostler re-reads it. `UserName`,
`Password`, `URL` and `Notes` are never read. Two tagged entries with one name are refused
(`secrets_conflict`); an untagged entry is not an account.

## Reserved names

`HOSTLER_ACCOUNT_NAME` is the account's own name, so no field may be called `name`:
`hostler account set qa-user --field name` is refused with exit 2. A KeePass custom field called
`name` that somebody added for their own use is the person's data — hostler does not list it, does
not inject it and does not remove it.

An account may not be called `screen`: `--confirmation screen` must always mean the screen, never
an account, so `hostler account set screen --field session` and `hostler account remove screen` are
refused with exit 2.

A tagged entry whose title is `screen` (or any title that is not a valid name) is **not** listed as
an account: `account list` does not show it, and hostler never uses or removes it. It stays the
person's entry. Rename it in KeePassXC (for example to `qa-user`), then `hostler account set
qa-user --field FIELD` adds hostler's fields to it again.

## Running with an account

```sh
hostler run --account qa-user -- ./login-test
hostler run --heavy --account qa-user -- ./gradle-integration
hostler run --screen --account qa-user -- ./ui-login
```

The account must exist, the project must be approved and the database unlocked, or the run refuses
with exit 78 before the command starts (`account_missing`, `secrets_locked`,
`project_not_approved`). Ask the operator for a missing account with `hostler account request`
below; never ask for a field's value in chat.

## Asking for an account

```sh
hostler account request qa-user --field session --field env --reason "<why you need it>" \
    [--max-wait MIN] [--json]
hostler account requests [--json]
```

- Request field **names**, never values. `--reason` is required (1–500 bytes, one line); it is
  shown to the person, so control characters are replaced with `�`.
- `--max-wait` is minutes; fractions are fine (`0.5`), `0` means no limit. The default is **10**
  minutes. The first status line appears at once, then every 30 seconds and on every change.
- The waiter re-sends the same request id every 5 seconds, on every `accounts.*` event and after a
  reconnect. An already-set account answers `stored` at once.
- A person answers in the hostler app (one card, every field stored in one save) or with
  `hostler account set qa-user --field FIELD` per field. A request is answered once all its
  fields are set. A person can also decline it with `hostler account decline <id>`.
- `hostler account requests` lists the pending requests machine-wide: an 8-character id, project,
  name, fields, age and the sanitized reason. Use `--json` instead of parsing text.

The states are `pending`, `stored`, `declined`, `withdrawn` (the requester died or Ctrl+C) and
`timed_out`. Exit codes:

| Exit | Meaning |
|---|---|
| **0** | The fields are stored now; the request is answered. |
| **1** | A person declined the request, or the command failed (locked or unapproved project at the first call, a lost service after 10 s of reconnecting). |
| **75** | The `--max-wait` deadline passed, or an operator (or the reaper, for a dead requester) withdrew the request. |
| **130** | Ctrl+C; the request was withdrawn. |
| **2** | Usage error (a missing `--reason`, an invalid name or field, a bad `--max-wait`). |

With `--json`, the final answer is one `Envelope<AccountRequestInfo>` on stdout; status and stop
lines stay on stderr. `account request` and `account requests` are agent-facing; `account set`
(whose value is typed at a hidden prompt) and `account decline` are person commands. `decline`
asks for the master password at a hidden prompt and passes the person gate (spec §8.5). There is
no command that prints an account's values.

`account decline` is the app's Decline button in the terminal; the app's card is the usual
answer. Without the app, a person sets the fields one by one.

The command sees:

- `HOSTLER_ACCOUNT_NAME` — the account's name;
- `HOSTLER_ACCOUNT_<FIELD>` — one variable per field, upper-cased (`session` →
  `HOSTLER_ACCOUNT_SESSION`).

One account per run: two would collide on those variable names. A run with both `--screen` and
`--account` takes two claims, in resource-id order.

The account is an exclusive claim per project. A second run from the same workspace **joins** the
first (both run at once); a run from another workspace waits with the usual waiting line, and the
account's holder path is shown by `account list` and `hostler status`.

## Masking

Field values are masked in the command's stdout and stderr exactly like `--secret` values (`***`,
leftmost first, longest value first), and in everything hostler records. The claim's command reads
`<command pending>` while the run waits — the values are not known yet — and `***` once they are.
The account materialization history records the field names (`account:qa-user/session`) and a
command already masked with the secret and field values.

Masking has limits: encoded or reformatted values, a value the command reads itself, and
output after the drain grace are not covered. See `hostler guide secrets`.

## Refreshing a session

Project tooling that obtains or refreshes a session writes it back through hostler, never to a
file:

```sh
printf '%s' "$NEW_SESSION" | hostler account set qa-user --field session
```

`account set` takes no claim and reads the value from stdin (or the hidden prompt). A run does not
need to be stopped to refresh its account: the next run reads the new value.

The value a run masks is fixed when the run takes its account (`account.materialize`). A value
written back **during** a run is not in that run's mask, so a refresh script must never print the
new value, its length or a fragment of it.

## Removing

```sh
hostler account remove qa-user --field env
hostler account remove qa-user
```

`hostler account remove` removes only hostler's own data: the account fields, or the one named with
`--field`, and — once no field is left — the `hostler-account` tag. The KeePass entry itself is
deleted only when nothing else is left in it. A password, user name, URL, notes, other custom
fields or attachments somebody keeps on the entry stay, and the entry stays with them. `--help`
says so.

Removal is refused while the account is claimed (`resource_in_use`). Two races are documented, not
closed: a run granted between hostler's holder check and the vault write finds the account missing
and refuses with 78 (nothing ran); and if a claim is seized between that check and the vault read
of `account.materialize`, the run still receives the values and the taker stops it. The output is
`removed` or `not set`; `--json` answers `Envelope<{removed}>` (`{"ok":true,"data":{"removed":true}}`,
never a bare object).

## Errors

| Code | When | What to do |
|---|---|---|
| `account_missing` | The project has no account with that name. | a person runs `hostler account set NAME --field FIELD` |
| `secrets_locked` | The database is locked or missing. | `hostler unlock` (a person) |
| `project_not_approved` | This checkout is not approved. | `hostler project approve` (a person) |
| `resource_in_use` | `account remove` while the account is claimed. | `hostler status` |
| `bad_request` | A bad name or field, a value as an argument, or more than 16 fields on one account. Names match `^[a-z][a-z0-9._-]{0,63}$` and are not `screen`; fields match `^[a-z][a-z0-9_]{0,31}$` and are not `name`. | fix the call |
| `secrets_conflict` | Two tagged entries with one name, or a value edited outside hostler that breaks the value rules. Nothing was written. | remove one entry in KeePassXC, or `hostler account set NAME --field FIELD` |
| `not_interactive` | `account set` was given a terminal for stdin but no full terminal for the prompt and no piped value. | run it in a terminal, or pipe the value on stdin |
| `not_in_project` | Not inside a git checkout. | run from the project |
| `service_unavailable` | The service could not be reached or started. | `hostler service start` |
| `contract_mismatch` | The running service speaks another contract version. | `hostler service start` (replaces an idle, locked older service), or the newer binary the error names |

Values never appear in an error, a log, an event, a history row or `--json`.
