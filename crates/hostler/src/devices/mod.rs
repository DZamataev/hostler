//! Devices (plan P3b): discovery, the registry, telemetry, installs, boots and agent-device
//! sessions, all in the service. P3b Task 1 creates every module as a stub; each later task fills
//! its own (the plan's ownership table).
//!
//! Product rule (spec anti-goal 2): hostler only reads from devices. Its only state-changing
//! calls boot an existing AVD or simulator for a `--boot` claim, shut down what it booted itself,
//! and close an agent-device session in a takeover the operator permitted.

pub mod agent_device;
pub mod android;
pub mod boot;
pub mod catalog;
pub mod installs;
pub mod ios;
pub mod lanes;
pub mod probes;
pub mod service;
pub mod telemetry;
pub mod tools;
pub mod windows;
