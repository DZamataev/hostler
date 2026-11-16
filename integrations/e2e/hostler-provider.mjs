// hostler's DeviceProvider for e2e.com (`@e2e-dev/mobile`): one hostler device hold per worker
// slot. Node >= 22, no dependencies. See README.md beside this file.
//
// Each `acquire` runs `hostler hold device auto:<platform>[:<kind>] --distinct --while-parent
// --json` in the run's project root: a device this workspace does not use yet, held while the
// e2e runner (this process) lives. `release` runs `hostler release <id>`.
import { spawn } from 'node:child_process';

/**
 * A provider's own release (after a failed or aborted acquire) ends within 10 s, kill included:
 * stopped after 7 s, killed outright 1.5 s later, settled at most `KILL_SETTLE_MS` after that.
 */
const SELF_RELEASE_MS = 7_000;
/** How long a stopped release may take to exit before it is killed outright. */
const RELEASE_KILL_GRACE_MS = 1_500;
/** How long a stopped hold may take to exit (withdrawing its request) before it is killed. */
const HOLD_KILL_GRACE_MS = 10_000;
/** After a kill: how long to wait for the process to exit, then to read what it wrote. */
const KILL_EXIT_MS = 1_000;
const KILL_DRAIN_MS = 300;
/** At most this long from a kill to settling. */
const KILL_SETTLE_MS = KILL_EXIT_MS + KILL_DRAIN_MS;

// `_holdKillGraceMs` is for the tests only (tests/e2e_provider.rs): clamped to 1 s .. the default.
export function hostlerProvider({
  platform,
  kind,
  boot = false,
  maxWait,
  hostler = 'hostler',
  _holdKillGraceMs = HOLD_KILL_GRACE_MS,
} = {}) {
  /**
   * The project root each lease was acquired in, by claim id: every release of it runs there, a
   * repeated one too. Kept for the provider's life (one short string per lease).
   */
  const roots = new Map();

  async function acquire(req) {
    if (req.signal.aborted) throw aborted(req.signal);
    let selector = `auto:${platform ?? req.platform}`;
    if (kind !== undefined) selector += `:${kind}`;
    const args = ['hold', 'device', selector, '--distinct', '--while-parent', '--json'];
    if (boot) args.push('--boot');
    if (maxWait !== undefined) args.push('--max-wait', String(maxWait));
    const ran = await run(hostler, args, {
      cwd: req.projectRoot,
      env: holdEnv(req.env),
      signal: req.signal,
      log: req.log,
      killGraceMs: Math.min(Math.max(Number(_holdKillGraceMs) || 0, 1_000), HOLD_KILL_GRACE_MS),
    });
    const envelope = parseEnvelope(ran.stdout);
    const claim = envelope?.ok === true ? envelope.data : undefined;
    if (typeof claim?.id !== 'string') {
      if (req.signal.aborted) throw aborted(req.signal);
      throw failure('hostler hold', ran, envelope);
    }
    // The claim is ours from here on: e2e never releases what a throwing acquire got.
    try {
      const deviceId = claim.resource?.attributes?.agent_device_id;
      if (typeof deviceId !== 'string' || deviceId === '') {
        throw new Error(
          `hostler: ${claim.resource?.display ?? 'the device'} has no agent_device_id ` +
            '(is it connected?); the hold was released',
        );
      }
      req.log(`holding ${claim.resource.display} (${deviceId}) as ${claim.id}`);
      if (req.signal.aborted) throw aborted(req.signal);
      roots.set(claim.id, req.projectRoot);
      // No `device`: e2e would select by that name first.
      return { id: claim.id, deviceId, client: {} };
    } catch (cause) {
      await selfRelease(claim.id, req, cause);
      throw cause;
    }
  }

  async function selfRelease(id, req, cause) {
    try {
      await releaseClaim(id, req.projectRoot, req.env, AbortSignal.timeout(SELF_RELEASE_MS), req.log);
    } catch (cleanup) {
      throw new AggregateError(
        [cause, cleanup],
        `${messageOf(cause)}; releasing hold ${id} also failed: ${messageOf(cleanup)} ` +
          `(hostler release ${id})`,
      );
    }
  }

  async function release(lease, ctx) {
    await releaseClaim(lease.id, roots.get(lease.id) ?? process.cwd(), ctx.env, ctx.signal, ctx.log);
  }

  async function releaseClaim(id, cwd, env, signal, log) {
    const ran = await run(hostler, ['release', id, '--json'], {
      cwd,
      env: holdEnv(env),
      signal,
      log,
      killGraceMs: RELEASE_KILL_GRACE_MS,
    });
    // A claim that already ended (released, expired, taken over) answers ok too.
    if (ran.code === 0) return;
    if (signal.aborted) throw aborted(signal, `hostler release ${id} did not finish in time`);
    throw failure(`hostler release ${id}`, ran, parseEnvelope(ran.stdout));
  }

  return { name: 'hostler', acquire, release };
}

/**
 * The run's environment for a hostler command, without `HOSTLER_CLAIM_TOKENS`: the provider
 * never joins a claim of an enclosing `hostler run` (P2-6).
 */
function holdEnv(env) {
  const merged = { ...process.env, ...env };
  for (const name of Object.keys(merged)) {
    // Windows names are case-insensitive.
    if (name.toUpperCase() === 'HOSTLER_CLAIM_TOKENS') delete merged[name];
  }
  return merged;
}

/**
 * Runs one hostler command to its end. Its stderr lines (waiting lines, warnings) go to `log`.
 * On `signal` the command is stopped (on Unix SIGTERM, so a waiting hold withdraws its request),
 * and still awaited: a claim it printed before it stopped must not be lost. `killGraceMs` after
 * the stop it is killed outright; what it wrote until then is still read, and the run settles
 * (`code: null`) within `KILL_SETTLE_MS`, whatever still holds its output open.
 */
function run(command, args, { cwd, env, signal, log, killGraceMs }) {
  return new Promise((resolve, reject) => {
    let child;
    try {
      child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    } catch (e) {
      reject(notStarted(command, e));
      return;
    }
    let stdout = '';
    let stderr = '';
    let partial = '';
    child.stdout.setEncoding('utf8').on('data', (text) => {
      stdout += text;
    });
    child.stderr.setEncoding('utf8').on('data', (text) => {
      stderr += text;
      const lines = (partial + text).split(/\r?\n|\r/);
      partial = lines.pop();
      for (const line of lines) say(log, line);
    });
    let killer;
    let settled = false;
    const settle = (code) => {
      if (settled) return;
      settled = true;
      signal.removeEventListener('abort', stop);
      clearTimeout(killer);
      say(log, partial);
      resolve({ code, stdout, stderr });
    };
    const stop = () => {
      child.kill();
      killer = setTimeout(async () => {
        child.kill('SIGKILL');
        // A claim the hold printed just before it died may still sit in the pipe: read it.
        const exited = child.exitCode !== null || child.signalCode !== null;
        if (!exited) await within(new Promise((r) => child.once('exit', r)), KILL_EXIT_MS);
        if (!child.stdout.readableEnded) {
          await within(new Promise((r) => child.stdout.once('end', r)), KILL_DRAIN_MS);
        }
        if (settled) return;
        // A grandchild may keep the pipes open: the bound holds without them.
        child.stdout.destroy();
        child.stderr.destroy();
        settle(null);
      }, killGraceMs);
    };
    if (signal.aborted) stop();
    else signal.addEventListener('abort', stop, { once: true });
    child.on('error', (e) => {
      if (settled) return;
      settled = true;
      signal.removeEventListener('abort', stop);
      clearTimeout(killer);
      reject(notStarted(command, e));
    });
    child.on('close', settle);
  });
}

/** `promise`, or nothing once `ms` have passed. */
function within(promise, ms) {
  let timer;
  return Promise.race([promise, new Promise((r) => (timer = setTimeout(r, ms)))]).finally(() =>
    clearTimeout(timer),
  );
}

/** A progress line for the run's reporter; a reporter that fails never stops a hostler command. */
function say(log, line) {
  const text = line.trim();
  if (text === '') return;
  try {
    log(text);
  } catch {
    // Progress only.
  }
}

/** stdout as a hostler `--json` envelope (the whole of it, else its last line), or `undefined`. */
function parseEnvelope(stdout) {
  const text = stdout.trim();
  for (const candidate of [text, text.split('\n').pop() ?? '']) {
    try {
      const value = JSON.parse(candidate);
      if (typeof value === 'object' && value !== null && typeof value.ok === 'boolean') return value;
    } catch {
      // Not this one.
    }
  }
  return undefined;
}

/** A failed hostler command: its error envelope's message and recovery, else its stderr. */
function failure(what, ran, envelope) {
  const error = envelope?.ok === false ? envelope.error : undefined;
  let text;
  if (typeof error?.message === 'string') {
    text = `hostler: ${error.message}`;
    if (typeof error.recovery === 'string' && error.recovery !== '') text += `. To fix: ${error.recovery}`;
  } else {
    const detail = ran.stderr.trim().split(/\r?\n/).pop() || 'no answer';
    text = `${what} exited with ${ran.code ?? 'a signal'}: ${detail}`;
  }
  const e = new Error(text);
  e.code = error?.code;
  e.recovery = error?.recovery ?? undefined;
  e.exitCode = ran.code;
  return e;
}

function notStarted(command, cause) {
  const hint =
    cause?.code === 'ENOENT'
      ? `: ${command} was not found (install hostler, or pass hostlerProvider({ hostler: '<path>' }))`
      : `: ${messageOf(cause)}`;
  return new Error(`hostler could not start${hint}`, { cause });
}

function aborted(signal, text = 'the device request was withdrawn (aborted)') {
  const e = new DOMException(`hostler: ${text}`, 'AbortError');
  if (signal.reason !== undefined) Object.defineProperty(e, 'cause', { value: signal.reason });
  return e;
}

function messageOf(e) {
  return e instanceof Error ? e.message : String(e);
}
