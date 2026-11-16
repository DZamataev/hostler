//! Device telemetry polling (P3b Task 8): registered, connected devices only (P-9); one call on
//! the Probe lane per device; quiet for the first 30 s of a claim, then every 5 min.

use std::ffi::OsStr;
use std::time::Duration;

use super::android;
use super::service::{DevicesService, every, test_ms};
use super::tools::{self, Tool};
use crate::contract::claims::ResourceId;
use crate::contract::devices::{DeviceInfo, DeviceKind, Platform, Telemetry, TransportKind};

/// Debug builds: the quiet time after a claim starts, in ms (tests).
pub const TEST_QUIET_MS_ENV: &str = "HOSTLER_TEST_TELEMETRY_QUIET_MS";
const ANDROID_EVERY: Duration = Duration::from_secs(60);
const IOS_EVERY: Duration = Duration::from_secs(120);
const CLAIMED_EVERY: Duration = Duration::from_secs(5 * 60);
const QUIET: Duration = Duration::from_secs(30);
const ADB_TELEMETRY_BOUND: Duration = Duration::from_secs(10);

/// The intervals in effect (the test hooks shorten them).
#[derive(Clone, Copy, Debug)]
struct Schedule {
    android_ms: i64,
    ios_ms: i64,
    claimed_ms: i64,
    quiet_ms: i64,
}

impl Schedule {
    fn now() -> Schedule {
        let ms = |d: Duration| d.as_millis() as i64;
        Schedule {
            android_ms: ms(every(ANDROID_EVERY)),
            ios_ms: ms(every(IOS_EVERY)),
            claimed_ms: ms(every(CLAIMED_EVERY)),
            quiet_ms: test_ms(TEST_QUIET_MS_ENV).map_or(ms(QUIET), |q| q as i64),
        }
    }
}

/// Registered, connected devices whose telemetry is due.
pub fn due(svc: &DevicesService, now_ms: i64) -> Vec<ResourceId> {
    let schedule = Schedule::now();
    let view = svc.claims_view();
    let tried = svc.book().lock().telemetry_tried.clone();
    let ideviceinfo = svc.tools().ideviceinfo.is_some();
    svc.cache()
        .snapshot()
        .devices
        .into_iter()
        .filter(|d| polled(d, ideviceinfo))
        .filter(|d| {
            let last = d.telemetry.as_ref().map(|t| t.at_ms);
            let last = last.max(tried.get(&d.resource).copied());
            let claim_since = view
                .on_device
                .get(&d.resource)
                .and_then(|claims| claims.iter().map(|c| c.since_ms).max());
            telemetry_due(d.platform, claim_since, last, now_ms, &schedule)
        })
        .map(|d| d.resource)
        .collect()
}

/// Android devices (an emulator reads as emulated), and wired iPhones while `ideviceinfo` is
/// there (`ios_battery` checks the pairing and the record); never a simulator.
fn polled(d: &DeviceInfo, ideviceinfo: bool) -> bool {
    let wired = d
        .transports
        .first()
        .is_some_and(|t| t.kind == TransportKind::Wired);
    d.registered
        && d.connected
        && !d.shutting_down
        && !d.removing
        && match (d.platform, d.kind) {
            (Platform::Android, _) => true,
            (Platform::Ios, DeviceKind::Physical) => wired && ideviceinfo,
            (Platform::Ios, _) => false,
        }
}

/// The schedule alone: during a claim nothing in the quiet time after its (latest) start, then
/// every 5 min; else every 60 s (Android) or 120 s (iOS).
fn telemetry_due(
    platform: Platform,
    claim_since: Option<i64>,
    last_ms: Option<i64>,
    now_ms: i64,
    s: &Schedule,
) -> bool {
    let every = match claim_since {
        Some(since) if now_ms < since + s.quiet_ms => return false,
        Some(_) => s.claimed_ms,
        None if platform == Platform::Ios => s.ios_ms,
        None => s.android_ms,
    };
    last_ms.is_none_or(|last| now_ms - last >= every)
}

/// One reading (Probe lane); `None` when the call failed or the device is not asked.
pub fn poll_one(svc: &DevicesService, r: &ResourceId) -> Option<Telemetry> {
    let now = svc.now();
    svc.book().lock().telemetry_tried.insert(r.clone(), now);
    let info = svc.cache().info(r)?;
    match (info.platform, info.kind) {
        (Platform::Android, DeviceKind::Emulator) => Some(android::parse_telemetry("", true, now)),
        (Platform::Android, _) => {
            let adb = svc.tools().adb?;
            let serial = &info.transports.first()?.address;
            let args = [
                OsStr::new("-s"),
                OsStr::new(serial),
                OsStr::new("shell"),
                OsStr::new(tools::TELEMETRY),
            ];
            let out = svc
                .call(Tool::Adb, &adb, &args, ADB_TELEMETRY_BOUND)
                .filter(|o| o.status == Some(0) && !o.timed_out && !o.truncated)?;
            Some(android::parse_telemetry(
                &String::from_utf8_lossy(&out.stdout),
                false,
                svc.now(),
            ))
        }
        (Platform::Ios, DeviceKind::Physical) => svc.ios_battery(r),
        (Platform::Ios, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::devices::Transport;

    const S: Schedule = Schedule {
        android_ms: 60_000,
        ios_ms: 120_000,
        claimed_ms: 300_000,
        quiet_ms: 30_000,
    };

    #[test]
    fn telemetry_due_rules() {
        let android = Platform::Android;
        // Idle: first at once, then every 60 s (Android) or 120 s (iOS).
        assert!(telemetry_due(android, None, None, 0, &S));
        assert!(!telemetry_due(android, None, Some(0), 59_999, &S));
        assert!(telemetry_due(android, None, Some(0), 60_000, &S));
        assert!(!telemetry_due(Platform::Ios, None, Some(0), 119_999, &S));
        assert!(telemetry_due(Platform::Ios, None, Some(0), 120_000, &S));
        // A claim started at 100 s: nothing for 30 s, even never read.
        assert!(!telemetry_due(android, Some(100_000), None, 129_999, &S));
        assert!(telemetry_due(android, Some(100_000), None, 130_000, &S));
        // Then every 5 min from the last reading.
        assert!(!telemetry_due(
            android,
            Some(100_000),
            Some(130_000),
            429_999,
            &S
        ));
        assert!(telemetry_due(
            android,
            Some(100_000),
            Some(130_000),
            430_000,
            &S
        ));

        // Who is polled: registered, connected Android; wired iPhones with ideviceinfo; never
        // simulators.
        let mut d = crate::devices::installs::tests::device(android, DeviceKind::Physical);
        assert!(polled(&d, false));
        d.registered = false;
        assert!(!polled(&d, true));
        d.registered = true;
        d.connected = false;
        assert!(!polled(&d, true));
        let mut phone =
            crate::devices::installs::tests::device(Platform::Ios, DeviceKind::Physical);
        phone.transports = vec![Transport {
            kind: TransportKind::Wired,
            address: "C0".into(),
            state: "connected".into(),
        }];
        assert!(polled(&phone, true));
        assert!(!polled(&phone, false), "no ideviceinfo");
        phone.transports[0].kind = TransportKind::Network;
        assert!(!polled(&phone, true), "not wired");
        let sim = crate::devices::installs::tests::device(Platform::Ios, DeviceKind::Simulator);
        assert!(!polled(&sim, true));
    }
}
