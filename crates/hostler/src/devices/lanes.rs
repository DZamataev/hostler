//! The tool lanes (P-3, P3b Task 5). Each device tool call runs in one lane, on the blocking pool;
//! a lane never waits on another, so slow app snapshots never delay discovery or the
//! agent-device status. At most one `Probe` or `Long` call runs per device at a time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::contract::claims::ResourceId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    /// The adb `host:version` probe, `adb devices -l`, `emulator -list-avds`.
    DiscoveryAndroid,
    /// `devicectl list`, `simctl list`.
    DiscoveryIos,
    /// The claims-dir check, `agent-device device status`.
    Sessions,
    /// Identity, telemetry, `boot_completed`, the lockdown stat with `ideviceinfo`, and
    /// agent-device's hourly `--version`.
    Probe,
    /// App snapshots, boots and shutdowns.
    Long,
}

impl Lane {
    pub const ALL: [Lane; 5] = [
        Lane::DiscoveryAndroid,
        Lane::DiscoveryIos,
        Lane::Sessions,
        Lane::Probe,
        Lane::Long,
    ];

    /// The Global Constraints' slots: one reserved each for discovery and sessions, two for the
    /// per-device lanes.
    pub fn slots(self) -> usize {
        match self {
            Lane::DiscoveryAndroid | Lane::DiscoveryIos | Lane::Sessions => 1,
            Lane::Probe | Lane::Long => 2,
        }
    }

    fn per_device(self) -> bool {
        matches!(self, Lane::Probe | Lane::Long)
    }
}

/// Per lane a semaphore; per device one `Probe`/`Long` call at a time.
pub struct ToolGate {
    lanes: HashMap<Lane, Arc<Semaphore>>,
    devices: Mutex<HashMap<ResourceId, Arc<Semaphore>>>,
}

impl Default for ToolGate {
    fn default() -> Self {
        ToolGate::new()
    }
}

impl ToolGate {
    pub fn new() -> ToolGate {
        ToolGate {
            lanes: Lane::ALL
                .into_iter()
                .map(|lane| (lane, Arc::new(Semaphore::new(lane.slots()))))
                .collect(),
            devices: Mutex::new(HashMap::new()),
        }
    }

    fn device_slot(&self, r: &ResourceId) -> Arc<Semaphore> {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        devices
            .entry(r.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    /// Runs `f` on the blocking pool once `lane` has a free slot (and, for `Probe`/`Long`, once
    /// `device` has none running). Waiting here is never a failure of the tool (P-3).
    pub async fn run<T: Send + 'static>(
        &self,
        lane: Lane,
        device: Option<&ResourceId>,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        // The device first: a call waiting for its device holds no lane slot meanwhile.
        let device_permit: Option<OwnedSemaphorePermit> = match device {
            Some(r) if lane.per_device() => Some(
                self.device_slot(r)
                    .acquire_owned()
                    .await
                    .expect("a device semaphore is never closed"),
            ),
            _ => None,
        };
        let lane_permit = self.lanes[&lane]
            .clone()
            .acquire_owned()
            .await
            .expect("a lane semaphore is never closed");
        let out = tokio::task::spawn_blocking(f).await;
        drop(lane_permit);
        drop(device_permit);
        match out {
            Ok(value) => value,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }

    /// Calls running or waiting nowhere: free slots of `lane` (tests and diagnostics).
    pub fn free(&self, lane: Lane) -> usize {
        self.lanes[&lane].available_permits()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    fn dev(i: u32) -> ResourceId {
        ResourceId(format!("device:test:{i}"))
    }

    /// Six slow Long calls never delay discovery or the sessions lane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lanes_never_wait_on_each_other() {
        let gate = Arc::new(ToolGate::new());
        let mut slow = Vec::new();
        for i in 0..6 {
            let gate = gate.clone();
            slow.push(tokio::spawn(async move {
                gate.run(Lane::Long, Some(&dev(i)), || {
                    std::thread::sleep(Duration::from_millis(800))
                })
                .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let start = Instant::now();
        for lane in [
            Lane::DiscoveryAndroid,
            Lane::DiscoveryIos,
            Lane::Sessions,
            Lane::Probe,
        ] {
            gate.run(lane, Some(&dev(9)), || ()).await;
        }
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "{:?}",
            start.elapsed()
        );
        for t in slow {
            t.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slots_per_lane_and_one_call_per_device() {
        let gate = Arc::new(ToolGate::new());
        for lane in Lane::ALL {
            assert_eq!(gate.free(lane), lane.slots());
        }
        // Long has two slots: three devices take at least two rounds.
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut calls = Vec::new();
        for i in 0..3 {
            let (gate, running, peak) = (gate.clone(), running.clone(), peak.clone());
            calls.push(tokio::spawn(async move {
                gate.run(Lane::Long, Some(&dev(i)), move || {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            }));
        }
        for c in calls {
            c.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);

        // One device: a Probe and a Long never overlap.
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut calls = Vec::new();
        for lane in [Lane::Probe, Lane::Long, Lane::Probe] {
            let (gate, running, peak) = (gate.clone(), running.clone(), peak.clone());
            calls.push(tokio::spawn(async move {
                gate.run(lane, Some(&dev(0)), move || {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            }));
        }
        for c in calls {
            c.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        for lane in Lane::ALL {
            assert_eq!(gate.free(lane), lane.slots(), "{lane:?} released");
        }
    }

    #[tokio::test]
    async fn a_panicking_call_releases_its_slot() {
        let gate = Arc::new(ToolGate::new());
        let g = gate.clone();
        let r =
            tokio::spawn(
                async move { g.run(Lane::DiscoveryAndroid, None, || panic!("boom")).await },
            )
            .await;
        assert!(r.is_err());
        assert_eq!(gate.free(Lane::DiscoveryAndroid), 1);
        assert_eq!(gate.run(Lane::DiscoveryAndroid, None, || 7).await, 7);
    }
}
