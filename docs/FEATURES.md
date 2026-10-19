# HID++ feature research

Read-only research notes for the Logitech PRO X 2. No setter/write/enable/reset/start
function was called during the research, and no request was sent to any 0x18xx or 0x1Exx
feature. The runtime requests were logged (43 requests: feature indices 0x00, 0x01, 0x05,
0x0A, 0x0E; functions 0, 1, 2, 3; root lookups only for 0x0001, 0x0020, 0x8090, 0x8110).

## Sources

- **S1** Solaar feature IDs: `lib/logitech_receiver/hidpp20_constants.py` —
  https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/hidpp20_constants.py
- **S2** Solaar ConfigChange getter/setter: `lib/logitech_receiver/hidpp20.py`
  (`get_configuration_cookie`, `set_configuration_complete`) —
  https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/hidpp20.py
- **S3** libratbag feature names and OnboardProfiles structs: `src/hidpp20.h`, `src/hidpp20.c` —
  https://github.com/libratbag/libratbag/blob/master/src/hidpp20.h
- **S4** OpenLogi feature registry: `crates/openlogi-hidpp/src/feature/registry.rs` —
  https://github.com/AprilNEA/OpenLogi/blob/master/crates/openlogi-hidpp/src/feature/registry.rs
- **S5** OpenLogi ModeStatus implementation and reference page —
  https://github.com/AprilNEA/OpenLogi/blob/master/crates/openlogi-hidpp/src/feature/mode_status.rs
  and https://openlogi.org/hidpp/features/x8090-mode-status
- **S6** OpenLogi MouseButtonFilter notes (name only, no typed implementation) —
  https://openlogi.org/hidpp/features/x8110-mouse-button-filter
- **S7** OpenLogi WirelessDeviceStatus implementation and reference page —
  https://github.com/AprilNEA/OpenLogi/blob/master/crates/openlogi-hidpp/src/feature/wireless_device_status.rs
  and https://openlogi.org/hidpp/features/x1d4b-wireless-device-status
- **S8** Clément Vuchener's HID++ tools, `IMouseButtonSpy.h/.cpp` (documented function numbers) —
  https://github.com/cvuchener/hidpp/blob/master/src/libhidpp/hidpp20/IMouseButtonSpy.h
- **S9** Solaar PR #3345 (open, not merged): format-7 onboard profile layout —
  https://github.com/pwr-Solaar/Solaar/pull/3345
- **S10** Solaar master format 1..5 profile layout (`OnboardProfile.from_bytes`, `angle_snap`,
  `ps_timeout`, `po_timeout`) — `lib/logitech_receiver/hidpp20.py` (S2)
- **S11** OpenLogi feature index and names (XyStats, WheelStats, BunnyHopping is absent there) —
  https://openlogi.org/hidpp/features

"Unknown" means no public source was found for that feature's name or functions; it is not a guess.

## Feature table

| Feature ID | Name (source) | Source(s) | Documented getters/setters (layout) | Observed value | User-facing setting? | Risk notes |
|---|---|---|---|---|---|---|
| 0x0020 | ConfigChange (S1) / Reset (S3) | S1, S2, S3 | **fn 0 GetCookie**, no params, reply payload 2 bytes = cookie. **fn 1 (0x10) SetComplete**, payload = 2-byte cookie. | Cookie `0x0000` (raw `11 01 05 0A 00 00 ...`). | none (host/device configuration handshake) | fn 1 is a setter; only fn 0 was called. libratbag calls this page "Reset", Solaar "ConfigChange"; name conflict unresolved. |
| 0x1500 | ForcePairing (S1) | S1 | No public getter/setter layout found. | not read | none (pairing) | Pairing-related. Calling unknown functions could disrupt the link; not called. |
| 0x1602 | unknown | none | No public source found (not in S1, S3, S4, S8, S11). | not read | unknown | Unknown function semantics; not called. Type byte 0x00 (not flagged hidden). |
| 0x1D4B | WirelessDeviceStatus (S1, S3, S7) | S1, S3, S7 | **Event-only.** No request functions. Event sub-id 0 payload: status (0x00 unknown, 0x01 reconnection), request (0x00 none, 0x01 reconfiguration needed), reason (0x00 unknown, 0x01 power switch). | no value (broadcast only) | none | Safe to listen to; the host never sends a request. Not called. |
| 0x2250 | XYStats (S1, S4) | S1, S4 | Name/purpose only ("X/Y motion statistics"). No getter/setter layout found. | not read | none (telemetry) | No layout; not called. |
| 0x2251 | WheelStats (S1, S4) | S1, S4 | Name/purpose only ("scroll-wheel motion statistics"). No getter/setter layout found. | not read | none (telemetry) | No layout; not called. |
| 0x8090 | ModeStatus (S1, S4, S5) | S1, S4, S5 | **fn 0 get_mode_status**, request `[00 00 00]`, reply byte 0 = status0 (bit 0 PERFORMANCE, clear = endurance), byte 1 = status1. **fn 2 get_device_config**, request `[00 00 00]`, reply big-endian u16 caps (bit 0 hardware switch, bit 1 software switch). **fn 1 set_mode_status**, 16-byte payload `[status0, status1, changed_mask0, changed_mask1, ...]`. | status0 `0x00` (endurance), status1 `0x02`, caps `0x000C`. | other (performance/endurance power mode); no G HUB source confirms it for this mouse | fn 1 is a setter and could change power/performance behaviour. Caps bits 2 and 3 are set but are **not** defined by S5; interpretation unconfirmed. |
| 0x80E0 | BunnyHopping (S1) | S1 | Name only. No getter/setter layout found (not in S4/S11). | not read | unknown | Name suggests a gaming/input behaviour feature; unknown semantics. Not called. |
| 0x8110 | MouseButtonSpy (S1, S8) / MouseButtonFilter (S6) | S1, S6, S8 | **fn 0 GetMouseButtonCount**, no params, reply byte 0 = count. **fn 3 GetMouseButtonMapping**, no params, reply payload = one byte per button (0 = disabled, 1..16 = code). **fn 1 Start**, **fn 2 Stop**, **fn 4 SetMouseButtonMapping** (setters/start-stop). Event fn 0 = 16-bit button bits. | count = **5**; mapping first 5 = **1, 2, 3, 4, 5**. | none (diagnostic; button filtering/remapping) | fn 1/2/4 change button reporting; only fn 0 and fn 3 were called. S6 describes a filter mask but gives no function numbers; S8 is the only source with the function layout. |
| 0x1801 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1802 | DeviceReset (S1, S4, S11) | S1, S4, S11 | Name/purpose only (device software reset). No getter/setter layout documented. | not read | none | Reset feature; never call. No request sent. |
| 0x1803 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1805 | OobState (S1, S4, S11) | S1, S4, S11 | Name/purpose only (out-of-box state management). No function layout documented. | not read | none | Could restore factory state; never call. No request sent. |
| 0x1806 | ConfigDeviceProps (S1, S4) | S1, S4 | Name/purpose only (configurable device properties). No function layout documented. | not read | none | Unknown device configuration; no request sent. |
| 0x1817 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1830 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1861 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1875 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1890 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x68). No request sent. |
| 0x18A1 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x18B1 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1E00 | EnableHiddenFeatures (S1, S4) | S1, S4 | Name/purpose only (engineering enable). No function layout documented. | not read | none | Engineering feature; never call. No request sent. |
| 0x1E02 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x60). No request sent. |
| 0x1E22 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |
| 0x1EB0 | unknown (hidden) | none | No public source. | not read | none | Hidden/internal (type byte 0x70). No request sent. |

Type-byte flags: Solaar's `FeatureFlag` defines INTERNAL 0x20, HIDDEN 0x40, OBSOLETE 0x80,
so 0x70 = hidden + internal + 0x10, 0x68 = hidden + internal + 0x08, 0x60 = hidden + internal,
0x40 = hidden. This matches the observed values.

## Onboard profile 1 (sector 0x0001)

Source of the layout: S9 (format 7) plus verified read-only dumps (sector CRC 0x5A96). No new
mouse read was needed.

| Offset | Field | Value | Confidence |
|---|---|---|---|
| 0x00 | report-rate index (0x8061 index) | 0x03 = 1000 Hz | confirmed on hardware |
| 0x01 | default DPI index | 3 | documented by S9; default index points at an empty stage |
| 0x02 | shifted DPI index | 0 | documented by S9 |
| 0x03 | active DPI index | 0 | documented by S9; live DPI comes from stage 0 |
| 0x04..0x08 | stage 0 X, Y (u16 LE), LOD | 1600, 1600, LOD 1 | confirmed |
| 0x09..0x1C | stages 1..4 | zero/empty | confirmed |
| 0x1D..0x2B | reserved (15 bytes) | `00 00 00 00 FF 00 FF FF FF FF FF FF FF FF FF` | documented by S9 |
| 0x2C..0x2D | power-save timeout (u16 LE) | **0x003C = 60** | offset documented by S9; units unconfirmed (likely seconds) |
| 0x2E..0x2F | power-off timeout (u16 LE) | **0x012C = 300** | offset documented by S9; units unconfirmed (likely seconds) |
| 0x30..0x6F | 16 button records (4 bytes each) | five mouse-button records, rest `FF` | documented by S9; behaviour not decoded here |
| 0x70..0xFC | unknown tail (141 bytes) | 96 x `FF`, then 45 bytes: `03 00 00 00 00 00 1F 40 00 00 00` repeated twice, `03 00 00 00 00 00 1F 40 32 00 00` repeated twice, then `03` | **unconfirmed** |
| 0xFD..0xFE | CRC-16-CCITT big-endian | 0x5A96 (MATCH) | confirmed |

The 45-byte tail is identical in profiles 1..5 and in the ROM candidate 0x0101. It does **not**
match any field documented for format 7. Its repeating shape suggests four 11-byte records
(`03`, five zero bytes, `1F 40` = 8000, `00 00 00` or `32 00 00`) plus a trailing `03`, but the
last record would be cut by the CRC, so this is **unconfirmed** and must not be written.

### Where the requested settings live

- **Sleep / power timeout: found, in the profile sector, not in a feature.** Format 7 stores
  `ps_timeout` (power-save, "Sleep after") at 0x2C and `po_timeout` (power-off, "Deep sleep") at 0x2E
  (little-endian u16; names from S9/S10). Observed values 60 and 300. Confidence: **high** for the
  offsets (S9 + dump), **medium** for the units (no source states seconds; 60/300 is consistent with
  seconds, not minutes). The app treats them as seconds — **unconfirmed; still to be verified.**
  The verified user-sector 0x0001 save path changes only these two words (plus the existing
  stage-0 DPI/LOD and report-rate byte), with the same fresh-read/backup/CRC/
  byte-exact-read-back/restore rules.
- **Angle snapping: not available through a documented format-7 field.** The dedicated feature
  0x2230 SensorAngleSnapping is **absent** from this mouse. Older profile formats 1..5 store an
  angle-snap byte at offset 17 (S10), but in format 7 offset 17 is inside the DPI-stage array,
  and S9 does not decode an angle-snap field. Confidence: **high** that no documented format-7
  angle-snap field exists; it may be in the unknown tail, which would be unconfirmed.
- **Motion Sync: not found.** No HID++ feature, profile field, or Solaar/libratbag/OpenLogi
  reference documents a Motion Sync setting. Confidence: **medium** that it is not exposed by
  this protocol/firmware; it may be fixed in sensor firmware.
- **Ripple Control: not found.** Ripple Control appears in third-party mouse vendor software
  (Corsair, Pulsar, Endgame Gear, etc.), not in Logitech HID++ sources. Confidence: **medium**
  that it is not exposed for this mouse.

## GUI candidates

Only settings with a documented getter **and** a documented setter are listed. There are no
other such candidates among the researched features.

1. **ModeStatus 0x8090 (performance / endurance)** — getter fn 0 + fn 2, setter fn 1.
   Usefulness: **medium** (power/performance trade-off). A write experiment would need to:
   read status0/status1 and caps; set only the PERFORMANCE bit with changed-mask 0x01; read back;
   check the live polling rate, battery drain and sleep timing before/after; check persistence
   across a mouse power cycle and interaction with OnboardProfiles host/onboard mode; restore the
   original values. The setter is documented, but the meaning of status1=0x02 and caps=0x000C is
   not, so the experiment must first characterise them read-only.
2. **MouseButtonSpy / MouseButtonFilter 0x8110 (button mapping/filtering)** — getter fn 0 + fn 3,
   setter fn 4 (plus Start/Stop fn 1/2). Usefulness: **low** (diagnostic/remap, not a normal
   setting). A write experiment would need to: read count and mapping; change one mapping with
   fn 4; read it back; verify normal HID button reports follow the mapping (and whether onboard
   mode disables it); check whether it resets on power cycle; restore the mapping. fn 1/2 could
   swallow button events if left running, so the experiment must stop the spy afterwards.

ConfigChange 0x0020 has a documented getter (fn 0) and setter (fn 1), but it is a protocol
handshake, not a user-facing setting; it is excluded.

## Profile-sector-only list (not feature getter/setter candidates)

- **Sleep/power timeouts** — sector 0x0001 offsets 0x2C/0x2E (`ps_timeout`/`po_timeout`, u16 LE).
  The verified profile memory-write path may change these words (and only these, besides stage 0
  X/Y/LOD and byte 0x00) through `save_to_mouse`/`save-to-mouse` and the GUI sleep rows. Units are
  assumed to be seconds but remain **unconfirmed**; no sleep-timing test was run.
- **Report rate, DPI stages, LOD, buttons** — already in sector 0x0001 and partly exposed by
  existing features/CLI; not part of this research.
- **Unknown 45-byte tail (0x70..0xFC)** — possibly contains additional per-profile settings
  (surface tuning, sensor options, LED data, etc.), but no public source decodes it. Treat as
  **unconfirmed**; do not write.
- **Angle snapping / Motion Sync / Ripple Control** — no documented feature or format-7 profile
  field. Nothing to expose until a source or a controlled read-only experiment identifies them.
