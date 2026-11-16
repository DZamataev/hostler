# hostler provider for e2e.com

`hostler-provider.mjs` is a `DeviceProvider` for e2e.com's mobile engine (`@e2e-dev/mobile`).
It lets hostler pick the phone, emulator or simulator each worker slot drives, so parallel e2e
runs and agents on this machine never drive the same device. It needs Node 22 or later and has
no dependencies. It is a repo file for now. Copy it into your project; it is not on npm yet.

```ts
// e2e.config.ts
import { mobile } from '@e2e-dev/mobile';
import { hostlerProvider } from './hostler-provider.mjs';

const iphone = mobile({
  platform: 'ios',
  device: hostlerProvider({ kind: 'simulator', boot: true }),
});

export default {
  targets: [{ name: 'iphone', engine: iphone, app: { bundleId: 'com.example.app' } }],
  workers: 2,
};
```

## Options

| Option | Default | Meaning |
|---|---|---|
| `platform` | the target's platform | `ios` or `android`. Leave it out: the engine's `platform` is what agent-device is told. |
| `kind` | any | `physical`, `emulator` (Android) or `simulator` (iOS). |
| `boot` | `false` | Boot a shut-down emulator or simulator, headless. hostler shuts down only a device it booted itself, after 2 minutes with no claim and no agent-device session on it. Without `boot`, a shut-down emulator or simulator is not taken; physical devices are taken as they are. |
| `maxWait` | hostler's (5) | Minutes to wait for a free device; `0` waits until the run is interrupted. |
| `hostler` | `hostler` | The hostler command, if it is not on `PATH`. |

Only devices registered with `hostler devices add` are leased. Any agent may add an AVD or a
simulator. A physical phone is added by a person.

## What it does

For every worker slot, `acquire` runs this command in the run's project root (`projectRoot`):

```
hostler hold device auto:<platform>[:<kind>] --distinct --while-parent --json [--boot] [--max-wait <min>]
```

- **`--distinct`** gives each slot a device this workspace does not use yet. A device that
  already carries one of this workspace's agent-device sessions is skipped too.
- **`--while-parent`** binds the hold to its parent process, the e2e runner. If the runner dies
  without releasing, hostler frees the device.
- **Inside `hostler run --device …`** the provider still takes other devices. It drops
  `HOSTLER_CLAIM_TOKENS` from the command's environment, and a distinct hold never joins the
  enclosing run's claim.

The lease is `{ id: <claim id>, deviceId: <agent-device id>, client: {} }`. `deviceId` is the
device's `agent_device_id`: a simulator's or iPhone's UDID, or an Android device's adb serial
(`emulator-5554` for a booted emulator). `client: {}` means the worker drives the device through
the local agent-device daemon.

**The agent-device rule:** the lease's `deviceId` alone picks the device. The lease has no
`device` (name) field, because e2e would select by name first and agent-device takes the first
name that matches. hostler exports nothing to the e2e workers: no `ANDROID_SERIAL` and no
`HOSTLER_DEVICE_*`. Only the lease reaches them. e2e passes the lease's `deviceId` to
agent-device as the device's UDID or serial. Inside `hostler run --device …`, the run's own
device variables are still in the environment that e2e inherits, but the lease's `deviceId` is
what picks the device.

`release` runs `hostler release <id>` in the same project root, within e2e's cleanup budget. An
already released claim counts as released. If `acquire` fails or is interrupted after hostler
granted the claim, the provider releases the claim itself, because e2e releases only leases that
`acquire` returned.

hostler's waiting lines and warnings go to the run's progress log. One example is a warning that
an agent-device session is still open on a device at its release.

## Limits

- One failed slot fails the whole run (e2e's rule). With fewer free devices than slots, the
  slots wait up to `maxWait` and then fail.
- Interrupting a waiting `acquire` stops its hostler command. On Unix the command withdraws its
  request (SIGTERM). On Windows the command is killed, and the service drops the request at its
  next check. A claim the command printed before it stopped is released by the provider, and
  `acquire` throws. Only a claim granted in the instant the command was killed, before it
  printed, stays held until the runner exits. Windows' kill makes that window real; on Unix the
  command withdraws or releases on SIGTERM.
