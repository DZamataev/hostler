// Drives integrations/e2e/hostler-provider.mjs the way e2e's `LeasedDevices` does: every slot's
// `acquire` at once, then `release` once per returned lease (tests/e2e_provider.rs).
//
// argv: <provider.mjs> <config JSON>. Config: `options` (hostlerProvider's), `platform`, `slots`,
// `projectRoot`, `abortGate` / `releaseGate` (files the test creates to abort the acquires or
// start the releases), `releaseTimes` (each lease released this often), `onGrant` (`abort` or
// `throw` when the provider logs its grant line). stdout: one JSON event per line.
import { existsSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const [providerPath, configText] = process.argv.slice(2);
const config = JSON.parse(configText);
const { hostlerProvider } = await import(pathToFileURL(providerPath).href);
const provider = hostlerProvider(config.options ?? {});
const say = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
const failure = (e) => ({ name: e?.name, message: String(e?.message ?? e), code: e?.code });

// Resolves once `path` exists (at once without one); an unref'd wait never keeps node running.
const gate = (path, { unref = false } = {}) =>
  new Promise((resolve) => {
    if (path === undefined) return resolve();
    const timer = setInterval(() => {
      if (existsSync(path)) {
        clearInterval(timer);
        resolve();
      }
    }, 50);
    if (unref) timer.unref();
  });

const controller = new AbortController();
if (config.abortGate !== undefined) {
  gate(config.abortGate, { unref: true }).then(() => controller.abort());
}

say({
  event: 'start',
  pid: process.pid,
  tokens: process.env.HOSTLER_CLAIM_TOKENS !== undefined,
  name: provider.name,
});

const slots = config.slots ?? 1;
const settled = await Promise.allSettled(
  Array.from({ length: slots }, (_, slot) =>
    provider.acquire({
      platform: config.platform,
      runId: 'run-1',
      targetName: 'app',
      slot,
      slots,
      agentDeviceVersion: '0.21.36',
      projectRoot: config.projectRoot,
      env: process.env,
      signal: controller.signal,
      log: (line) => {
        say({ event: 'log', slot, line });
        if (line.startsWith('holding ')) {
          if (config.onGrant === 'abort') controller.abort();
          if (config.onGrant === 'throw') throw new Error('the log failed');
        }
      },
    }),
  ),
);
const leases = settled.filter((s) => s.status === 'fulfilled').map((s) => s.value);
say({
  event: 'acquired',
  leases,
  errors: settled.filter((s) => s.status === 'rejected').map((s) => failure(s.reason)),
});

if (leases.length > 0) {
  await gate(config.releaseGate);
  const errors = [];
  for (let round = 0; round < (config.releaseTimes ?? 1); round += 1) {
    const done = await Promise.allSettled(
      leases.map((lease) =>
        provider.release(lease, {
          runId: 'run-1',
          targetName: 'app',
          env: process.env,
          signal: AbortSignal.timeout(30_000),
          log: (line) => say({ event: 'log', line }),
        }),
      ),
    );
    errors.push(...done.filter((s) => s.status === 'rejected').map((s) => failure(s.reason)));
  }
  say({ event: 'released', errors });
}
