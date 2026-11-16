# P3b device fixtures (sanitized, 2026-11-08)
Captured read-only from this machine's phones, emulator, simulators and agent-device 0.21.36, then
sanitized by `prospero/sanitize.py` plus a second pass: serials, UDIDs, CoreDevice ids, IPs, MACs,
the Wi-Fi name, home paths and every app id replaced; network-agent lines cut to `ni{…}` + the
`Transports/Capabilities` part. No fixture holds a real serial, device id, address, network
name, workspace hash, pid or person; device models are kept (they are public).
- adb-devices-l.txt — 4 USB phones, the Samsung also over Wi-Fi (`10.0.0.7:5555`, no `usb:` field),
  an emulator line (real shape) and a synthetic `unauthorized` line.
- battery-a10|a14|a17.txt — `dumpsys battery` (Android 10, 14, 17); a10 is USB powered but status 4.
- network-agents-*.txt — `dumpsys connectivity | grep NetworkAgentInfo{`; a14-vpn-ims has WIFI, VPN
  and an IMS-only MOBILE agent (no INTERNET). No captured phone had mobile data on: tests add a
  synthetic `Transports: CELLULAR Capabilities: INTERNET&…` line.
- wifi-*.txt — the first `mWifiInfo SSID:` line (quoted on 14/17, unquoted on 10);
  wifi-a10-no-bssid.txt is an Android 10 vendor build's line without a `BSSID` field
  (Task 13 F-1), synthetic name.
- pm-list-3-a10|a17.txt, dumpsys-package-a10|a17.txt — 3 user packages; firstInstallTime is
  package-level on 10, under `User 0:` on 17.
- devicectl-list.json, devicectl-apps.json — `xcrun devicectl … --json-output` (devicectl 518.33).
- simctl-list-devices.json, simctl-listapps.json — `simctl list devices --json`, `listapps | plutil -convert json`.
- ideviceinfo-battery.txt — `ideviceinfo -q com.apple.mobile.battery` (libimobiledevice 1.4.0).
- agent-device-status-0.21.36.json — `agent-device device status --json` with an iOS-simulator claim
  and an Android-phone claim (deviceKey `local:android:none:<serial>`).
  Its owner fields are synthetic (L57): the session hash is `cwd:0000000000000000`, the pids
  (11111, 22222, 33333) and the start times are fake; only the `ctime` shape is real.

Round-1 additions (P3b Task 2, synthetic or sanitized the same way):
- agent-device-status-iphone.json — `device status --json` with a simulator claim and a physical-iPhone claim
  (`kind: device`, `deviceKey local:apple:ios:<hardware UDID>`; the UDID is devicectl-list.json's wired phone).
- adb-devices-l-tls.txt — synthetic: a USB line plus an Android 11+ wireless-debugging
  `adb-<id>._adb-tls-connect._tcp` line of the same phone.
- adb-mismatch.txt — synthetic: the stderr shape of an adb client meeting a server of another version.
- agent-device.cmd — the npm (cmd-shim) launcher shape on Windows, for resolving the package's bin script.
