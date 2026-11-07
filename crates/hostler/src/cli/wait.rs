//! Pure pieces of waiting in the heavy queue (plan Task 8): the status line, when to print it, and
//! when to give up. All waiting arithmetic uses `Instant`; wall clock only feeds `held` for display.

use std::path::Path;
use std::time::{Duration, Instant};

/// How often the waiting status is repeated when nothing changes.
pub const PRINT_EVERY: Duration = Duration::from_secs(30);

/// A running entry as shown to a waiter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub command: String,
    pub cwd: String,
    pub pid: u32,
    pub held: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitStatus {
    pub capacity: u8,
    pub position: u32,
    pub of: u32,
    pub running: Vec<Holder>,
    pub waited: Duration,
    /// `None` = no limit.
    pub max_wait: Option<Duration>,
}

/// `N min`, or `N s` under a minute.
pub fn duration_text(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs / 60)
    }
}

pub fn wait_line(p: &WaitStatus) -> String {
    let running = if p.running.is_empty() {
        "nothing yet".to_owned()
    } else {
        p.running
            .iter()
            .map(|h| {
                let dir = Path::new(&h.cwd)
                    .file_name()
                    .map_or_else(|| h.cwd.clone(), |n| n.to_string_lossy().into_owned());
                format!(
                    "{} ({dir}, PID {}, {})",
                    h.command,
                    h.pid,
                    duration_text(h.held)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let limit = p
        .max_wait
        .map_or_else(|| "no limit".to_owned(), duration_text);
    format!(
        "[hostler] Heavy queue (capacity {}): you are {} of {}. Running: {running}. Waited {} of \
         {limit} — this is waiting, not hanging; Ctrl+C withdraws the request.",
        p.capacity,
        p.position,
        p.of,
        duration_text(p.waited)
    )
}

/// Print on every change, else every 30 s.
pub fn should_print(last_print: Instant, now: Instant, changed: bool) -> bool {
    changed || now.saturating_duration_since(last_print) >= PRINT_EVERY
}

pub fn deadline_reached(started: Instant, now: Instant, max_wait: Option<Duration>) -> bool {
    max_wait.is_some_and(|limit| now.saturating_duration_since(started) >= limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(max_wait: Option<Duration>) -> WaitStatus {
        WaitStatus {
            capacity: 1,
            position: 2,
            of: 3,
            running: vec![Holder {
                command: "./gradlew assemble".into(),
                cwd: "/work/app".into(),
                pid: 4242,
                held: Duration::from_secs(185),
            }],
            waited: Duration::from_secs(42),
            max_wait,
        }
    }

    #[test]
    fn wait_line_shows_position_holder_and_limit() {
        assert_eq!(
            wait_line(&status(Some(Duration::from_secs(3600)))),
            "[hostler] Heavy queue (capacity 1): you are 2 of 3. Running: ./gradlew assemble \
             (app, PID 4242, 3 min). Waited 42 s of 60 min — this is waiting, not hanging; \
             Ctrl+C withdraws the request."
        );
    }

    #[test]
    fn wait_line_no_limit_and_several_holders() {
        let mut s = status(None);
        s.capacity = 2;
        s.waited = Duration::from_secs(60);
        s.running.push(Holder {
            command: "cargo build".into(),
            cwd: "/".into(),
            pid: 7,
            held: Duration::from_millis(59_999),
        });
        let line = wait_line(&s);
        assert!(line.contains("(capacity 2)"), "{line}");
        assert!(
            line.contains("(app, PID 4242, 3 min); cargo build (/, PID 7, 59 s)."),
            "{line}"
        );
        assert!(line.contains("Waited 1 min of no limit"), "{line}");
    }

    #[test]
    fn wait_line_without_holders() {
        let mut s = status(None);
        s.running.clear();
        assert!(wait_line(&s).contains("Running: nothing yet."));
    }

    #[test]
    fn durations_switch_to_minutes_at_60_s() {
        assert_eq!(duration_text(Duration::ZERO), "0 s");
        assert_eq!(duration_text(Duration::from_millis(59_999)), "59 s");
        assert_eq!(duration_text(Duration::from_secs(60)), "1 min");
        assert_eq!(duration_text(Duration::from_secs(119)), "1 min");
        assert_eq!(duration_text(Duration::from_secs(7200)), "120 min");
    }

    #[test]
    fn should_print_on_change_or_every_30_s() {
        let t = Instant::now();
        assert!(should_print(t, t, true));
        assert!(!should_print(t, t + Duration::from_secs(29), false));
        assert!(should_print(t, t + Duration::from_secs(30), false));
        // A clock that appears to go backwards never panics.
        assert!(!should_print(t + Duration::from_secs(5), t, false));
    }

    #[test]
    fn deadline_none_never_and_exact_limit_reached() {
        let t = Instant::now();
        let limit = Duration::from_secs(60);
        assert!(!deadline_reached(
            t,
            t + Duration::from_secs(1_000_000),
            None
        ));
        assert!(!deadline_reached(
            t,
            t + Duration::from_secs(59),
            Some(limit)
        ));
        assert!(deadline_reached(t, t + limit, Some(limit)));
        assert!(deadline_reached(
            t,
            t + Duration::from_secs(61),
            Some(limit)
        ));
    }
}
