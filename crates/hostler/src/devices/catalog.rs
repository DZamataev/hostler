//! `DeviceCatalog`: registered devices as claimable resources, answered from the cache without
//! I/O (P3b Task 5; T5-4).

use std::collections::BTreeMap;
use std::sync::Arc;

use super::service::DeviceCache;
use crate::claims::ResourceCatalog;
use crate::contract::claims::{ResourceInfo, ResourceKind};
use crate::contract::devices::{DeviceInfo, DeviceKind, Platform, TransportKind};

pub struct DeviceCatalog(pub Arc<DeviceCache>);

impl ResourceCatalog for DeviceCatalog {
    fn resources(&self) -> Vec<ResourceInfo> {
        self.0.resources()
    }
}

fn platform_text(p: Platform) -> &'static str {
    match p {
        Platform::Android => "android",
        Platform::Ios => "ios",
    }
}

fn kind_text(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Physical => "physical",
        DeviceKind::Emulator => "emulator",
        DeviceKind::Simulator => "simulator",
    }
}

fn transport_text(k: TransportKind) -> &'static str {
    match k {
        TransportKind::Usb => "usb",
        TransportKind::Wifi => "wifi",
        TransportKind::Emulator => "emulator",
        TransportKind::Simulator => "simulator",
        TransportKind::Wired => "wired",
        TransportKind::Network => "network",
    }
}

/// A physical device is available while connected; a virtual one while it exists (`ready` says
/// whether it runs). Never while hostler shuts it down or removes it (P-6, P2-14).
fn available(d: &DeviceInfo) -> bool {
    let exists = match d.kind {
        DeviceKind::Physical => d.connected,
        DeviceKind::Emulator | DeviceKind::Simulator => d.connected || d.ready,
    };
    exists && !d.shutting_down && !d.removing
}

/// Catalog order: registered physical connected devices (USB before Wi-Fi), then ready virtual,
/// then not-ready virtual, then disconnected physical ones; each by alias.
fn rank(d: &DeviceInfo) -> (u8, u8) {
    let transport = d.transports.first().map(|t| t.kind);
    match (d.kind, d.connected, d.ready) {
        (DeviceKind::Physical, true, _) => (
            0,
            match transport {
                Some(TransportKind::Usb | TransportKind::Wired) => 0,
                _ => 1,
            },
        ),
        (DeviceKind::Physical, false, _) => (3, 0),
        (_, _, true) => (1, 0),
        (_, _, false) => (2, 0),
    }
}

/// The catalog entry of every registered device. `display` is the alias, the `--confirmation`
/// token (T1-3).
pub fn entries(devices: &[DeviceInfo]) -> Vec<ResourceInfo> {
    let mut registered: Vec<&DeviceInfo> = devices.iter().filter(|d| d.registered).collect();
    registered.sort_by(|a, b| {
        (rank(a), a.alias.as_deref(), &a.resource).cmp(&(rank(b), b.alias.as_deref(), &b.resource))
    });
    registered
        .into_iter()
        .map(|d| {
            let current = d.transports.first().filter(|_| d.connected);
            let mut attributes = BTreeMap::from([
                ("platform".to_owned(), platform_text(d.platform).to_owned()),
                ("kind".to_owned(), kind_text(d.kind).to_owned()),
                ("model".to_owned(), d.model.clone()),
                ("ready".to_owned(), d.ready.to_string()),
                (
                    "transport".to_owned(),
                    current
                        .map_or("none", |t| transport_text(t.kind))
                        .to_owned(),
                ),
            ]);
            match d.platform {
                Platform::Android => {
                    if let Some(t) = current {
                        attributes.insert("serial".into(), t.address.clone());
                        attributes.insert("agent_device_id".into(), t.address.clone());
                    }
                }
                Platform::Ios => {
                    attributes.insert("udid".into(), d.hardware_id.clone());
                    attributes.insert("agent_device_id".into(), d.hardware_id.clone());
                }
            }
            ResourceInfo {
                id: d.resource.clone(),
                kind: ResourceKind::Device,
                display: d.alias.clone().unwrap_or_else(|| d.resource.0.clone()),
                attributes,
                available: available(d),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::claims::ResourceId;
    use crate::contract::devices::Transport;

    fn device(
        id: &str,
        alias: Option<&str>,
        kind: DeviceKind,
        transport: Option<TransportKind>,
        connected: bool,
        ready: bool,
    ) -> DeviceInfo {
        let platform = if id.starts_with("device:ios") {
            Platform::Ios
        } else {
            Platform::Android
        };
        DeviceInfo {
            resource: ResourceId(id.into()),
            hardware_id: id.rsplit(':').next().unwrap().into(),
            platform,
            kind,
            registered: alias.is_some(),
            alias: alias.map(Into::into),
            alias_by_person: false,
            model: "Model".into(),
            name: None,
            os_version: None,
            note: None,
            connected,
            ready,
            transports: transport
                .map(|kind| Transport {
                    kind,
                    address: format!("{}-addr", id.rsplit(':').next().unwrap()),
                    state: "device".into(),
                })
                .into_iter()
                .collect(),
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

    #[test]
    fn catalog_lists_registered_devices_only_in_order() {
        use DeviceKind::*;
        let devices = vec![
            device(
                "device:android:avd:b",
                Some("b-avd"),
                Emulator,
                None,
                false,
                false,
            ),
            device(
                "device:android:W1",
                Some("z-wifi"),
                Physical,
                Some(TransportKind::Wifi),
                true,
                true,
            ),
            device(
                "device:android:U1",
                Some("y-usb"),
                Physical,
                Some(TransportKind::Usb),
                true,
                true,
            ),
            device(
                "device:android:avd:a",
                Some("a-avd"),
                Emulator,
                Some(TransportKind::Emulator),
                true,
                true,
            ),
            device(
                "device:android:OFF",
                Some("a-off"),
                Physical,
                None,
                false,
                false,
            ),
            device(
                "device:android:NEW",
                None,
                Physical,
                Some(TransportKind::Usb),
                true,
                true,
            ),
            device(
                "device:ios:SIM",
                Some("c-sim"),
                Simulator,
                Some(TransportKind::Simulator),
                false,
                false,
            ),
        ];
        let order: Vec<String> = entries(&devices).into_iter().map(|e| e.display).collect();
        assert_eq!(
            order,
            ["y-usb", "z-wifi", "a-avd", "b-avd", "c-sim", "a-off"]
        );
    }

    #[test]
    fn catalog_display_is_the_alias_and_token_matches() {
        let devices = vec![device(
            "device:android:U1",
            Some("pixel-9"),
            DeviceKind::Physical,
            Some(TransportKind::Usb),
            true,
            true,
        )];
        let e = &entries(&devices)[0];
        assert_eq!(e.display, "pixel-9");
        assert_eq!(crate::claims::engine::token_of(e), "pixel-9");
        assert_eq!(e.kind, ResourceKind::Device);
        assert_eq!(e.attributes["platform"], "android");
        assert_eq!(e.attributes["kind"], "physical");
        assert_eq!(e.attributes["transport"], "usb");
        assert_eq!(e.attributes["serial"], "U1-addr");
        assert_eq!(e.attributes["agent_device_id"], "U1-addr");
        assert_eq!(e.attributes["ready"], "true");
        assert!(e.available);
    }

    #[test]
    fn availability_follows_connection_existence_and_marks() {
        use DeviceKind::*;
        let mut off = device(
            "device:android:OFF",
            Some("off"),
            Physical,
            None,
            false,
            false,
        );
        assert!(!entries(std::slice::from_ref(&off))[0].available);
        off.connected = true;
        off.transports = vec![Transport {
            kind: TransportKind::Usb,
            address: "OFF".into(),
            state: "device".into(),
        }];
        assert!(entries(std::slice::from_ref(&off))[0].available);
        // A virtual device that exists but does not run is available with ready false.
        let avd = device(
            "device:android:avd:p",
            Some("p"),
            Emulator,
            None,
            true,
            false,
        );
        let e = &entries(std::slice::from_ref(&avd))[0];
        assert!(e.available);
        assert_eq!(e.attributes["ready"], "false");
        assert_eq!(e.attributes["transport"], "none");
        assert!(!e.attributes.contains_key("serial"));
        for mark in [0, 1] {
            let mut d = avd.clone();
            if mark == 0 {
                d.shutting_down = true;
            } else {
                d.removing = true;
            }
            assert!(!entries(&[d])[0].available);
        }
        // iOS: the UDID.
        let sim = device(
            "device:ios:SIM-1",
            Some("sim"),
            Simulator,
            Some(TransportKind::Simulator),
            true,
            true,
        );
        let e = &entries(&[sim])[0];
        assert_eq!(e.attributes["udid"], "SIM-1");
        assert_eq!(e.attributes["agent_device_id"], "SIM-1");
    }
}
