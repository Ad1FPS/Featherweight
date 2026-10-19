use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use gpx_tool::hidpp::{self, Battery, Device, Error};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("status");

    let result = match command {
        "status" => status(),
        "devices" => devices(),
        "features" => features(),
        "feature-info" => gpx_tool::features::feature_info(),
        "battery-watch" => battery_watch(),
        "ranges" => ranges(args.iter().any(|arg| arg == "--raw")),
        "rate" => rate(args.iter().any(|arg| arg == "--raw")),
        "set-dpi" => match args.get(1).and_then(|value| value.parse::<u16>().ok()) {
            Some(value) => set_dpi_command(value),
            None => {
                eprintln!("usage: gpx-tool set-dpi <dpi>");
                return ExitCode::FAILURE;
            }
        },
        "set-rate" => match args.get(1).and_then(|value| value.parse::<u16>().ok()) {
            Some(hz) => set_rate_command(hz),
            None => {
                eprintln!("usage: gpx-tool set-rate <hz>");
                return ExitCode::FAILURE;
            }
        },
        "set-lod" => match args.get(1).and_then(|value| value.parse::<u8>().ok()) {
            Some(lod) => set_lod_command(lod),
            None => {
                eprintln!("usage: gpx-tool set-lod <1|2|3>");
                return ExitCode::FAILURE;
            }
        },
        "onboard" => onboard(),
        "profile-dump" => gpx_tool::profile::dump(),
        "restore-sector" => match args.get(1) {
            Some(file) => gpx_tool::profile::restore_profile_1(
                Path::new(file),
                args.iter().any(|arg| arg == "--yes"),
            ),
            None => {
                eprintln!("usage: gpx-tool restore-sector <255-byte sector 0x0001 backup> --yes");
                return ExitCode::FAILURE;
            }
        },
        "sleep-timers" => gpx_tool::profile::read_sleep_timers().map(|timers| {
            println!(
                "Sleep after: {} s (ps_timeout=0x{:04X}) | Deep sleep after: {} s (po_timeout=0x{:04X})",
                timers.ps_timeout, timers.ps_timeout, timers.po_timeout, timers.po_timeout
            );
        }),
        "save-dpi" | "save-to-mouse" => {
            let usage = "usage: gpx-tool save-to-mouse [<dpi>] [--dpi N] [--lod 1|2|3] \
                         [--rate 125|250|500|1000|2000|4000|8000] [--sleep SECONDS] \
                         [--deep-sleep SECONDS] --yes";
            let mut fields = gpx_tool::profile::SaveFields::default();
            let mut index = 1;
            if let Some(value) = args.get(1).and_then(|value| value.parse::<u16>().ok()) {
                fields.dpi = Some(value);
                index = 2;
            }
            while index < args.len() {
                match args[index].as_str() {
                    "--dpi" => match args.get(index + 1).and_then(|v| v.parse::<u16>().ok()) {
                        Some(value) => fields.dpi = Some(value),
                        None => {
                            eprintln!("{usage}");
                            return ExitCode::FAILURE;
                        }
                    },
                    "--lod" => match args.get(index + 1).and_then(|v| v.parse::<u8>().ok()) {
                        Some(value) => fields.lod = Some(value),
                        None => {
                            eprintln!("{usage}");
                            return ExitCode::FAILURE;
                        }
                    },
                    "--rate" => match args.get(index + 1).and_then(|v| v.parse::<u16>().ok()) {
                        Some(value) => fields.rate = Some(value),
                        None => {
                            eprintln!("{usage}");
                            return ExitCode::FAILURE;
                        }
                    },
                    "--sleep" => match args.get(index + 1).and_then(|v| v.parse::<u16>().ok()) {
                        Some(value) => fields.ps_timeout = Some(value),
                        None => {
                            eprintln!("{usage}");
                            return ExitCode::FAILURE;
                        }
                    },
                    "--deep-sleep" => {
                        match args.get(index + 1).and_then(|v| v.parse::<u16>().ok()) {
                            Some(value) => fields.po_timeout = Some(value),
                            None => {
                                eprintln!("{usage}");
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            if fields.is_empty() {
                eprintln!("{usage}");
                return ExitCode::FAILURE;
            }
            gpx_tool::profile::save_to_mouse(fields, args.iter().any(|arg| arg == "--yes"))
                .map(|_| ())
        }
        "-h" | "--help" => {
            println!(
                "usage: gpx-tool [status | devices | features | feature-info | battery-watch | ranges [--raw] | rate [--raw] | \
                 set-dpi <dpi> | set-rate <hz> | set-lod <1|2|3> | onboard | \
                 save-to-mouse [<dpi>] [--dpi N] [--lod 1|2|3] [--rate HZ] \
                 [--sleep SECONDS] [--deep-sleep SECONDS] --yes | sleep-timers | \
                 profile-dump | restore-sector <file> --yes]"
            );
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "usage: gpx-tool [status | devices | features | feature-info | battery-watch | ranges [--raw] | rate [--raw] | \
                 set-dpi <dpi> | set-rate <hz> | set-lod <1|2|3> | onboard | \
                 save-to-mouse [<dpi>] [--dpi N] [--lod 1|2|3] [--rate HZ] \
                 [--sleep SECONDS] [--deep-sleep SECONDS] --yes | sleep-timers | \
                 profile-dump | restore-sector <file> --yes]"
            );
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn status() -> Result<(), Error> {
    let snapshot = hidpp::snapshot()?;

    let mut line = format!(
        "{} | Battery {}% ({}) | DPI {} (default {}, Y {}, LOD {}) | Mode: {}",
        snapshot.name,
        snapshot.battery.percent,
        snapshot.battery.status_text(),
        snapshot.dpi.x,
        snapshot.dpi.default_x,
        snapshot.dpi.y,
        snapshot.dpi.lod,
        mode_text(snapshot.mode)
    );
    if let Some(rate) = &snapshot.rate {
        line.push_str(&format!(" | Rate {} Hz", rate.current_hz));
    }
    println!("{line}");

    Ok(())
}

fn devices() -> Result<(), Error> {
    let discovery = hidpp::discover()?;

    if discovery.interfaces.is_empty() {
        println!(
            "No HID++ long-report interfaces (VID 046D, usage page 0xFF00 / usage 0x0002) found."
        );
    } else {
        println!("HID++ interfaces:");
        for interface in &discovery.interfaces {
            println!("  PID {:04X}  {}", interface.pid, interface.path_summary);
            if interface.devices.is_empty() {
                println!("    (no device index answered)");
            }
            for device in &interface.devices {
                let name = device
                    .name
                    .as_deref()
                    .map(|name| format!("{name:?}"))
                    .unwrap_or_else(|| String::from("(unknown)"));
                println!(
                    "    index {:#04X}: name {}, battery {}, DPI {}, {} ms",
                    device.index,
                    name,
                    yes_no(device.battery),
                    yes_no(device.dpi),
                    device.elapsed_ms
                );
            }
        }
    }

    match &discovery.selected {
        Some(selected) => {
            let name = selected
                .name
                .as_deref()
                .map(|name| format!(", name {name:?}"))
                .unwrap_or_default();
            println!(
                "Selected: PID {:04X} index {:#04X}{name} ({})",
                selected.pid, selected.index, selected.path_summary
            );
            println!("Reason: {}", discovery.reason);
        }
        None => println!("Selected: none. Reason: {}", discovery.reason),
    }

    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn features() -> Result<(), Error> {
    let device = Device::open()?;
    let name = device.name()?;

    println!(
        "Features of {name:?} (device index {:#04X}):",
        device.device_index()
    );
    println!("  index  feature  type  name");
    for feature in device.feature_list()? {
        println!(
            "  {:5}  0x{:04X}  0x{:02X}  {}",
            feature.index,
            feature.id,
            feature.type_byte,
            feature_name(feature.id)
        );
    }

    Ok(())
}

fn feature_name(id: u16) -> &'static str {
    match id {
        0x0000 => "IRoot",
        0x0001 => "IFeatureSet",
        0x0003 => "DeviceFwVersion",
        0x0005 => "DeviceName",
        0x1D4B => "WirelessDeviceStatus",
        0x0020 => "ConfigChange",
        0x1004 => "UnifiedBattery",
        0x2202 => "ExtendedAdjustableDPI",
        0x8061 => "ExtendedAdjustableReportRate",
        0x8100 => "OnboardProfiles",
        0x8110 => "MouseButtonSpy",
        0x1500 => "",
        0x2250 => "",
        0x2251 => "",
        0x8090 => "",
        0x80E0 => "",
        _ => "",
    }
}

fn set_dpi_command(value: u16) -> Result<(), Error> {
    let dpi = hidpp::set_dpi(value)?;
    println!("DPI set to {}.", dpi.x);
    println!(
        "The mouse is now in host mode (app controls DPI). Run `gpx-tool onboard` to return \
         to the onboard profile."
    );
    Ok(())
}

fn set_rate_command(hz: u16) -> Result<(), Error> {
    let rate = hidpp::set_report_rate(hz)?;
    println!("Polling rate set to {} Hz.", rate.current_hz);
    println!(
        "The mouse is now in host mode (host mode is required for polling-rate changes). Run \
         `gpx-tool onboard` to return to the onboard profile."
    );
    Ok(())
}

fn set_lod_command(lod: u8) -> Result<(), Error> {
    let dpi = hidpp::set_lod(lod)?;
    println!("LOD set to {} (Low 1, Medium 2, High 3).", dpi.lod);
    println!(
        "The mouse is now in host mode (host mode is required for LOD changes). Run \
         `gpx-tool onboard` to return to the onboard profile."
    );
    Ok(())
}

fn onboard() -> Result<(), Error> {
    hidpp::set_onboard_mode()?;
    println!("Mouse is back in onboard mode (mouse profile).");
    Ok(())
}

pub fn mode_text(mode: u8) -> &'static str {
    match mode {
        1 => "onboard",
        2 => "host",
        _ => "unknown",
    }
}

fn ranges(raw: bool) -> Result<(), Error> {
    let device = Device::open()?;
    let words = device.dpi_range_words()?;

    if raw {
        let line = words
            .iter()
            .map(|word| format!("{word:04X}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("raw: {line}");
        let sensors = device.sensor_count()?;
        println!("sensors: {sensors}");
        for sensor in 0..sensors {
            let (levels, capabilities) = device.sensor_capabilities(sensor)?;
            println!("sensor {sensor}: dpi levels {levels}, capabilities 0x{capabilities:02X}");
        }
    }

    for range in hidpp::decode_dpi_ranges(&words) {
        println!("{}..{} step {}", range.from, range.to, range.step);
    }

    Ok(())
}

fn rate(raw: bool) -> Result<(), Error> {
    let device = Device::open()?;
    let supported = device.report_rate_supported_raw()?;
    let (connection, current) = device.report_rate_current_raw()?;

    if raw {
        println!("raw supported (fn 1): {}", hex_report(&supported));
        println!(
            "raw current (fn 2, connection {}): {}",
            connection,
            hex_report(&current)
        );
    }

    match hidpp::decode_report_rate_index(current[4]) {
        Some(hz) => println!("Current: {hz} Hz"),
        None => println!("Current: unknown (index {:#04X})", current[4]),
    }
    print_rate_list("Supported (fn 1)", supported[5]);

    for connection in [0u8, 1u8] {
        match device.report_rate_capabilities_raw(connection) {
            Ok(reply) => {
                if raw {
                    println!(
                        "raw capabilities conn {connection} (fn 0): {}",
                        hex_report(&reply)
                    );
                }
                print_rate_list(&format!("Supported conn {connection} (fn 0)"), reply[5]);
            }
            Err(error) => println!("Supported conn {connection} (fn 0): unavailable ({error})"),
        }

        match device.report_rate_current_for(connection) {
            Ok(reply) => {
                if raw {
                    println!(
                        "raw current conn {connection} (fn 2): {}",
                        hex_report(&reply)
                    );
                }
                match hidpp::decode_report_rate_index(reply[4]) {
                    Some(hz) => println!("Current conn {connection} (fn 2): {hz} Hz"),
                    None => println!(
                        "Current conn {connection} (fn 2): unknown (index {:#04X})",
                        reply[4]
                    ),
                }
            }
            Err(error) => println!("Current conn {connection} (fn 2): unavailable ({error})"),
        }
    }

    Ok(())
}

fn print_rate_list(label: &str, mask: u8) {
    let supported_hz = hidpp::decode_report_rate_list(mask);
    if supported_hz.is_empty() {
        println!("{label}: none reported (mask {mask:#04X})");
    } else {
        let rates = supported_hz
            .iter()
            .map(|hz| hz.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        println!("{label}: {rates} Hz (mask {mask:#04X})");
    }
}

/// Prints the raw UnifiedBattery reply and the decoded status every 2 s for
/// 60 s, switching between the receiver and the mouse's own wired interface
/// when the long-report interface set changes. Read-only.
fn battery_watch() -> Result<(), Error> {
    let started = Instant::now();
    let mut device = match Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("[  0.0s] initial open failed: {error}; retrying wired/receiver");
            Device::open_wired()
                .or_else(|_| Device::open_reconnect(true))
                .ok()
        }
    };

    for sample in 0..30 {
        if sample > 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        let stamp = format!("{:5.1}s", started.elapsed().as_secs_f64());

        if device.is_none() {
            device = Device::open_wired()
                .or_else(|_| Device::open_reconnect(true))
                .ok();
            if device.is_none() {
                println!("[{stamp}] path=unavailable (receiver and wired mouse not reachable)");
                continue;
            }
        }

        let pids = hidpp::long_report_interface_pids().unwrap_or_default();
        let switch_to_wired = match &device {
            Some(current) => {
                current.device_index() != 0xFF
                    && pids.iter().any(|pid| *pid != current.product_id())
            }
            None => true,
        };
        if switch_to_wired {
            if let Ok(wired) = Device::open_wired() {
                device = Some(wired);
            }
        }

        let Some(current) = device.take() else {
            continue;
        };
        let path = if current.device_index() == 0xFF {
            format!("wired (PID {:04X})", current.product_id())
        } else {
            format!(
                "receiver (PID {:04X}, index {:#04X})",
                current.product_id(),
                current.device_index()
            )
        };

        match current.battery_reply() {
            Ok(reply) => {
                let battery = Battery {
                    percent: reply[4],
                    level_flags: reply[5],
                    status: reply[6],
                    external_power: reply[7],
                };
                println!(
                    "[{stamp}] path={path} raw={} | {}% | status={} ({}) | level_flags=0x{:02X} | external_power={}",
                    hex_report(&reply),
                    battery.percent,
                    battery.status,
                    battery.status_text(),
                    battery.level_flags,
                    battery.external_power
                );
                device = Some(current);
            }
            Err(error) if error.is_broken_handle() => {
                println!("[{stamp}] path={path} error: {error}; reopening next tick");
            }
            Err(error) => {
                println!("[{stamp}] path={path} error: {error}");
                device = Some(current);
            }
        }
    }

    Ok(())
}

fn hex_report(report: &[u8; 20]) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
