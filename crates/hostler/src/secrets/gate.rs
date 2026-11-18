//! The person gate (P5, spec §8.5): a master-password proof per connection and the
//! service-wide limit on password checks.
//!
//! This is a proof of intent, not authentication of the app: an agent that does not know the
//! master password cannot use a gated method, and the service still cannot tell one same-user
//! client from another (spec §8.2).
//!
//! **The grant.** A successful `secrets.prove` or `secrets.unlock` hands the server a
//! [`GrantMeta`] for its connection. A gated method is admitted by [`admit`] only when the grant
//! has not expired and was made in the vault's current unlock generation; the engine runs that
//! check under the vault mutex, in the same section as the method's work.
//!
//! **The limit on guessing** ([`PasswordLimiter`]), one per service, over prove and unlock:
//! - at most one check at a time (`in_flight`); another is refused at once;
//! - after `n` failures in a row, a check waits `min(2^(min(n,10)−1), 300)` units after the last
//!   failure (the unit is 1 s; debug builds may scale it for tests);
//! - the count and the last failure's wall time live in the store's `password_limit` row, read
//!   at every check, so neither a disconnect nor a restart resets them. Before a check runs,
//!   the row is written as if it failed (`n + 1`, now): a store that cannot be written refuses
//!   the check (fail closed), and a service killed mid-check keeps the failure;
//! - the clocks (spec §8.5 "clock changes"): a stored time later than now counts as now (and is
//!   written back), so a wait never exceeds the cap; while the service runs, the last failure is
//!   also kept on a monotonic clock and a check needs both waits to have passed; a loaded row is
//!   sanitized (count clamped to `0..=64`; a count without a valid time counts as now).
//!
//! There is no bypass: no method, flag or (release-build) environment variable skips it.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::contract::secrets::errors;
use crate::contract::{ErrorCode, HostlerError};

/// How long a proof or an unlock grants its connection.
pub const PROOF_TTL_MS: i64 = 120_000;

/// The stored failure count is kept within `0..=MAX_FAILURES`.
pub const MAX_FAILURES: i64 = 64;

/// The exponent stops growing here: `2^(10−1)` units is already over the cap.
const EXPONENT_CAP: i64 = 10;

/// The longest wait, in units (5 min at the 1 s unit).
const CAP_UNITS: u64 = 300;

/// What a refusal while another check runs suggests: about one key derivation.
const BUSY_RETRY_MS: u64 = 1000;

/// What a successful `secrets.prove` or `secrets.unlock` hands the server for its connection
/// (R2-3). Internal: never on the wire. The server installs it only after a successful answer
/// that carries one, and clears the connection's grant after a failed prove or unlock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantMeta {
    pub expires_at_ms: i64,
    /// The vault's unlock generation when the grant was made.
    pub generation_at_grant: u64,
}

/// The plan's name for the grant.
pub type Grant = GrantMeta;

/// The admission of a gated method: a live grant of the current generation, else
/// `proof_required`. The engine calls it under the vault mutex with the vault's generation.
pub fn admit(grant: Option<&GrantMeta>, now_ms: i64, generation: u64) -> Result<(), HostlerError> {
    match grant {
        Some(g) if now_ms < g.expires_at_ms && g.generation_at_grant == generation => Ok(()),
        _ => Err(errors::proof_required()),
    }
}

/// How long a grant lasts: `PROOF_TTL_MS`; debug builds read `HOSTLER_TEST_PROVE_MS`.
pub fn proof_ttl_ms() -> i64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_PROVE_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|ms| *ms > 0)
    {
        return ms;
    }
    PROOF_TTL_MS
}

/// The wall clock, in Unix milliseconds (injected for tests).
pub trait WallClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// A monotonic clock: time since an arbitrary origin, never going back (injected for tests).
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// The process's monotonic clock.
pub struct SystemMonotonic(Instant);

impl SystemMonotonic {
    pub fn new() -> SystemMonotonic {
        SystemMonotonic(Instant::now())
    }
}

impl Default for SystemMonotonic {
    fn default() -> SystemMonotonic {
        SystemMonotonic::new()
    }
}

impl MonotonicClock for SystemMonotonic {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Where the count and the last failure's time are kept: the store's `password_limit` row.
pub trait LimitRow {
    /// `(failures, last_failure_at_ms)` as stored; `None` when there is no row.
    fn load(&self) -> anyhow::Result<Option<(i64, Option<i64>)>>;
    fn save(&self, failures: i64, last_failure_at_ms: Option<i64>) -> anyhow::Result<()>;
}

impl LimitRow for crate::store::Store {
    fn load(&self) -> anyhow::Result<Option<(i64, Option<i64>)>> {
        self.password_limit()
    }

    fn save(&self, failures: i64, last_failure_at_ms: Option<i64>) -> anyhow::Result<()> {
        self.set_password_limit(failures, last_failure_at_ms)
    }
}

/// Which method checks a password (named in the failure log line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Prove,
    Unlock,
}

impl fmt::Display for CheckKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CheckKind::Prove => "prove",
            CheckKind::Unlock => "unlock",
        })
    }
}

/// How a check ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The password opened the database: the count resets.
    Success,
    /// The password did not open it: the count grows.
    Failure,
    /// No password was compared (no database, an unreadable key file or database file): the
    /// row goes back to what it was.
    Neutral,
}

/// The count after sanitizing a stored row at `now` (spec §8.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub failures: i64,
    pub last_failure_at_ms: Option<i64>,
}

impl Row {
    /// The count clamped to `0..=MAX_FAILURES`; with a count, a missing, negative or future time
    /// counts as `now`.
    pub fn sanitized(stored: Option<(i64, Option<i64>)>, now_ms: i64) -> Row {
        let Some((failures, last)) = stored else {
            return Row {
                failures: 0,
                last_failure_at_ms: None,
            };
        };
        let failures = failures.clamp(0, MAX_FAILURES);
        let last_failure_at_ms = if failures == 0 {
            last
        } else {
            match last {
                Some(t) if (0..=now_ms).contains(&t) => Some(t),
                _ => Some(now_ms),
            }
        };
        Row {
            failures,
            last_failure_at_ms,
        }
    }
}

/// The wait after `failures` failures in a row: `min(2^(min(n,10)−1), 300)` units, in ms, with
/// saturating integer math; 0 without a failure.
pub fn backoff_ms(failures: i64, unit_ms: u64) -> u64 {
    if failures <= 0 {
        return 0;
    }
    let exponent = failures.min(EXPONENT_CAP) - 1;
    let units = 1u64.checked_shl(exponent as u32).unwrap_or(u64::MAX);
    units.min(CAP_UNITS).saturating_mul(unit_ms)
}

/// The backoff unit: 1 s; debug builds read `HOSTLER_TEST_PASSWORD_BACKOFF_SCALE_MS`.
pub fn backoff_unit_ms() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_PASSWORD_BACKOFF_SCALE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        return ms;
    }
    1000
}

struct LimiterState {
    /// A check holds the permit.
    in_flight: bool,
    /// The last failure in this process, on the monotonic clock.
    last_failure_mono: Option<Duration>,
}

/// The service-wide limit on master-password checks (spec §8.5). See the module comment.
pub struct PasswordLimiter {
    state: Mutex<LimiterState>,
    wall: Box<dyn WallClock>,
    mono: Box<dyn MonotonicClock>,
    unit_ms: u64,
}

/// One admitted check. Finish it with [`Permit::finish`]; dropped unfinished, it counts as a
/// failure (the row written before the check stays).
pub struct Permit<'a> {
    limiter: &'a PasswordLimiter,
    kind: CheckKind,
    peer_pid: Option<u32>,
    /// The sanitized row before the check.
    before: Row,
    finished: bool,
}

fn store_failed(e: &anyhow::Error) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!(
            "the hostler store failed, so no master-password check runs (nothing was unlocked or \
             granted): {e:#}"
        ),
        details: json!({}),
        recovery: None,
    }
}

fn pid_text(pid: Option<u32>) -> String {
    pid.map_or_else(|| "unknown".to_owned(), |p| p.to_string())
}

impl PasswordLimiter {
    pub fn new(
        wall: Box<dyn WallClock>,
        mono: Box<dyn MonotonicClock>,
        unit_ms: u64,
    ) -> PasswordLimiter {
        PasswordLimiter {
            state: Mutex::new(LimiterState {
                in_flight: false,
                last_failure_mono: None,
            }),
            wall,
            mono,
            unit_ms: unit_ms.max(1),
        }
    }

    fn state(&self) -> MutexGuard<'_, LimiterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one check, or refuses it with `proof_limited` (another runs, or the backoff has
    /// not passed) or `Internal` (the store cannot be read or written). On admission the row
    /// already counts this check as a failure.
    pub fn begin(
        &self,
        kind: CheckKind,
        peer_pid: Option<u32>,
        row: &dyn LimitRow,
    ) -> Result<Permit<'_>, HostlerError> {
        let mut state = self.state();
        if state.in_flight {
            return Err(errors::proof_busy(BUSY_RETRY_MS.max(self.unit_ms)));
        }
        let now = self.wall.now_ms();
        let stored = row.load().map_err(|e| store_failed(&e))?;
        let before = Row::sanitized(stored, now);
        if stored != Some((before.failures, before.last_failure_at_ms)) && stored.is_some() {
            row.save(before.failures, before.last_failure_at_ms)
                .map_err(|e| store_failed(&e))?;
        }
        let wait = backoff_ms(before.failures, self.unit_ms);
        if wait > 0 {
            let wall_left = before
                .last_failure_at_ms
                .map(|t| t.saturating_add_unsigned(wait).saturating_sub(now).max(0) as u64)
                .unwrap_or(0);
            let mono_left = state
                .last_failure_mono
                .map(|m| {
                    (m.saturating_add(Duration::from_millis(wait)))
                        .saturating_sub(self.mono.now())
                        .as_millis() as u64
                })
                .unwrap_or(0);
            let left = wall_left.max(mono_left);
            if left > 0 {
                return Err(errors::proof_limited(left));
            }
        }
        let pessimistic = (before.failures + 1).min(MAX_FAILURES);
        row.save(pessimistic, Some(now))
            .map_err(|e| store_failed(&e))?;
        state.in_flight = true;
        Ok(Permit {
            limiter: self,
            kind,
            peer_pid,
            before,
            finished: false,
        })
    }

    /// Debug/test view: whether a check holds the permit.
    #[cfg(test)]
    fn in_flight(&self) -> bool {
        self.state().in_flight
    }
}

impl Permit<'_> {
    /// Records how the check ended. A store write that fails here is logged: the row written
    /// before the check (one more failure) stays, which only errs toward waiting.
    pub fn finish(mut self, outcome: Outcome, row: Option<&dyn LimitRow>) {
        self.finished = true;
        let limiter = self.limiter;
        let mut state = limiter.state();
        state.in_flight = false;
        let saved = match outcome {
            Outcome::Success => {
                state.last_failure_mono = None;
                row.map(|r| r.save(0, None))
            }
            Outcome::Failure => {
                let failures = (self.before.failures + 1).min(MAX_FAILURES);
                state.last_failure_mono = Some(limiter.mono.now());
                let wait = backoff_ms(failures, limiter.unit_ms);
                tracing::warn!(
                    "wrong master password ({}) from peer pid {}; next try in {} s",
                    self.kind,
                    pid_text(self.peer_pid),
                    wait.div_ceil(1000)
                );
                row.map(|r| r.save(failures, Some(limiter.wall.now_ms())))
            }
            Outcome::Neutral => {
                row.map(|r| r.save(self.before.failures, self.before.last_failure_at_ms))
            }
        };
        match saved {
            Some(Ok(())) => {}
            Some(Err(e)) => tracing::warn!(
                "cannot record the master-password check in the store (the stored count stays \
                 one failure higher): {e:#}"
            ),
            None => tracing::warn!(
                "cannot reach the store to record the master-password check (the stored count \
                 stays one failure higher)"
            ),
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self.limiter.state();
            state.in_flight = false;
            state.last_failure_mono = Some(self.limiter.mono.now());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

    use super::*;

    const UNIT: u64 = 1000;
    const HOUR_MS: i64 = 3_600_000;

    #[derive(Clone, Default)]
    struct Clocks {
        wall: Arc<AtomicI64>,
        mono: Arc<AtomicU64>,
    }

    struct Wall(Arc<AtomicI64>);
    impl WallClock for Wall {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Mono(Arc<AtomicU64>);
    impl MonotonicClock for Mono {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    impl Clocks {
        fn new() -> Clocks {
            let c = Clocks::default();
            c.wall.store(1_000_000_000, Ordering::SeqCst);
            c.mono.store(5_000, Ordering::SeqCst);
            c
        }
        fn limiter(&self) -> PasswordLimiter {
            PasswordLimiter::new(
                Box::new(Wall(self.wall.clone())),
                Box::new(Mono(self.mono.clone())),
                UNIT,
            )
        }
        fn now(&self) -> i64 {
            self.wall.load(Ordering::SeqCst)
        }
        /// Both clocks move.
        fn advance(&self, ms: u64) {
            self.wall.fetch_add(ms as i64, Ordering::SeqCst);
            self.mono.fetch_add(ms, Ordering::SeqCst);
        }
        fn wall_jump(&self, ms: i64) {
            self.wall.fetch_add(ms, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct FakeRow {
        row: Mutex<Option<(i64, Option<i64>)>>,
        fail_writes: AtomicBool,
        fail_reads: AtomicBool,
    }

    impl LimitRow for FakeRow {
        fn load(&self) -> anyhow::Result<Option<(i64, Option<i64>)>> {
            if self.fail_reads.load(Ordering::SeqCst) {
                anyhow::bail!("read failed");
            }
            Ok(*self.row.lock().unwrap())
        }
        fn save(&self, failures: i64, last: Option<i64>) -> anyhow::Result<()> {
            if self.fail_writes.load(Ordering::SeqCst) {
                anyhow::bail!("attempt to write a readonly database");
            }
            *self.row.lock().unwrap() = Some((failures, last));
            Ok(())
        }
    }

    impl FakeRow {
        fn with(failures: i64, last: Option<i64>) -> FakeRow {
            let row = FakeRow::default();
            *row.row.lock().unwrap() = Some((failures, last));
            row
        }
        fn get(&self) -> Option<(i64, Option<i64>)> {
            *self.row.lock().unwrap()
        }
    }

    fn fail(l: &PasswordLimiter, row: &FakeRow) {
        l.begin(CheckKind::Prove, Some(7), row)
            .expect("admitted")
            .finish(Outcome::Failure, Some(row));
    }

    fn limited_ms(r: Result<Permit<'_>, HostlerError>) -> u64 {
        let e = r.err().expect("refused");
        assert_eq!(e.code, ErrorCode::ProofLimited, "{}", e.message);
        e.details["retry_after_ms"].as_u64().unwrap()
    }

    #[test]
    fn backoff_follows_the_schedule_and_caps() {
        assert_eq!(backoff_ms(0, UNIT), 0);
        assert_eq!(backoff_ms(-3, UNIT), 0);
        assert_eq!(backoff_ms(1, UNIT), 1_000);
        assert_eq!(backoff_ms(2, UNIT), 2_000);
        assert_eq!(backoff_ms(3, UNIT), 4_000);
        assert_eq!(backoff_ms(9, UNIT), 256_000);
        assert_eq!(backoff_ms(10, UNIT), 300_000);
        assert_eq!(backoff_ms(64, UNIT), 300_000);
    }

    #[test]
    fn exponent_saturates() {
        assert_eq!(backoff_ms(64, UNIT), 300_000);
        assert_eq!(backoff_ms(i64::MAX, UNIT), 300_000);
        assert_eq!(backoff_ms(i64::MAX, u64::MAX), u64::MAX);
        let c = Clocks::new();
        let row = FakeRow::with(64, Some(c.now()));
        let l = c.limiter();
        assert_eq!(limited_ms(l.begin(CheckKind::Unlock, None, &row)), 300_000);
        c.advance(300_000);
        fail(&l, &row);
        assert_eq!(
            row.get(),
            Some((64, Some(c.now()))),
            "the count stays at 64"
        );
    }

    #[test]
    fn failures_wait_and_success_resets() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        fail(&l, &row);
        assert_eq!(row.get(), Some((1, Some(c.now()))));
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 1_000);
        c.advance(999);
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 1);
        c.advance(1);
        fail(&l, &row);
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 2_000);
        c.advance(2_000);
        fail(&l, &row);
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 4_000);
        c.advance(4_000);
        l.begin(CheckKind::Unlock, None, &row)
            .unwrap()
            .finish(Outcome::Success, Some(&row));
        assert_eq!(row.get(), Some((0, None)));
        // The next check runs at once.
        l.begin(CheckKind::Prove, None, &row)
            .unwrap()
            .finish(Outcome::Success, Some(&row));
    }

    #[test]
    fn one_check_at_a_time() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        let permit = l.begin(CheckKind::Prove, None, &row).unwrap();
        // Counted before the check: a kill now keeps the failure.
        assert_eq!(row.get(), Some((1, Some(c.now()))));
        let e = l.begin(CheckKind::Unlock, None, &row).err().unwrap();
        assert_eq!(e.code, ErrorCode::ProofLimited);
        assert!(
            e.message
                .starts_with("another master-password check is running; next try in 1 s"),
            "refused as busy, not by the backoff: {}",
            e.message
        );
        assert!(e.details["retry_after_ms"].as_u64().unwrap() >= 1000);
        assert!(l.in_flight());
        permit.finish(Outcome::Neutral, Some(&row));
        assert!(!l.in_flight());
        assert_eq!(
            row.get(),
            Some((0, None)),
            "a neutral check restores the row"
        );
    }

    #[test]
    fn a_dropped_permit_counts_as_a_failure() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        drop(l.begin(CheckKind::Prove, None, &row).unwrap());
        assert!(!l.in_flight());
        assert_eq!(row.get(), Some((1, Some(c.now()))));
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 1_000);
    }

    #[test]
    fn clock_back_an_hour() {
        let c = Clocks::new();
        // A failure stored an hour ahead of now: the clock went back.
        let row = FakeRow::with(5, Some(c.now() + HOUR_MS));
        let l = c.limiter();
        let wait = limited_ms(l.begin(CheckKind::Prove, None, &row));
        assert_eq!(wait, 16_000, "counted from now, never an hour");
        assert!(wait <= 300_000);
        assert_eq!(row.get(), Some((5, Some(c.now()))), "t rewritten to now");
        // Also with a capped count: at most the cap.
        let row = FakeRow::with(40, Some(c.now() + 10 * HOUR_MS));
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 300_000);
    }

    #[test]
    fn clock_back_while_running_caps_the_wait() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        for _ in 0..3 {
            c.advance(300_000);
            fail(&l, &row);
        }
        c.wall_jump(-HOUR_MS);
        c.mono.fetch_add(1_000, Ordering::SeqCst);
        let wait = limited_ms(l.begin(CheckKind::Prove, None, &row));
        assert_eq!(
            wait, 4_000,
            "the future time counts as now: one full wait, not an hour"
        );
        assert_eq!(row.get(), Some((3, Some(c.now()))));
    }

    #[test]
    fn clock_forward_while_running() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        for _ in 0..5 {
            c.advance(300_000);
            fail(&l, &row);
        }
        // Wall +1 h, monotonic +1 s: still 15 s to wait.
        c.wall_jump(HOUR_MS);
        c.mono.fetch_add(1_000, Ordering::SeqCst);
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 15_000);
        c.mono.fetch_add(15_000, Ordering::SeqCst);
        l.begin(CheckKind::Prove, None, &row)
            .unwrap()
            .finish(Outcome::Success, Some(&row));
    }

    #[test]
    fn restart_keeps_the_wait() {
        let c = Clocks::new();
        let row = FakeRow::default();
        {
            let l = c.limiter();
            for _ in 0..3 {
                c.advance(300_000);
                fail(&l, &row);
            }
        }
        c.advance(1_000);
        // A new limiter (a restarted service) over the same row.
        let l = c.limiter();
        assert_eq!(limited_ms(l.begin(CheckKind::Unlock, None, &row)), 3_000);
        c.advance(3_000);
        assert!(l.begin(CheckKind::Unlock, None, &row).is_ok());
    }

    #[test]
    fn bad_rows_are_sanitized() {
        let now = 1_000_000;
        let s = |f, t| Row::sanitized(Some((f, t)), now);
        assert_eq!(
            s(-5, Some(now - 10)),
            Row {
                failures: 0,
                last_failure_at_ms: Some(now - 10)
            }
        );
        assert_eq!(
            s(i64::MAX, Some(now - 10)),
            Row {
                failures: 64,
                last_failure_at_ms: Some(now - 10)
            }
        );
        assert_eq!(
            s(3, None),
            Row {
                failures: 3,
                last_failure_at_ms: Some(now)
            }
        );
        assert_eq!(
            s(3, Some(-1)),
            Row {
                failures: 3,
                last_failure_at_ms: Some(now)
            }
        );
        assert_eq!(
            s(3, Some(i64::MAX)),
            Row {
                failures: 3,
                last_failure_at_ms: Some(now)
            }
        );
        assert_eq!(
            Row::sanitized(None, now),
            Row {
                failures: 0,
                last_failure_at_ms: None
            }
        );

        // Through the limiter: the sanitized row is written back before the wait is computed.
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::with(i64::MAX, None);
        assert_eq!(limited_ms(l.begin(CheckKind::Prove, None, &row)), 300_000);
        assert_eq!(row.get(), Some((64, Some(c.now()))));
        let row = FakeRow::with(-7, Some(-7));
        l.begin(CheckKind::Prove, None, &row)
            .unwrap()
            .finish(Outcome::Failure, Some(&row));
        assert_eq!(row.get(), Some((1, Some(c.now()))));
    }

    #[test]
    fn store_failure_fails_closed() {
        let c = Clocks::new();
        let l = c.limiter();
        let row = FakeRow::default();
        row.fail_writes.store(true, Ordering::SeqCst);
        let e = l.begin(CheckKind::Prove, None, &row).err().unwrap();
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(!l.in_flight(), "nothing runs");
        row.fail_writes.store(false, Ordering::SeqCst);
        row.fail_reads.store(true, Ordering::SeqCst);
        assert_eq!(
            l.begin(CheckKind::Prove, None, &row).err().unwrap().code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn admit_checks_expiry_and_generation() {
        let g = GrantMeta {
            expires_at_ms: 2_000,
            generation_at_grant: 4,
        };
        assert!(admit(Some(&g), 1_999, 4).is_ok());
        assert_eq!(
            admit(Some(&g), 2_000, 4).unwrap_err().code,
            ErrorCode::ProofRequired
        );
        assert_eq!(
            admit(Some(&g), 1_000, 5).unwrap_err().code,
            ErrorCode::ProofRequired
        );
        assert_eq!(
            admit(None, 0, 0).unwrap_err().code,
            ErrorCode::ProofRequired
        );
    }
}
