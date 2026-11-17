# Agent sandboxes

An agent sandbox is a kernel policy around the agent's commands — Codex's sandbox on macOS, Claude
Code's Bash sandbox. It denies reads and writes outside the project, and it denies connecting to a
local socket the agent was not allowed. hostler's service listens on exactly that kind of socket,
so a sandboxed agent needs the socket allowed, or a way to run hostler outside the sandbox.

## The service runs outside the sandbox

There is one hostler service per user, and it outlives any agent: it owns the machine-wide heavy
queue, the claims and the open secrets database. Register it with the OS once
(`hostler service install`); it then starts at logon, outside every sandbox, and the agent's
commands are only clients of it.

The socket path depends on the OS; `hostler service status` prints the one in use:

- macOS: `~/Library/Application Support/io.github.dzamataev.hostler/hostler.sock`
- Linux: `~/.local/share/hostler/hostler.sock`
- with a long home path: `/tmp/hostler-<uid>-<hash>/hostler.sock`
- Windows: the named pipe `\\.\pipe\hostler-<user>-<hash>`

## What a denial looks like

Both sandboxes allow the `stat()` of the socket and refuse the `connect()` with `EPERM`. hostler
reports that as `service_unavailable` with `details.reason = "sandbox_denied"`: "this process may
not reach hostler's socket (an agent sandbox?)". It is never "the service is not running", and
hostler starts nothing: a service started inside the sandbox would serve only the sandbox and
would end with it.

The error's recovery says: allow hostler's socket in the agent's sandbox, or run hostler outside
the sandbox; if no sandbox is involved, check the permissions of the directory it names.

## Claude Code

Add the socket to the project's (or the user's) `.claude/settings.json`. The sandbox stays on for
everything else:

```json
{
  "sandbox": {
    "network": {
      "allowUnixSockets": [
        "/Users/you/Library/Application Support/io.github.dzamataev.hostler/hostler.sock"
      ]
    }
  }
}
```

`allowAllUnixSockets: true` is the broad alternative Claude Code's documentation gives, and the
only one on Linux and WSL2: it lets every sandboxed command reach every Unix socket. Prefer the
one path.

## Codex: a command rule

No stable Codex setting allows one socket: every setting that reaches the socket without a rule
opens the whole network to every sandboxed command. The narrow, stable route is Codex's command
rule. Its `.rules` exec policy is on by default, and a command that matches an `allow` rule runs
**outside the sandbox** — in the interactive TUI and in `codex exec` alike.

Save this file as `~/.codex/rules/hostler.rules`, or as
`<checkout>/.codex/rules/hostler.rules` (Codex reads a project's rules only in a trusted project):

```
# ~/.codex/rules/hostler.rules  (or <trusted project>/.codex/rules/hostler.rules)
prefix_rule(
    pattern = ["hostler", ["status", "release", "workspace", "guide", "init", "doctor"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", "hold", ["screen", "account", "device", "renew"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", "devices", ["list", "show", "installs", "tools"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", "service", ["status", "start"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", "queue", ["history", "--json"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", "secret", ["status", "list", "request", "requests"]],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
prefix_rule(
    pattern = ["hostler", ["account", "project"], "list"],
    decision = "allow",
    justification = "hostler reaches its per-user service over a Unix socket the sandbox blocks",
)
```

It allows hostler's agent-facing commands and nothing else. With the rule in place:

- The agent calls plain `hostler` commands: `hostler status`, `hostler hold screen`,
  `hostler guide`, `hostler init`, `hostler doctor`, `hostler devices list`,
  `hostler queue --json` and the rest of the list above. They run outside the sandbox and reach
  the service with no prompt.
- A command with an environment prefix (`HOSTLER_HOME=… hostler …`), a `cd … &&`, a pipe, a `;`,
  a redirection, a substitution or a glob is not matched: the whole command stays sandboxed and
  answers `sandbox_denied`. Put `HOSTLER_HOME` in the environment instead of on the command line.
- Read the project with `hostler project list` and `hostler status`, and the queue with
  `hostler queue --json`. Bare `hostler project` stays sandboxed on purpose: a rule for it would
  also cover `hostler project approve`.
- `hostler service install` is not in the rule. It replaces the OS registration and restarts the
  service (when forced, over live work), so under Codex a person runs it (or a session outside
  the sandbox). A sandboxed agent that tries it gets `sandbox_denied`; when
  `hostler doctor` or `hostler init` name `hostler service install`, hand it to the person.
- A prefix rule cannot exclude a flag: `hostler hold … --force` runs outside the sandbox like the
  rest of `hostler hold`. The rule does not enforce the force rule, the agent does. An agent uses
  --force only with the operator's explicit permission.
- A project's rules apply in a trusted project only. In an untrusted one Codex ignores
  `<checkout>/.codex/rules/`, and every hostler call answers `sandbox_denied`; starting `codex`
  in the checkout once makes Codex record the project as trusted.

### There is no rule for `hostler run`

`hostler run` is deliberately not in the rule. It starts another program, so a rule for it would
run that program outside the sandbox too. A sandboxed `hostler run --heavy` therefore fails open:

```
[hostler] Warning: the hostler service is unavailable (this process may not reach hostler's
socket (an agent sandbox?)); running without the heavy queue.
[hostler] To fix: allow hostler's socket in the agent's sandbox, or run hostler outside the
sandbox; if no sandbox is involved, check the permissions of <dir>; see: hostler guide sandbox
```

and the command runs unqueued — the build competes with every other build on the machine. Tell the
person when that happens.

`hostler run` has no `--json` envelope: those two lines on stderr are its whole report. The same
denial seen through a command that does take `--json` — `hostler status --json` or `hostler service
status --json` — is `service_unavailable` with `details.reason = "sandbox_denied"`.

`--device`, `--screen`, `--account` and `--secret` are claims: they fail closed (exit 78) instead
of running without their resource or without their injected values.

A person who accepts unsandboxed builds may knowingly add their own rule for it:

```
prefix_rule(
    pattern = ["hostler", "run"],
    decision = "allow",
    justification = "the person accepts that this command's child runs outside the sandbox",
)
```

That runs the command's child outside the sandbox as well. Do not use `decision = "prompt"` for it:
under `codex exec` a `prompt` rule rejects the command outright.

### Never take "don't ask again"

In the TUI without a rule, hostler's call is escalated and the person approves it once (or lets
the turn stop). Never accept the TUI's "don't ask again for commands that start with `hostler`":
it writes `prefix_rule(pattern = ["hostler"], decision = "allow")`, which also unsandboxes
`hostler run -- <any command>`, child included. Replace that file with the narrow rule above.

`codex exec` always runs with the policy `never`, so no person can approve there: the rule file is
the way in.

## When a command still answers `sandbox_denied`

- **Device, screen, account and secret commands** fail closed: ask the person to allow the socket
  or to approve running hostler outside the sandbox.
- **A heavy run** fails open with the warning; the build ran unqueued, so tell the person.
- **`hostler init`** prints both clients' settings as a person step and writes neither.
- **`hostler doctor`** reports the denial as `service.reachable` with
  `details.reason = "sandbox_denied"` and a fix for the person.

`hostler guide recovery` lists every code and the `service_unavailable` reasons.
