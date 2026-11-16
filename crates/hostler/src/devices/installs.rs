//! App snapshots, their diff and install attribution (P3b Task 8).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::time::Duration;

use super::android;
use super::ios;
use super::service::{DevicesService, keys};
use super::tools::{self, Tool};
use super::windows::ClaimWindow;
use crate::contract::claims::ResourceId;
use crate::contract::devices::{
    AppState, DeviceInfo, DeviceKind, InstallChange, InstallEvent, InstallSource, InstallTime,
    Platform,
};
use crate::contract::wire::Event;

/// Every 30 min idle; every 10 min while an iOS claim is open (Android times come from the
/// device, so its claims do not shorten it).
const IDLE_EVERY_MS: i64 = 30 * 60 * 1000;
const IOS_CLAIMED_EVERY_MS: i64 = 10 * 60 * 1000;
/// A close snapshot attributes a change to its window only when it started this soon (P2-4).
const CLOSE_WITHIN_MS: i64 = 30_000;
/// Without both snapshots' conversions, Android update times are compared within this: the skew
/// moves by `date +%s`'s 1 s step plus the call's latency (Task 13 F-2).
const UNKNOWN_SHIFT_TOLERANCE_MS: i64 = 2_000;
/// Android install times have 1 s resolution.
const DEVICE_SLACK_MS: i64 = 1_000;
const ADB_SNAPSHOT_BOUND: Duration = Duration::from_secs(30);
const DEVICECTL_APPS_BOUND: Duration = Duration::from_secs(30);
const LISTAPPS_BOUND: Duration = Duration::from_secs(20);
const PLUTIL_BOUND: Duration = Duration::from_secs(5);
/// Simulator version names: at most this many `plutil` calls per snapshot; the rest `None`.
const MAX_VERSION_READS: usize = 50;
/// An owed snapshot (claim-open, close) whose read failed is tried again this long after the
/// failure, doubling per failure in a row up to 30 times it (R3-1, R3-2): first the Long pass's
/// own 10 s, so one transient failure of a close read is retried within `CLOSE_WITHIN_MS`; at
/// most every 5 min, so an unreadable device takes a Long slot for at most one bound (20-30 s)
/// in 5 min. Test-shortened with the passes.
const RETRY_FIRST: Duration = Duration::from_secs(10);
const RETRY_MAX_FACTOR: i64 = 30;

/// One good snapshot: when it finished, and the apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub at_ms: i64,
    pub apps: Vec<AppState>,
    /// Android: how much more this snapshot's time conversion (skew minus UTC offset) adds than
    /// the previous one's; `None` when either is unknown (Task 13 F-2).
    pub clock_shift_ms: Option<i64>,
}

/// A window's snapshots: its first one that started inside it, and its close snapshot (the
/// first that started after its end): when that one started and finished.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowSnapshots {
    pub first_in_window_ms: Option<i64>,
    pub close_started_ms: Option<i64>,
    pub close_at_ms: Option<i64>,
    /// Not a window row (R3-1): a device claim the windows pass has not recorded yet, from the
    /// engine's own records, or a row whose claim is neither live nor in history. It can make a
    /// change `Around`, never `Claim`.
    pub unrecorded: bool,
}

/// Registered, connected (iOS: reachable; virtual: booted) devices whose app snapshot is due:
/// by the schedule, or owed (the seed of a device never snapshotted, claim-open, or a close a
/// snapshot starting now could still book) and not backing off after a failed read.
pub fn due(svc: &DevicesService, now_ms: i64) -> Vec<ResourceId> {
    let view = svc.claims_view();
    let owed = claim_open_owed(svc);
    let devices: Vec<DeviceInfo> = svc
        .cache()
        .snapshot()
        .devices
        .into_iter()
        .filter(snapshottable)
        .collect();
    // Per device: its last attempt, whether an owed snapshot may be tried now, and whether a
    // close still waits that a snapshot starting now could attribute (R3-2).
    let marks: Vec<(Option<i64>, bool, bool)> = {
        let book = svc.book().lock();
        devices
            .iter()
            .map(|d| {
                let r = &d.resource;
                let may_retry = book
                    .snapshot_retry
                    .get(r)
                    .is_none_or(|(_, next)| now_ms >= *next);
                let closing = book
                    .pending_close_ms(r)
                    .is_some_and(|to| now_ms - to <= CLOSE_WITHIN_MS);
                (book.snapshot_tried.get(r).copied(), may_retry, closing)
            })
            .collect()
    };
    devices
        .into_iter()
        .zip(marks)
        .filter(|(d, (tried, may_retry, closing))| {
            let last = svc.store().device_snapshot_at(&d.resource).ok().flatten();
            // F-4: a device with no seed yet owes one, as soon as it can be read.
            if last.is_none() || *closing || owed.contains(&d.resource) {
                return *may_retry;
            }
            let last = last.max(*tried);
            let claimed = view.on_device.contains_key(&d.resource);
            snapshot_due(d.platform, claimed, last, now_ms)
        })
        .map(|(d, _)| d.resource)
        .collect()
}

/// How long after its `failures`-th failed read in a row an owed snapshot is tried again.
fn retry_after_ms(failures: u32, first_ms: i64) -> i64 {
    let doubled = 1i64 << failures.saturating_sub(1).min(5);
    first_ms.saturating_mul(doubled.min(RETRY_MAX_FACTOR))
}

/// iOS devices with an open claim window and no in-window snapshot yet (F-3: a claim granted
/// while its simulator was shut down or booting; the window's own snapshot found it not ready):
/// due as soon as they are snapshottable. An install before that snapshot reads its apps still
/// reads `Around`: there is no earlier in-window snapshot to bound it.
pub(crate) fn claim_open_owed(svc: &DevicesService) -> HashSet<ResourceId> {
    let open = svc.store().open_claim_windows().unwrap_or_default();
    let book = svc.book().lock();
    open.into_iter()
        .filter(|w| w.resource.0.starts_with("device:ios:") && !book.has_in_window(&w.claim_id))
        .map(|w| w.resource)
        .collect()
}

fn snapshottable(d: &DeviceInfo) -> bool {
    d.registered && d.connected && d.ready && !d.shutting_down && !d.removing
}

/// The schedule alone: never snapshotted is due at once.
fn snapshot_due(platform: Platform, claimed: bool, last_ms: Option<i64>, now_ms: i64) -> bool {
    let every = if platform == Platform::Ios && claimed {
        IOS_CLAIMED_EVERY_MS
    } else {
        IDLE_EVERY_MS
    };
    last_ms.is_none_or(|last| now_ms - last >= every)
}

/// One snapshot of `r` (Long lane), diffed against the last good one; each change is
/// attributed, recorded and emitted as `devices.installed`. A failed or truncated read changes
/// nothing.
pub fn snapshot_and_diff(svc: &DevicesService, r: &ResourceId) {
    let started = svc.now();
    // F-4: no read runs for a device that cannot be read (an AVD shut down, a phone offline),
    // so no attempt is recorded: it stays due until it can be.
    let Some(info) = svc.cache().info(r).filter(snapshottable) else {
        return;
    };
    svc.book().lock().snapshot_tried.insert(r.clone(), started);
    let Some((apps, clock)) = read_apps(svc, &info, started) else {
        let now = svc.now();
        let first_ms = super::service::every(RETRY_FIRST).as_millis() as i64;
        let mut book = svc.book().lock();
        let failures = book.snapshot_retry.get(r).map_or(0, |(n, _)| *n) + 1;
        book.snapshot_retry.insert(
            r.clone(),
            (failures, now + retry_after_ms(failures, first_ms)),
        );
        return;
    };
    svc.book().lock().snapshot_retry.remove(r);
    let clock_key = format!("{}{}", keys::CLOCK_PREFIX, r.0);
    let (before, prev_at, previous) = {
        let store = svc.store();
        (
            store.device_apps(r).unwrap_or_default(),
            store.device_snapshot_at(r).ok().flatten(),
            Stored::of(store.setting(&clock_key).ok().flatten().as_deref()),
        )
    };
    let (clock_shift_ms, reseed) = compare_clocks(&previous, clock.as_ref());
    let after = Snapshot {
        at_ms: svc.now(),
        apps,
        clock_shift_ms,
    };
    // L69: a new time zone re-prints every date (and a corrupt stored clock is no base): this
    // snapshot seeds again, like a first one.
    let first = prev_at.is_none() || reseed;
    let prev_at = prev_at.unwrap_or(after.at_ms);
    let changes = diff(&before, prev_at, &after, info.platform, first);

    // The windows any change could fall in, and those open when this snapshot started.
    let earliest = changes
        .iter()
        .map(|(_, _, when)| match *when {
            InstallTime::Device { at_ms } | InstallTime::FirstSeen { at_ms } => at_ms,
            InstallTime::ChangedBetween { from_ms, .. } => from_ms,
        })
        .min()
        .unwrap_or(started)
        .min(started);
    let (owed, sources) = account(svc, r, &changes, earliest, started, after.at_ms);
    let apps = with_ios_times(&before, &after, prev_at, info.platform, first);
    let events: Vec<InstallEvent> = changes
        .into_iter()
        .zip(sources)
        .map(|((app_id, change, when), source)| {
            let app = apps
                .iter()
                .chain(before.iter())
                .find(|a| a.app_id == app_id);
            InstallEvent {
                id: uuid::Uuid::new_v4().to_string(),
                device: r.clone(),
                alias: info.alias.clone(),
                version_name: app.and_then(|a| a.version_name.clone()),
                version_code: app.and_then(|a| a.version_code.clone()),
                app_id,
                change,
                source,
                when,
                recorded_at_ms: after.at_ms,
            }
        })
        .collect();
    let clock = clock.map(|c| c.to_string());
    let setting = clock.as_deref().map(|value| (clock_key.as_str(), value));
    record(svc, r, &apps, after.at_ms, setting, events);
    if owed {
        svc.snapshot_soon(r);
    }
}

/// A good snapshot's install events, its apps and the clock they were converted with, in one
/// transaction (L69, L71); the events are emitted only once it committed. On a failure nothing
/// is stored or emitted: the next good snapshot diffs against the old apps again.
fn record(
    svc: &DevicesService,
    r: &ResourceId,
    apps: &[AppState],
    at_ms: i64,
    setting: Option<(&str, &str)>,
    events: Vec<InstallEvent>,
) {
    let stored = svc
        .store()
        .put_device_snapshot(r, apps, at_ms, setting, &events);
    if let Err(e) = stored {
        tracing::warn!("cannot record an app snapshot and its installs: {e:#}");
        return;
    }
    for event in events {
        svc.emit(Event::DevicesInstalled(Box::new(event)));
    }
}

/// One good snapshot's bookkeeping and the source of each of its `changes`, against the windows
/// of `r` as the claims engine has them now: a window whose claim already ended in the engine's
/// history carries that end, though the windows pass may not have closed its row yet (R2-1).
/// Read after the snapshot finished, so a claim that ends during it counts as ended. `true`:
/// another snapshot is owed for a close.
pub(crate) fn account(
    svc: &DevicesService,
    r: &ResourceId,
    changes: &[(String, InstallChange, InstallTime)],
    earliest_ms: i64,
    started_ms: i64,
    at_ms: i64,
) -> (bool, Vec<InstallSource>) {
    let from_ms = earliest_ms - DEVICE_SLACK_MS;
    let (mut rows, live, ended) = {
        let store = svc.store();
        (
            store
                .claim_windows_on(r, from_ms, at_ms)
                .unwrap_or_default(),
            store.live_device_claims(r).unwrap_or_default(),
            store
                .ended_device_claims(r, from_ms, at_ms)
                .unwrap_or_default(),
        )
    };
    // A row is open only while its claim is live in the engine (R3-1); an ended one carries its
    // end from the history (R2-1); one that is neither has an unknown end and is set aside.
    let mut unknown = Vec::new();
    rows.retain_mut(|w| {
        if w.to_ms.is_some() {
            return true;
        }
        // An ended record wins over a live one (L68).
        let ended_now = ended
            .iter()
            .find(|e| e.claim_id == w.claim_id)
            .and_then(|e| e.to_ms);
        if ended_now.is_none() && live.iter().any(|l| l.claim_id == w.claim_id) {
            return true;
        }
        let end = ended_now.or_else(|| {
            let item = svc.store().history_item_of(&w.claim_id).ok().flatten();
            item.map(|h| h.ended_at_ms)
        });
        match end {
            Some(end) => {
                w.to_ms = Some(end);
                true
            }
            None => {
                unknown.push(ClaimWindow {
                    to_ms: Some(at_ms),
                    ..w.clone()
                });
                false
            }
        }
    });
    let (owed, mut windows) = {
        let mut book = svc.book().lock();
        let owed = book.snapshot_taken(r, &rows, started_ms, at_ms);
        (owed, book.with_snapshots(rows))
    };
    // The engine's device claims the windows pass has not recorded yet (a live next claim, a
    // short claim inside the gap), for this snapshot only: never written as rows.
    let mut known: HashSet<String> = windows
        .iter()
        .map(|(w, _)| w.claim_id.clone())
        .chain(unknown.iter().map(|w| w.claim_id.clone()))
        .collect();
    // One per claim id; a claim in both records counts as ended (L68).
    let blockers: Vec<ClaimWindow> = ended
        .into_iter()
        .chain(live)
        .filter(|c| known.insert(c.claim_id.clone()))
        .collect();
    let unrecorded = WindowSnapshots {
        unrecorded: true,
        ..WindowSnapshots::default()
    };
    windows.extend(
        unknown
            .into_iter()
            .chain(blockers)
            .map(|w| (w, unrecorded.clone())),
    );
    let sources = changes
        .iter()
        .map(|(_, _, when)| attribute(when, &windows))
        .collect();
    (owed, sources)
}

/// An Android snapshot's clock as stored with its apps (`devices.clock.<resource>`): what its
/// time conversion added (`android::DeviceClock::conversion_ms`) and the time zone id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Clock {
    conversion_ms: i64,
    zone: Option<String>,
}

impl Clock {
    /// Exactly `<integer>` or `<integer> <zone>` (a zone without whitespace); anything else is
    /// malformed (`None`).
    fn parse(value: &str) -> Option<Clock> {
        let (conversion, zone) = match value.split_once(' ') {
            Some((c, z)) => (c, Some(z)),
            None => (value, None),
        };
        let digits = conversion.strip_prefix('-').unwrap_or(conversion);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if zone.is_some_and(|z| z.is_empty() || z.contains(char::is_whitespace)) {
            return None;
        }
        Some(Clock {
            conversion_ms: conversion.parse().ok()?,
            zone: zone.map(str::to_owned),
        })
    }
}

impl std::fmt::Display for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.zone {
            Some(zone) => write!(f, "{} {zone}", self.conversion_ms),
            None => write!(f, "{}", self.conversion_ms),
        }
    }
}

/// What is stored for a device's clock: nothing (apps from before F-2: the 2 s tolerance), a
/// clock, or a value that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stored {
    Missing,
    Clock(Clock),
    Malformed,
}

impl Stored {
    fn of(value: Option<&str>) -> Stored {
        match value {
            None => Stored::Missing,
            Some(v) => Clock::parse(v).map_or(Stored::Malformed, Stored::Clock),
        }
    }
}

/// This snapshot's conversion against the stored one: the shift between them (F-2), and whether
/// to re-seed without events. A re-seed comes from a known zone that changed (L69; a stored
/// clock without a zone, from before the zone was read, just gains it) or from a stored value
/// that does not parse (never trusted for a diff).
fn compare_clocks(previous: &Stored, now: Option<&Clock>) -> (Option<i64>, bool) {
    match (previous, now) {
        (Stored::Malformed, _) => (None, true),
        (Stored::Clock(before), Some(now)) => match (&before.zone, &now.zone) {
            (Some(was), Some(is)) if was != is => (None, true),
            _ => (Some(now.conversion_ms - before.conversion_ms), false),
        },
        _ => (None, false),
    }
}

/// The apps of `info` now, or `None` (a failed, timed-out or truncated read; an Android one
/// without its clock lines, L69); for Android also its clock.
fn read_apps(
    svc: &DevicesService,
    info: &DeviceInfo,
    started_ms: i64,
) -> Option<(Vec<AppState>, Option<Clock>)> {
    let tools = svc.tools();
    match (info.platform, info.kind) {
        (Platform::Android, _) => {
            let adb = tools.adb?;
            let serial = &info.transports.first()?.address;
            let args = [
                OsStr::new("-s"),
                OsStr::new(serial),
                OsStr::new("shell"),
                OsStr::new(tools::SNAPSHOT),
            ];
            let out = svc
                .call(Tool::Adb, &adb, &args, ADB_SNAPSHOT_BOUND)
                .filter(|o| o.status == Some(0) && !o.timed_out)?;
            let text = String::from_utf8_lossy(&out.stdout);
            let clock = android::parse_clock(&text, started_ms)?;
            let apps = android::parse_apps(&text, out.truncated, Some(&clock))?;
            let clock = Clock {
                conversion_ms: clock.conversion_ms(),
                zone: android::parse_zone(&text),
            };
            Some((apps, Some(clock)))
        }
        (Platform::Ios, DeviceKind::Simulator) => {
            simulator_apps(svc, &tools, &info.hardware_id).map(|apps| (apps, None))
        }
        (Platform::Ios, _) => {
            let xcrun = tools.xcrun?;
            let path = svc.json_path("devicectl-apps");
            let _ = std::fs::remove_file(&path);
            let args = [
                OsStr::new("devicectl"),
                OsStr::new("device"),
                OsStr::new("info"),
                OsStr::new("apps"),
                OsStr::new("--device"),
                OsStr::new(&info.hardware_id),
                OsStr::new("--json-output"),
                path.as_os_str(),
            ];
            let out = svc.call(Tool::Xcrun, &xcrun, &args, DEVICECTL_APPS_BOUND);
            let bytes = std::fs::read(&path).ok();
            let _ = std::fs::remove_file(&path);
            out.filter(ok)?;
            ios::parse_devicectl_apps(&bytes?).map(|apps| (apps, None))
        }
    }
}

fn ok(out: &tools::ToolOutput) -> bool {
    out.status == Some(0) && !out.timed_out && !out.truncated
}

/// `simctl listapps` (an OpenStep plist, converted by `plutil` from stdin), then up to 50
/// version names from each app's `Info.plist`.
fn simulator_apps(
    svc: &DevicesService,
    tools: &tools::ToolPaths,
    udid: &str,
) -> Option<Vec<AppState>> {
    let (xcrun, plutil) = (tools.xcrun.clone()?, tools.plutil.clone()?);
    let args = [
        OsStr::new("simctl"),
        OsStr::new("listapps"),
        OsStr::new(udid),
    ];
    let listed = svc
        .call(Tool::Xcrun, &xcrun, &args, LISTAPPS_BOUND)
        .filter(ok)?;
    let args = ["-convert", "json", "-o", "-", "-"].map(OsStr::new);
    let json = svc
        .call_with_stdin(Tool::Plutil, &plutil, &args, &listed.stdout, PLUTIL_BOUND)
        .filter(ok)?;
    let mut apps = Vec::new();
    for (i, (mut app, bundle)) in ios::parse_simctl_listapps(&json.stdout)?
        .into_iter()
        .enumerate()
    {
        if let Some(bundle) = bundle.filter(|_| i < MAX_VERSION_READS) {
            let plist = bundle.join("Info.plist");
            let args = [
                OsStr::new("-extract"),
                OsStr::new("CFBundleShortVersionString"),
                OsStr::new("raw"),
                plist.as_os_str(),
            ];
            app.version_name = svc
                .call(Tool::Plutil, &plutil, &args, PLUTIL_BOUND)
                .filter(ok)
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|v| !v.is_empty());
        }
        apps.push(app);
    }
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    Some(apps)
}

/// iOS apps carry no times of their own: a seeded app is `FirstSeen`, a new or changed one
/// `ChangedBetween` the two snapshots, an unchanged one keeps its times. Android apps are
/// stored as read.
fn with_ios_times(
    before: &[AppState],
    after: &Snapshot,
    prev_at_ms: i64,
    platform: Platform,
    first: bool,
) -> Vec<AppState> {
    if platform != Platform::Ios {
        return after.apps.clone();
    }
    let between = InstallTime::ChangedBetween {
        from_ms: prev_at_ms,
        to_ms: after.at_ms,
    };
    after
        .apps
        .iter()
        .map(|app| {
            let mut app = app.clone();
            match before.iter().find(|b| b.app_id == app.app_id) {
                Some(b) => {
                    app.installed = b.installed.clone();
                    app.updated = if same_version(b, &app) {
                        b.updated.clone()
                    } else {
                        Some(between.clone())
                    };
                }
                None if first => {
                    app.installed = Some(InstallTime::FirstSeen { at_ms: after.at_ms });
                }
                None => app.installed = Some(between.clone()),
            }
            app
        })
        .collect()
}

fn same_version(a: &AppState, b: &AppState) -> bool {
    a.version_name == b.version_name && a.version_code == b.version_code
}

/// The changes from `before` (taken at `prev_at_ms`) to `after`. A first snapshot only seeds.
/// Callers pass good, whole snapshots only, so a failed read never "removes" an app.
pub fn diff(
    before: &[AppState],
    prev_at_ms: i64,
    after: &Snapshot,
    platform: Platform,
    first: bool,
) -> Vec<(String, InstallChange, InstallTime)> {
    if first {
        return Vec::new();
    }
    let between = InstallTime::ChangedBetween {
        from_ms: prev_at_ms,
        to_ms: after.at_ms,
    };
    let old: HashMap<&str, &AppState> = before.iter().map(|a| (a.app_id.as_str(), a)).collect();
    let mut out = Vec::new();
    for app in &after.apps {
        match (old.get(app.app_id.as_str()), platform) {
            (None, Platform::Android) => {
                let when = app
                    .installed
                    .clone()
                    .unwrap_or(InstallTime::FirstSeen { at_ms: after.at_ms });
                out.push((app.app_id.clone(), InstallChange::Installed, when));
            }
            (None, Platform::Ios) => {
                out.push((
                    app.app_id.clone(),
                    InstallChange::Installed,
                    between.clone(),
                ));
            }
            (Some(b), Platform::Android) => {
                let moved = match (&b.updated, &app.updated) {
                    (
                        Some(InstallTime::Device { at_ms: was }),
                        Some(InstallTime::Device { at_ms: now }),
                    ) => match after.clock_shift_ms {
                        // F-2: both are whole device seconds plus each snapshot's conversion
                        // (the skew has `date +%s`'s 1 s steps and the call's latency): the
                        // device's own value moved only when they differ by more than that.
                        Some(shift) => now - was != shift,
                        // No earlier conversion known (the first snapshot after an upgrade).
                        None => (now - was).abs() > UNKNOWN_SHIFT_TOLERANCE_MS,
                    },
                    (Some(was), Some(now)) => was != now,
                    _ => !same_version(b, app),
                };
                if moved {
                    let when = app.updated.clone().unwrap_or_else(|| between.clone());
                    out.push((app.app_id.clone(), InstallChange::Updated, when));
                }
            }
            (Some(b), Platform::Ios) => {
                if !same_version(b, app) {
                    out.push((app.app_id.clone(), InstallChange::Updated, between.clone()));
                }
            }
        }
    }
    let now: BTreeSet<&str> = after.apps.iter().map(|a| a.app_id.as_str()).collect();
    for app in before.iter().filter(|a| !now.contains(a.app_id.as_str())) {
        out.push((app.app_id.clone(), InstallChange::Removed, between.clone()));
    }
    out
}

/// Which claim a change came through (P2-4, P2-21).
pub fn attribute(when: &InstallTime, windows: &[(ClaimWindow, WindowSnapshots)]) -> InstallSource {
    match *when {
        InstallTime::FirstSeen { .. } => InstallSource::NotThroughHostler,
        InstallTime::Device { at_ms } => {
            let inside: Vec<&(ClaimWindow, WindowSnapshots)> = windows
                .iter()
                .filter(|(w, _)| {
                    w.from_ms - DEVICE_SLACK_MS <= at_ms
                        && w.to_ms.is_none_or(|to| at_ms <= to + DEVICE_SLACK_MS)
                })
                .collect();
            let claims: Vec<&ClaimWindow> = inside.iter().map(|(w, _)| w).collect();
            // An unrecorded claim inside it: never a `Claim` (R3-1).
            if inside.iter().any(|(_, s)| s.unrecorded) {
                return around(&claims);
            }
            claim_or_around(&claims)
        }
        InstallTime::ChangedBetween { from_ms, to_ms } => {
            let opened = |after: i64, until: i64, own: &str| {
                windows
                    .iter()
                    .any(|(o, _)| o.claim_id != own && o.from_ms > after && o.from_ms <= until)
            };
            let through: Vec<&ClaimWindow> = windows
                .iter()
                .filter(|(w, s)| {
                    let Some(first) = s.first_in_window_ms else {
                        return false;
                    };
                    if from_ms < first {
                        return false;
                    }
                    match w.to_ms {
                        // Closed: `to` is its close snapshot, started soon after its end, and
                        // no other claim began in between (L52 lets the next one straight in).
                        Some(end) => {
                            s.close_at_ms == Some(to_ms)
                                && s.close_started_ms.is_some_and(|started| {
                                    started >= end
                                        && started - end <= CLOSE_WITHIN_MS
                                        && !opened(end, started, &w.claim_id)
                                })
                        }
                        // Still open: both snapshots inside it, and no other claim began
                        // between them.
                        None => !opened(from_ms, to_ms, &w.claim_id),
                    }
                })
                .map(|(w, _)| w)
                .collect();
            if !through.is_empty() {
                return claim_or_around(&through);
            }
            let overlapping: Vec<&ClaimWindow> = windows
                .iter()
                .map(|(w, _)| w)
                .filter(|w| w.from_ms <= to_ms && w.to_ms.is_none_or(|to| to >= from_ms))
                .collect();
            around(&overlapping)
        }
    }
}

/// Around `windows` (the first one's workspace); none: not through hostler.
fn around(windows: &[&ClaimWindow]) -> InstallSource {
    match windows.first() {
        None => InstallSource::NotThroughHostler,
        Some(w) => InstallSource::Around {
            claim_ids: windows.iter().map(|w| w.claim_id.clone()).collect(),
            workspace_path: w.workspace_path.clone(),
        },
    }
}

/// Windows of one workspace: that claim (the latest one's branch, commit and command, every
/// claim id); of two or more: around them; none: not through hostler.
fn claim_or_around(windows: &[&ClaimWindow]) -> InstallSource {
    let Some(latest) = windows.iter().max_by_key(|w| w.from_ms) else {
        return InstallSource::NotThroughHostler;
    };
    let claim_ids = windows.iter().map(|w| w.claim_id.clone()).collect();
    let workspaces: BTreeSet<&str> = windows.iter().map(|w| w.workspace_path.as_str()).collect();
    if workspaces.len() > 1 {
        return around(windows);
    }
    InstallSource::Claim {
        claim_ids,
        workspace_path: latest.workspace_path.clone(),
        branch: latest.branch.clone(),
        commit: latest.head_commit.clone(),
        command: latest.command.clone(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A registered, connected, ready device.
    pub(crate) fn device(platform: Platform, kind: DeviceKind) -> DeviceInfo {
        DeviceInfo {
            resource: ResourceId("device:test:X".into()),
            hardware_id: "X".into(),
            platform,
            kind,
            registered: true,
            alias: Some("x".into()),
            alias_by_person: false,
            model: "m".into(),
            name: None,
            os_version: None,
            note: None,
            connected: true,
            ready: true,
            transports: Vec::new(),
            coredevice_id: None,
            telemetry: None,
            booted_by_hostler: false,
            shutting_down: false,
            removing: false,
            sessions: Vec::new(),
            checking_sessions: false,
            first_seen_ms: 0,
            last_seen_ms: 0,
        }
    }

    fn app(id: &str, code: &str, installed: Option<i64>, updated: Option<i64>) -> AppState {
        AppState {
            app_id: id.into(),
            version_name: None,
            version_code: Some(code.into()),
            installed: installed.map(|at_ms| InstallTime::Device { at_ms }),
            updated: updated.map(|at_ms| InstallTime::Device { at_ms }),
        }
    }

    fn ios_app(id: &str, version: &str) -> AppState {
        AppState {
            app_id: id.into(),
            version_name: Some(version.into()),
            version_code: Some("1".into()),
            installed: None,
            updated: None,
        }
    }

    fn snap(at_ms: i64, apps: Vec<AppState>) -> Snapshot {
        Snapshot {
            at_ms,
            apps,
            clock_shift_ms: None,
        }
    }

    fn window(id: &str, ws: &str, from: i64, to: Option<i64>) -> ClaimWindow {
        ClaimWindow {
            claim_id: id.into(),
            resource: ResourceId("device:android:X".into()),
            workspace_id: ws.into(),
            workspace_path: format!("/work/{ws}"),
            branch: Some(format!("{id}-branch")),
            head_commit: Some(format!("{id}-commit")),
            command: Some(format!("{id} cmd")),
            from_ms: from,
            to_ms: to,
        }
    }

    fn claim_of(source: &InstallSource) -> Option<(Vec<String>, Option<String>)> {
        match source {
            InstallSource::Claim {
                claim_ids, branch, ..
            } => Some((claim_ids.clone(), branch.clone())),
            _ => None,
        }
    }

    fn snaps(first: Option<i64>, started: Option<i64>, at: Option<i64>) -> WindowSnapshots {
        WindowSnapshots {
            first_in_window_ms: first,
            close_started_ms: started,
            close_at_ms: at,
            unrecorded: false,
        }
    }

    #[test]
    fn snapshot_due_registered_only() {
        // The schedule: first at once, then 30 min; an iOS claim makes it 10 min, an Android
        // one does not.
        assert!(snapshot_due(Platform::Android, false, None, 0));
        assert!(!snapshot_due(
            Platform::Android,
            false,
            Some(0),
            IDLE_EVERY_MS - 1
        ));
        assert!(snapshot_due(
            Platform::Android,
            false,
            Some(0),
            IDLE_EVERY_MS
        ));
        assert!(!snapshot_due(
            Platform::Android,
            true,
            Some(0),
            IOS_CLAIMED_EVERY_MS
        ));
        assert!(snapshot_due(
            Platform::Ios,
            true,
            Some(0),
            IOS_CLAIMED_EVERY_MS
        ));
        assert!(!snapshot_due(
            Platform::Ios,
            false,
            Some(0),
            IOS_CLAIMED_EVERY_MS
        ));
        // Registered, connected and booted only.
        let d = device(Platform::Android, DeviceKind::Physical);
        assert!(snapshottable(&d));
        for change in [
            |d: &mut DeviceInfo| d.registered = false,
            |d: &mut DeviceInfo| d.connected = false,
            |d: &mut DeviceInfo| d.ready = false,
            |d: &mut DeviceInfo| d.shutting_down = true,
            |d: &mut DeviceInfo| d.removing = true,
        ] {
            let mut other = d.clone();
            change(&mut other);
            assert!(!snapshottable(&other), "{other:?}");
        }
    }

    #[test]
    fn diff_android_update_install_remove() {
        let before = vec![
            app("com.example.kept", "1", Some(100_000), Some(100_000)),
            app("com.example.updated", "1", Some(100_000), Some(100_000)),
            app("com.example.gone", "1", Some(100_000), Some(100_000)),
        ];
        let after = snap(
            900_000,
            vec![
                app("com.example.kept", "1", Some(100_000), Some(100_000)),
                // A same-version reinstall moves only lastUpdateTime.
                app("com.example.updated", "1", Some(100_000), Some(500_000)),
                app("com.example.new", "3", Some(600_000), Some(600_000)),
            ],
        );
        let changes = diff(&before, 200_000, &after, Platform::Android, false);
        assert_eq!(
            changes,
            vec![
                (
                    "com.example.updated".into(),
                    InstallChange::Updated,
                    InstallTime::Device { at_ms: 500_000 }
                ),
                (
                    "com.example.new".into(),
                    InstallChange::Installed,
                    InstallTime::Device { at_ms: 600_000 }
                ),
                (
                    "com.example.gone".into(),
                    InstallChange::Removed,
                    InstallTime::ChangedBetween {
                        from_ms: 200_000,
                        to_ms: 900_000
                    }
                ),
            ]
        );
    }

    /// Task 13 F-2: one app list read twice, the clock line's skew 0.7 s apart (and an hour of
    /// UTC offset, a DST change): no `Updated`; a real `lastUpdateTime` move still is one, at its
    /// converted time.
    #[test]
    fn android_clock_jitter_is_no_update() {
        let text = |last: &str| {
            format!(
                "1800000000\n+0100\npackage:com.example.a versionCode:7\n\
                 package:com.example.b versionCode:3\n===\n\
                 Package [com.example.a] (1):\n  versionCode=7\n  \
                 firstInstallTime=2026-01-01 10:00:00\n  lastUpdateTime={last}\n\
                 Package [com.example.b] (2):\n  versionCode=3\n  \
                 firstInstallTime=2026-01-01 10:00:00\n  lastUpdateTime=2026-01-02 10:00:00\n"
            )
        };
        let read = |text: &str, host_ms: i64| {
            let clock = android::parse_clock(text, host_ms).unwrap();
            (
                android::parse_apps(text, false, Some(&clock)).unwrap(),
                clock.conversion_ms(),
            )
        };
        let same = text("2026-01-03 10:00:00");
        let (before, k1) = read(&same, 1_800_000_000_000);
        let (apps, k2) = read(&same, 1_800_000_000_700);
        let after = Snapshot {
            at_ms: 9_000,
            apps,
            clock_shift_ms: Some(k2 - k1),
        };
        assert!(diff(&before, 5_000, &after, Platform::Android, false).is_empty());
        // Without the earlier conversion: within the 2 s tolerance.
        let unknown = Snapshot {
            clock_shift_ms: None,
            ..after.clone()
        };
        assert!(diff(&before, 5_000, &unknown, Platform::Android, false).is_empty());
        // A DST change shifts every converted time by an hour, the device's values not.
        let dst = same.replace("+0100", "+0200");
        let (apps, k3) = read(&dst, 1_800_000_000_400);
        let after_dst = Snapshot {
            at_ms: 9_000,
            apps,
            clock_shift_ms: Some(k3 - k1),
        };
        assert!(diff(&before, 5_000, &after_dst, Platform::Android, false).is_empty());

        // A real move by one second is an update, at the new converted time.
        let moved = text("2026-01-03 10:00:01");
        let (apps, k4) = read(&moved, 1_800_000_000_900);
        let want = apps[0].updated.clone().unwrap();
        let after = Snapshot {
            at_ms: 9_000,
            apps,
            clock_shift_ms: Some(k4 - k1),
        };
        assert_eq!(
            diff(&before, 5_000, &after, Platform::Android, false),
            vec![("com.example.a".into(), InstallChange::Updated, want)]
        );
    }

    /// L69: the phone's time zone changes, so every printed date moves by an hour: this
    /// snapshot re-seeds without events; the next one, in the new zone, diffs normally.
    #[test]
    fn a_time_zone_change_reseeds_without_events() {
        let text = |zone: &str, offset: &str, hour: u32, extra_s: u32| {
            format!(
                "1800000000\n{offset}\n{zone}\npackage:com.example.a versionCode:7\n===\n\
                 Package [com.example.a] (1):\n  versionCode=7\n  \
                 firstInstallTime=2026-01-01 {hour:02}:00:00\n  \
                 lastUpdateTime=2026-01-02 {hour:02}:00:{extra_s:02}\n"
            )
        };
        let read = |text: &str| {
            let clock = android::parse_clock(text, 1_800_000_000_300).unwrap();
            let apps = android::parse_apps(text, false, Some(&clock)).unwrap();
            let clock = Clock {
                conversion_ms: clock.conversion_ms(),
                zone: android::parse_zone(text),
            };
            (apps, clock)
        };
        let snapshot = |apps, shift| Snapshot {
            at_ms: 9_000,
            apps,
            clock_shift_ms: shift,
        };
        let (before, berlin) = read(&text("Europe/Berlin", "+0100", 10, 0));
        assert_eq!(berlin.zone.as_deref(), Some("Europe/Berlin"));
        let (moved, athens) = read(&text("Europe/Athens", "+0200", 11, 0));
        let (shift, reseed) = compare_clocks(&Stored::Clock(berlin.clone()), Some(&athens));
        assert!(reseed && shift.is_none());
        assert!(
            diff(
                &before,
                5_000,
                &snapshot(moved.clone(), shift),
                Platform::Android,
                reseed
            )
            .is_empty()
        );
        // Stored and read back, the next snapshot in that zone diffs normally.
        let stored = Clock::parse(&athens.to_string()).unwrap();
        assert_eq!(stored, athens);
        let (same, again) = read(&text("Europe/Athens", "+0200", 11, 0));
        let (shift, reseed) = compare_clocks(&Stored::Clock(stored.clone()), Some(&again));
        assert!(!reseed);
        assert!(
            diff(
                &moved,
                5_000,
                &snapshot(same, shift),
                Platform::Android,
                false
            )
            .is_empty()
        );
        let (updated, later) = read(&text("Europe/Athens", "+0200", 11, 30));
        let (shift, _) = compare_clocks(&Stored::Clock(stored.clone()), Some(&later));
        let changes = diff(
            &moved,
            5_000,
            &snapshot(updated, shift),
            Platform::Android,
            false,
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].1, InstallChange::Updated);
        // A clock stored before the zone was read: its conversion, no zone.
        assert_eq!(
            Clock::parse("-3600000"),
            Some(Clock {
                conversion_ms: -3_600_000,
                zone: None
            })
        );
    }

    /// F-R2: a re-seed needs both zones known and different; a stored clock without a zone just
    /// gains one. A malformed stored value re-seeds; a missing one keeps the 2 s tolerance.
    #[test]
    fn stored_clocks_legacy_malformed_and_missing() {
        let now = Clock {
            conversion_ms: 1_700,
            zone: Some("Europe/Berlin".into()),
        };
        let legacy = Stored::of(Some("1000"));
        assert_eq!(compare_clocks(&legacy, Some(&now)), (Some(700), false));
        let unknown_now = Clock {
            zone: None,
            ..now.clone()
        };
        let berlin = Stored::of(Some("1000 Europe/Berlin"));
        assert_eq!(
            compare_clocks(&berlin, Some(&unknown_now)),
            (Some(700), false)
        );
        assert_eq!(compare_clocks(&berlin, Some(&now)), (Some(700), false));
        let athens = Stored::of(Some("1000 Europe/Athens"));
        assert_eq!(compare_clocks(&athens, Some(&now)), (None, true));
        assert_eq!(
            Stored::of(Some("1000 GMT+05:30")),
            Stored::Clock(Clock {
                conversion_ms: 1_000,
                zone: Some("GMT+05:30".into())
            })
        );
        for bad in [
            "",
            " ",
            "abc",
            "1000 ",
            " 1000",
            "1000  UTC",
            "1000 Europe/Berlin extra",
            "+1000",
            "10.5",
            "-",
            "1000\tUTC",
            "99999999999999999999",
        ] {
            assert_eq!(Stored::of(Some(bad)), Stored::Malformed, "{bad:?}");
            assert_eq!(
                compare_clocks(&Stored::of(Some(bad)), Some(&now)),
                (None, true),
                "{bad:?}"
            );
        }
        assert_eq!(compare_clocks(&Stored::Missing, Some(&now)), (None, false));
    }

    #[test]
    fn diff_ios_changed_between() {
        let before = vec![
            ios_app("com.example.a", "1.0"),
            ios_app("com.example.b", "1.0"),
        ];
        let after = snap(
            9_000,
            vec![
                ios_app("com.example.a", "1.1"),
                ios_app("com.example.c", "2.0"),
            ],
        );
        let between = InstallTime::ChangedBetween {
            from_ms: 4_000,
            to_ms: 9_000,
        };
        assert_eq!(
            diff(&before, 4_000, &after, Platform::Ios, false),
            vec![
                (
                    "com.example.a".into(),
                    InstallChange::Updated,
                    between.clone()
                ),
                (
                    "com.example.c".into(),
                    InstallChange::Installed,
                    between.clone()
                ),
                (
                    "com.example.b".into(),
                    InstallChange::Removed,
                    between.clone()
                ),
            ]
        );
        // Stored times: unchanged apps keep theirs, new and updated ones get the interval.
        let stored = with_ios_times(&before, &after, 4_000, Platform::Ios, false);
        assert_eq!(stored[0].updated, Some(between.clone()));
        assert_eq!(stored[1].installed, Some(between));
    }

    #[test]
    fn first_snapshot_seeds_without_events() {
        let after = snap(
            5_000,
            vec![app("com.example.a", "1", Some(1_000), Some(1_000))],
        );
        assert!(diff(&[], 5_000, &after, Platform::Android, true).is_empty());
        let ios = snap(5_000, vec![ios_app("com.example.a", "1.0")]);
        assert!(diff(&[], 5_000, &ios, Platform::Ios, true).is_empty());
        let stored = with_ios_times(&[], &ios, 5_000, Platform::Ios, true);
        assert_eq!(
            stored[0].installed,
            Some(InstallTime::FirstSeen { at_ms: 5_000 })
        );
    }

    /// A failed or truncated read never reaches `diff`: the Android parser answers `None` for
    /// it (so no app is "removed"), and an empty but whole list is a real "no apps".
    #[test]
    fn failed_or_truncated_snapshot_never_removes() {
        let clock = android::DeviceClock {
            utc_offset_s: 0,
            skew_ms: 0,
        };
        let whole = "1\n+0000\npackage:com.example.a versionCode:1\n===\n\
                     Package [com.example.a] (1):\n  versionCode=1\n";
        assert!(android::parse_apps(whole, false, Some(&clock)).is_some());
        assert_eq!(
            android::parse_apps(whole, true, Some(&clock)),
            None,
            "truncated"
        );
        assert_eq!(
            android::parse_apps("1\n+0000\n", false, Some(&clock)),
            None,
            "pm failed: no marker"
        );
        assert_eq!(ios::parse_devicectl_apps(b"{}"), None);
        assert_eq!(ios::parse_simctl_listapps(b"{}"), None, "no System apps");
    }

    #[test]
    fn attribute_inside_around_outside() {
        let windows = vec![
            (
                window("c1", "a", 10_000, Some(20_000)),
                WindowSnapshots::default(),
            ),
            (
                window("c2", "b", 30_000, Some(40_000)),
                WindowSnapshots::default(),
            ),
            (window("c3", "a", 35_000, None), WindowSnapshots::default()),
        ];
        let inside = attribute(&InstallTime::Device { at_ms: 15_000 }, &windows);
        assert_eq!(
            claim_of(&inside),
            Some((vec!["c1".into()], Some("c1-branch".into())))
        );
        // Two workspaces' windows at once.
        assert!(matches!(
            attribute(&InstallTime::Device { at_ms: 36_000 }, &windows),
            InstallSource::Around { ref claim_ids, .. } if claim_ids.len() == 2
        ));
        assert_eq!(
            attribute(&InstallTime::Device { at_ms: 25_000 }, &windows),
            InstallSource::NotThroughHostler
        );
        assert_eq!(
            attribute(&InstallTime::FirstSeen { at_ms: 15_000 }, &windows),
            InstallSource::NotThroughHostler
        );
        // An iOS interval overlapping a window without its snapshots: around it.
        assert!(matches!(
            attribute(
                &InstallTime::ChangedBetween {
                    from_ms: 5_000,
                    to_ms: 12_000
                },
                &windows
            ),
            InstallSource::Around { ref claim_ids, .. } if claim_ids == &["c1".to_string()]
        ));
        assert_eq!(
            attribute(
                &InstallTime::ChangedBetween {
                    from_ms: 21_000,
                    to_ms: 29_000
                },
                &windows
            ),
            InstallSource::NotThroughHostler
        );
    }

    /// F-4: a device added while it cannot be read (here a shut-down simulator) is not marked
    /// tried by the seed that found it so; once it can be read it is due at once, and a seed
    /// whose read fails is retried with the owed backoff, not after the 30-min schedule.
    #[test]
    fn an_unreadable_device_is_not_marked_tried() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicI64, Ordering};

        struct Moving(AtomicI64);
        impl crate::heavy::engine::Clock for Moving {
            fn now_ms(&self) -> i64 {
                self.0.load(Ordering::SeqCst)
            }
        }
        let clock = Arc::new(Moving(AtomicI64::new(1_000_000)));
        let (tx, _) = tokio::sync::broadcast::channel(16);
        // No xcrun: a read that runs fails at once.
        let (svc, _) = DevicesService::with_clock(
            crate::store::Store::open_in_memory().unwrap(),
            tools::ToolPaths::default(),
            tx,
            clock.clone(),
        );
        let udid = "00000000-0000-4000-8000-0000000000B2";
        let sim = |booted| ios::Simulator {
            udid: udid.into(),
            name: "iPhone Air".into(),
            runtime: "iOS 26.5".into(),
            booted,
            last_booted_at: None,
        };
        svc.test_ios(Vec::new(), vec![sim(false)]);
        svc.add(udid, None, false).unwrap();
        let r = ResourceId(format!("device:ios:{udid}"));
        let now = clock.0.load(Ordering::SeqCst);
        // The seed `add` owes finds it shut down: no read, no attempt recorded.
        snapshot_and_diff(&svc, &r);
        assert!(!svc.book().lock().snapshot_tried.contains_key(&r));
        assert!(!due(&svc, now).contains(&r), "not readable yet");
        // Booted: due at once, though the seed was asked for before.
        svc.test_ios(Vec::new(), vec![sim(true)]);
        assert!(due(&svc, now).contains(&r));
        // A seed read that fails: tried, then due again after the owed backoff.
        snapshot_and_diff(&svc, &r);
        assert_eq!(svc.book().lock().snapshot_tried.get(&r), Some(&now));
        let first = super::super::service::every(RETRY_FIRST).as_millis() as i64;
        assert!(!due(&svc, now + first - 1).contains(&r));
        assert!(due(&svc, now + first).contains(&r), "not 30 min later");
    }

    /// L71: a snapshot's installs, apps and clock are stored together, and its events emitted
    /// only after the commit. An insert that fails partway (a duplicate id here) stores and
    /// emits nothing, so the next good pass diffs against the old apps and records them once.
    #[test]
    fn a_failed_install_write_stores_and_emits_nothing() {
        use std::sync::Arc;

        struct At(i64);
        impl crate::heavy::engine::Clock for At {
            fn now_ms(&self) -> i64 {
                self.0
            }
        }
        let (tx, mut events) = tokio::sync::broadcast::channel(16);
        let (svc, _) = DevicesService::with_clock(
            crate::store::Store::open_in_memory().unwrap(),
            tools::ToolPaths::default(),
            tx,
            Arc::new(At(0)),
        );
        let r = ResourceId("device:android:X".into());
        let key = "devices.clock.device:android:X";
        let old = [app("com.example.old", "1", Some(1_000), Some(1_000))];
        record(&svc, &r, &old, 10_000, Some((key, "0 UTC")), Vec::new());
        let event = |id: &str, app_id: &str| InstallEvent {
            id: id.into(),
            device: r.clone(),
            alias: None,
            app_id: app_id.into(),
            change: InstallChange::Installed,
            version_name: None,
            version_code: Some("1".into()),
            source: InstallSource::NotThroughHostler,
            when: InstallTime::Device { at_ms: 15_000 },
            recorded_at_ms: 20_000,
        };
        let new = [
            old[0].clone(),
            app("com.example.a", "1", Some(15_000), Some(15_000)),
            app("com.example.b", "1", Some(15_000), Some(15_000)),
        ];
        let stored = |svc: &DevicesService| {
            let store = svc.store();
            let q = crate::contract::heavy::HistoryQuery {
                since_ms: None,
                limit: 10,
            };
            (
                store.device_apps(&r).unwrap().len(),
                store.device_snapshot_at(&r).unwrap(),
                store.setting(key).unwrap(),
                store.device_installs(Some(&r), &q).unwrap().len(),
            )
        };
        // The second insert fails: the first one, the apps and the clock are rolled back.
        record(
            &svc,
            &r,
            &new,
            20_000,
            Some((key, "5 UTC")),
            vec![event("e1", "com.example.a"), event("e1", "com.example.b")],
        );
        assert_eq!(
            stored(&svc),
            (1, Some(10_000), Some("0 UTC".into()), 0),
            "nothing stored"
        );
        assert!(events.try_recv().is_err(), "nothing emitted");
        // The next good pass: both, once.
        record(
            &svc,
            &r,
            &new,
            30_000,
            Some((key, "5 UTC")),
            vec![event("e2", "com.example.a"), event("e3", "com.example.b")],
        );
        assert_eq!(stored(&svc), (3, Some(30_000), Some("5 UTC".into()), 2));
        let mut emitted = Vec::new();
        while let Ok(Event::DevicesInstalled(e)) = events.try_recv() {
            emitted.push(e.id);
        }
        assert_eq!(emitted, ["e2", "e3"]);
    }

    /// R3-1, R3-2: an owed snapshot whose read fails waits 10 s, doubling per failure in a row
    /// up to 5 min; a pending close is due (from its first try on) only while a snapshot that
    /// starts then could still be its close snapshot. A close resets the backoff.
    #[test]
    fn owed_snapshots_back_off_after_failed_reads() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicI64, Ordering};

        let gaps: Vec<i64> = (1..=7).map(|n| retry_after_ms(n, 10_000)).collect();
        assert_eq!(
            gaps,
            [10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000]
        );
        assert_eq!(retry_after_ms(u32::MAX, 10_000), 300_000);

        struct Moving(AtomicI64);
        impl crate::heavy::engine::Clock for Moving {
            fn now_ms(&self) -> i64 {
                self.0.load(Ordering::SeqCst)
            }
        }
        let clock = Arc::new(Moving(AtomicI64::new(1_000_000)));
        let store = crate::store::Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();
        store
            .upsert_workspace("w", &project.id, "/work/app", "/work/app/.git", 1)
            .unwrap();
        let (tx, _) = tokio::sync::broadcast::channel(16);
        // No xcrun: every simulator read fails at once.
        let (svc, _) =
            DevicesService::with_clock(store, tools::ToolPaths::default(), tx, clock.clone());
        let udid = "00000000-0000-4000-8000-0000000000B1";
        svc.test_ios(
            Vec::new(),
            vec![ios::Simulator {
                udid: udid.into(),
                name: "iPhone Air".into(),
                runtime: "iOS 26.5".into(),
                booted: true,
                last_booted_at: None,
            }],
        );
        svc.add(udid, None, false).unwrap();
        let r = ResourceId(format!("device:ios:{udid}"));
        // Seeded before (F-4: an unseeded device owes its seed, retried like these).
        svc.store()
            .put_device_snapshot(&r, &[], 1, None, &[])
            .unwrap();
        let first = super::super::service::every(RETRY_FIRST).as_millis() as i64;
        let now = || clock.0.load(Ordering::SeqCst);
        let at = |ms: i64| clock.0.store(ms, Ordering::SeqCst);
        let is_due = |ms: i64| due(&svc, ms).contains(&r);

        // R3-1: an open iOS window owes its claim-open snapshot; each failure doubles the wait.
        svc.store()
            .insert_claim_window(&ClaimWindow {
                resource: r.clone(),
                workspace_id: "w".into(),
                ..window("c1", "app", now(), None)
            })
            .unwrap();
        assert!(is_due(now()), "the first try at once");
        for n in 1..=7 {
            snapshot_and_diff(&svc, &r);
            let wait = retry_after_ms(n, first);
            assert!(!is_due(now()), "not every pass (failure {n})");
            assert!(!is_due(now() + wait - 1), "failure {n}");
            assert!(is_due(now() + wait), "failure {n}");
            at(now() + wait);
        }
        assert_eq!(svc.book().lock().snapshot_retry[&r].0, 7);

        // R3-2: the claim ends; its close is tried at once, then retried within 30 s of the end,
        // then left to the schedule.
        super::super::windows::on_claims(
            &svc,
            &crate::contract::claims::ClaimsSnapshot {
                resources: Vec::new(),
            },
            now(),
        );
        let end = now();
        assert_eq!(svc.book().lock().pending_close_ms(&r), Some(end));
        assert!(is_due(end), "a close resets the backoff");
        let mut tries = 0;
        while is_due(now()) {
            snapshot_and_diff(&svc, &r);
            tries += 1;
            at(now() + retry_after_ms(tries, first));
        }
        assert!(
            now() - end > CLOSE_WITHIN_MS,
            "until the close cannot count"
        );
        assert!(
            (2..=5).contains(&tries),
            "a few tries for the close: {tries}"
        );
        assert!(!is_due(end + IDLE_EVERY_MS - 1));
        assert_eq!(
            svc.book().lock().pending_close_ms(&r),
            Some(end),
            "still waiting: the next good snapshot books it"
        );
    }

    #[test]
    fn close_snapshot_attributes_a_short_ios_claim() {
        // A 4 s hold: its open snapshot at 11 s, its close snapshot started at 15 s.
        let windows = vec![(
            window("c1", "a", 10_000, Some(14_000)),
            snaps(Some(11_000), Some(15_000), Some(16_000)),
        )];
        let when = InstallTime::ChangedBetween {
            from_ms: 11_000,
            to_ms: 16_000,
        };
        assert_eq!(
            claim_of(&attribute(&when, &windows)),
            Some((vec!["c1".into()], Some("c1-branch".into())))
        );
        // Still open: two snapshots inside it.
        let open = vec![(
            window("c1", "a", 10_000, None),
            snaps(Some(11_000), None, None),
        )];
        assert!(claim_of(&attribute(&when, &open)).is_some());
    }

    #[test]
    fn close_snapshot_too_late_or_after_another_window_is_around() {
        let when = InstallTime::ChangedBetween {
            from_ms: 11_000,
            to_ms: 60_000,
        };
        // Started 31 s after the end.
        let late = vec![(
            window("c1", "a", 10_000, Some(14_000)),
            snaps(Some(11_000), Some(45_000), Some(60_000)),
        )];
        assert!(matches!(
            attribute(&when, &late),
            InstallSource::Around { .. }
        ));
        // In time, but the same workspace's next claim opened before it started.
        let next = vec![
            (
                window("c1", "a", 10_000, Some(14_000)),
                snaps(Some(11_000), Some(20_000), Some(60_000)),
            ),
            (window("c2", "a", 15_000, None), WindowSnapshots::default()),
        ];
        assert!(matches!(
            attribute(&when, &next),
            InstallSource::Around { .. }
        ));
        // The interval began before the first in-window snapshot.
        let early = vec![(
            window("c1", "a", 10_000, Some(14_000)),
            snaps(Some(12_000), Some(15_000), Some(60_000)),
        )];
        assert!(matches!(
            attribute(&when, &early),
            InstallSource::Around { .. }
        ));
    }

    /// R3-1: an Android time inside a claim the windows pass has not recorded is never `Claim`,
    /// even of the same workspace.
    #[test]
    fn an_unrecorded_claim_makes_a_device_time_around() {
        let unrecorded = WindowSnapshots {
            unrecorded: true,
            ..WindowSnapshots::default()
        };
        let windows = vec![
            (
                window("c1", "a", 10_000, Some(20_000)),
                WindowSnapshots::default(),
            ),
            (window("c2", "a", 19_000, None), unrecorded.clone()),
        ];
        assert!(matches!(
            attribute(&InstallTime::Device { at_ms: 19_500 }, &windows),
            InstallSource::Around { ref claim_ids, .. } if claim_ids.len() == 2
        ));
        let alone = vec![(window("c2", "a", 19_000, None), unrecorded)];
        assert!(matches!(
            attribute(&InstallTime::Device { at_ms: 25_000 }, &alone),
            InstallSource::Around { .. }
        ));
    }

    #[test]
    fn android_times_use_one_second_slack_and_one_workspace() {
        let windows = vec![
            (
                window("c1", "a", 10_000, Some(20_000)),
                WindowSnapshots::default(),
            ),
            (
                window("c2", "a", 15_000, Some(25_000)),
                WindowSnapshots::default(),
            ),
        ];
        // 1 s either side of a window still counts.
        for at_ms in [9_000, 26_000] {
            assert!(
                claim_of(&attribute(&InstallTime::Device { at_ms }, &windows)).is_some(),
                "{at_ms}"
            );
        }
        for at_ms in [8_999, 26_001] {
            assert_eq!(
                attribute(&InstallTime::Device { at_ms }, &windows),
                InstallSource::NotThroughHostler,
                "{at_ms}"
            );
        }
        // Both claims of one workspace: one attribution naming both, the latest's details.
        assert_eq!(
            claim_of(&attribute(&InstallTime::Device { at_ms: 18_000 }, &windows)),
            Some((vec!["c1".into(), "c2".into()], Some("c2-branch".into())))
        );
    }

    /// Device-local `firstInstallTime`/`lastUpdateTime` become UTC with the device's `date +%z`
    /// and its skew against the host.
    #[test]
    fn times_converted_with_device_offset() {
        // The device's clock reads 2026-01-01 12:00:00 local at +0200, 3 s behind the host.
        let device_epoch_s = 1_767_261_600; // 2026-01-01T10:00:00Z
        let host_now = device_epoch_s * 1000 + 3_000;
        let out = format!(
            "{device_epoch_s}\n+0200\npackage:com.example.a versionCode:7\n===\n\
             Package [com.example.a] (1):\n  versionCode=7\n  \
             firstInstallTime=2026-01-01 11:00:00\n  lastUpdateTime=2026-01-01 11:30:00\n"
        );
        let clock = android::parse_clock(&out, host_now).unwrap();
        let apps = android::parse_apps(&out, false, Some(&clock)).unwrap();
        // 11:00 local at +0200 is 09:00Z, plus the 3 s skew.
        let nine = (device_epoch_s - 3_600) * 1000 + 3_000;
        assert_eq!(apps[0].installed, Some(InstallTime::Device { at_ms: nine }));
        assert_eq!(
            apps[0].updated,
            Some(InstallTime::Device {
                at_ms: nine + 1_800_000
            })
        );
    }
}
