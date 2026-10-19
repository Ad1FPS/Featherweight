use crate::hidpp::{Device, Error};

const FEATURE_CONFIG_CHANGE: u16 = 0x0020;
const FEATURE_FORCE_PAIRING: u16 = 0x1500;
const FEATURE_1602: u16 = 0x1602;
const FEATURE_WIRELESS_DEVICE_STATUS: u16 = 0x1D4B;
const FEATURE_XY_STATS: u16 = 0x2250;
const FEATURE_WHEEL_STATS: u16 = 0x2251;
const FEATURE_MODE_STATUS: u16 = 0x8090;
const FEATURE_BUNNY_HOPPING: u16 = 0x80E0;
const FEATURE_MOUSE_BUTTON_SPY: u16 = 0x8110;

struct Target {
    id: u16,
    name: &'static str,
    hidden: bool,
    getter: bool,
}

const TARGETS: [Target; 25] = [
    Target { id: FEATURE_CONFIG_CHANGE, name: "ConfigChange", hidden: false, getter: true },
    Target { id: FEATURE_FORCE_PAIRING, name: "ForcePairing", hidden: false, getter: false },
    Target { id: FEATURE_1602, name: "unknown 0x1602", hidden: false, getter: false },
    Target { id: FEATURE_WIRELESS_DEVICE_STATUS, name: "WirelessDeviceStatus", hidden: false, getter: false },
    Target { id: FEATURE_XY_STATS, name: "XYStats", hidden: false, getter: false },
    Target { id: FEATURE_WHEEL_STATS, name: "WheelStats", hidden: false, getter: false },
    Target { id: FEATURE_MODE_STATUS, name: "ModeStatus", hidden: false, getter: true },
    Target { id: FEATURE_BUNNY_HOPPING, name: "BunnyHopping", hidden: false, getter: false },
    Target { id: FEATURE_MOUSE_BUTTON_SPY, name: "MouseButtonSpy / MouseButtonFilter", hidden: false, getter: true },
    Target { id: 0x1801, name: "unknown 0x1801", hidden: true, getter: false },
    Target { id: 0x1802, name: "DeviceReset", hidden: true, getter: false },
    Target { id: 0x1803, name: "unknown 0x1803", hidden: true, getter: false },
    Target { id: 0x1805, name: "OobState", hidden: true, getter: false },
    Target { id: 0x1806, name: "ConfigDeviceProps", hidden: true, getter: false },
    Target { id: 0x1817, name: "unknown 0x1817", hidden: true, getter: false },
    Target { id: 0x1830, name: "unknown 0x1830", hidden: true, getter: false },
    Target { id: 0x1861, name: "unknown 0x1861", hidden: true, getter: false },
    Target { id: 0x1875, name: "unknown 0x1875", hidden: true, getter: false },
    Target { id: 0x1890, name: "unknown 0x1890", hidden: true, getter: false },
    Target { id: 0x18A1, name: "unknown 0x18A1", hidden: true, getter: false },
    Target { id: 0x18B1, name: "unknown 0x18B1", hidden: true, getter: false },
    Target { id: 0x1E00, name: "EnableHiddenFeatures", hidden: true, getter: false },
    Target { id: 0x1E02, name: "unknown 0x1E02", hidden: true, getter: false },
    Target { id: 0x1E22, name: "unknown 0x1E22", hidden: true, getter: false },
    Target { id: 0x1EB0, name: "unknown 0x1EB0", hidden: true, getter: false },
];

pub fn feature_info() -> Result<(), Error> {
    let device = Device::open()?;
    let name = device.name()?;
    device.begin_trace("feature_info_log.txt")?;

    let features = device.feature_list()?;
    println!(
        "Feature info for {name:?} (device index {:#04X})",
        device.device_index()
    );
    println!("Every request/reply is appended to feature_info_log.txt.");
    println!();
    println!(
        "{:<6} {:<6} {:<4} {:<38} {}",
        "id", "index", "type", "name", "read"
    );

    for target in TARGETS {
        let found = features.iter().find(|feature| feature.id == target.id);
        let index = found
            .map(|feature| format!("{:#04X}", feature.index))
            .unwrap_or_else(|| String::from("--"));
        let type_byte = found
            .map(|feature| format!("{:#04X}", feature.type_byte))
            .unwrap_or_else(|| String::from("--"));
        let read = if found.is_none() {
            String::from("not present")
        } else if target.hidden {
            String::from("hidden/internal: no request sent")
        } else if !target.getter {
            String::from("no documented getter (not called)")
        } else {
            probe(&device, target.id)
        };

        println!(
            "{:<6} {:<6} {:<4} {:<38} {}",
            format!("{:#06X}", target.id),
            index,
            type_byte,
            target.name,
            read
        );
    }

    println!();
    println!("No setter, write, enable, reset or start function was called.");
    Ok(())
}

fn probe(device: &Device, id: u16) -> String {
    device.trace_label(&format!("resolve {id:#06X}"));
    if let Err(error) = device.feature_index(id) {
        return format!("resolve failed: {error}");
    }

    match id {
        FEATURE_CONFIG_CHANGE => {
            let reply = match read(device, id, 0, &[], "0x0020 fn0 GetCookie") {
                Ok(reply) => reply,
                Err(error) => return error,
            };
            let cookie = u16::from_be_bytes([reply[4], reply[5]]);
            format!("fn0 GetCookie: 0x{cookie:04X} | {}", hex(&reply))
        }
        FEATURE_MODE_STATUS => {
            let status = match read(device, id, 0, &[0, 0, 0], "0x8090 fn0 get_mode_status") {
                Ok(reply) => reply,
                Err(error) => return error,
            };
            let capabilities =
                match read(device, id, 2, &[0, 0, 0], "0x8090 fn2 get_device_config") {
                    Ok(reply) => reply,
                    Err(error) => return error,
                };
            let status0 = status[4];
            let status1 = status[5];
            let caps = u16::from_be_bytes([capabilities[4], capabilities[5]]);
            format!(
                "fn0 status0={status0:#04X} ({}) status1={status1:#04X} | \
                 fn2 caps={caps:#06X} (hw switch {}, sw switch {}) | {} | {}",
                if status0 & 0x01 != 0 {
                    "performance"
                } else {
                    "endurance"
                },
                if caps & 0x0001 != 0 { "yes" } else { "no" },
                if caps & 0x0002 != 0 { "yes" } else { "no" },
                hex(&status),
                hex(&capabilities)
            )
        }
        FEATURE_MOUSE_BUTTON_SPY => {
            let count_reply =
                match read(device, id, 0, &[], "0x8110 fn0 getMouseButtonCount") {
                    Ok(reply) => reply,
                    Err(error) => return error,
                };
            let count = count_reply[4] as usize;
            let mapping = match read(device, id, 3, &[], "0x8110 fn3 getMouseButtonMapping") {
                Ok(reply) => reply,
                Err(error) => return error,
            };
            let shown = count.min(16);
            let values = mapping[4..4 + shown]
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn0 count={count} | fn3 mapping (first {shown}): [{values}] | {} | {}",
                hex(&count_reply),
                hex(&mapping)
            )
        }
        _ => String::from("no documented getter (not called)"),
    }
}

fn read(
    device: &Device,
    id: u16,
    function: u8,
    params: &[u8],
    label: &str,
) -> Result<[u8; 20], String> {
    device.trace_label(label);
    device
        .read_feature(id, function, params)
        .map_err(|error| format!("ERROR {label}: {error}"))
}

fn hex(report: &[u8; 20]) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
