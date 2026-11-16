# The screen

This machine's logged-in desktop session is one exclusive resource. Anything that drives the real
GUI — Peekaboo, AppleScript/System Events, a screenshot tool, a click-and-type script — must hold
it first, or two agents will type into the same window.

```sh
hostler run --screen -- ./drive-the-gui.sh
hostler run --heavy --screen -- ./gradle-ui-test
hostler run --screen --secret API_KEY -- ./deploy-and-check
```

`hostler run --screen` takes a **lease**: the screen is held for the command and released when it
ends, including on Ctrl+C. hostler does not capture the command's output; only `--secret` and
`--account` add the masking wrapper, and then its stdout and stderr are pipes.

A device is a separate claim: `hostler run --screen --device pixel-9 -- ./drive-and-test` takes
both, one device per run; see `hostler guide devices`.

Take an explicit **hold** when several commands must use the screen in turn:

```sh
hostler hold screen            # TTL 30 minutes; release it with the id it prints
hostler release <id>
hostler run --screen -- ./first
hostler run --screen -- ./second
```

Every `run --screen` in that workspace joins the hold instead of queueing, and the hold keeps the
screen after each command. Release it when the GUI work is done: while you keep it, other
workspaces wait.

A second `run --screen` beside another screen lease of the same workspace waits; the screen is
shared by a hold and by re-entrant nesting (a `hostler run` started inside a command that already
holds it). A `hostler run --screen` started inside another `run --screen` says
`[hostler] screen: held by the enclosing run.` when it ends; a run under the workspace's hold says
`still held by your hold … until …` instead.

From outside a git checkout the screen is refused like every other claim (`not_in_project`).

Waiting, the exit codes, the summary lines, takeover and its limits are the same as for any
claim — see `hostler guide holds`. To take the screen from another workspace you need the
operator's decision, `--force --confirmation screen`. An agent uses --force only with the
operator's explicit permission.

`hostler status` shows who holds the screen, with the holder's workspace path and its activity.
