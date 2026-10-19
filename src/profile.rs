use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::hidpp::{Device, Dpi, Error};

pub const USER_DIRECTORY_SECTOR: u16 = 0x0000;
pub const ROM_DIRECTORY_SECTOR: u16 = 0x0100;

#[derive(Debug, Clone)]
pub struct Description {
    pub memory_model_id: u8,
    pub profile_format_id: u8,
    pub macro_format_id: u8,
    pub profile_count: u8,
    pub profile_count_oob: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: u16,
    pub mechanical_layout: u8,
    pub various_info: u8,
    pub reserved: [u8; 5],
    pub raw: [u8; 20],
}

impl Description {
    pub fn parse(reply: &[u8; 20]) -> Self {
        let p = &reply[4..20];
        Self {
            memory_model_id: p[0],
            profile_format_id: p[1],
            macro_format_id: p[2],
            profile_count: p[3],
            profile_count_oob: p[4],
            button_count: p[5],
            sector_count: p[6],
            sector_size: u16::from_be_bytes([p[7], p[8]]),
            mechanical_layout: p[9],
            various_info: p[10],
            reserved: [p[11], p[12], p[13], p[14], p[15]],
            raw: *reply,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub position: usize,
    pub sector: u16,
    pub enabled: bool,
    pub reserved: u8,
}

#[derive(Debug, Clone)]
pub struct SectorData {
    pub sector: u16,
    pub data: Vec<u8>,
    pub label: String,
}

fn be16(data: &[u8], offset: usize) -> u16 {
    (u16::from(data[offset]) << 8) | u16::from(data[offset + 1])
}

fn le16(data: &[u8], offset: usize) -> u16 {
    u16::from(data[offset]) | (u16::from(data[offset + 1]) << 8)
}

pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn checksum(data: &[u8]) -> (Option<u16>, Option<u16>, bool) {
    if data.len() < 2 {
        return (None, None, false);
    }
    let stored = be16(data, data.len() - 2);
    let computed = crc16_ccitt(&data[..data.len() - 2]);
    (Some(stored), Some(computed), stored == computed)
}

fn directory_is_blank(data: &[u8]) -> bool {
    data.len() >= 4
        && (data[..4] == [0x00, 0x00, 0x00, 0x00]
            || data[..4] == [0xFF, 0xFF, 0xFF, 0xFF])
}

pub fn parse_directory(data: &[u8]) -> Vec<DirectoryEntry> {
    if data.len() < 4 || directory_is_blank(data) {
        return Vec::new();
    }
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= data.len() {
        let sector = be16(data, offset);
        if sector == 0xFFFF {
            break;
        }
        entries.push(DirectoryEntry {
            position: entries.len() + 1,
            sector,
            enabled: data[offset + 2] != 0,
            reserved: data[offset + 3],
        });
        offset += 4;
    }
    entries
}

fn read_sector_with_retry(device: &Device, sector: u16, size: u16) -> Result<Vec<u8>, Error> {
    let mut last = Error::InvalidReply;
    for attempt in 0..3 {
        match device.onboard_read_sector(sector, size) {
            Ok(data) => return Ok(data),
            Err(error) => {
                last = error;
                if attempt < 2 {
                    sleep(Duration::from_millis(200));
                }
            }
        }
    }
    Err(last)
}

fn decode_name(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.iter().all(|&byte| byte == 0xFF) {
        return String::from("(all 0xFF: no name)");
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let decoded = String::from_utf16_lossy(&units);
    decoded
        .chars()
        .take_while(|&ch| ch != '\0' && ch != '\u{FFFF}')
        .collect()
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn append_hex_dump(out: &mut String, data: &[u8]) {
    for (offset, chunk) in data.chunks(16).enumerate() {
        let ascii: String = chunk
            .iter()
            .map(|&byte| {
                if (0x20..=0x7E).contains(&byte) {
                    byte as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "  {:04X}  {:<47}  {}\n",
            offset * 16,
            hex(chunk),
            ascii
        ));
    }
}

fn append_checksum(out: &mut String, data: &[u8]) {
    let (stored, computed, matches) = checksum(data);
    match (stored, computed) {
        (Some(stored), Some(computed)) => out.push_str(&format!(
            "  checksum: stored 0x{stored:04X}, computed 0x{computed:04X} -> {}\n",
            if matches { "MATCH" } else { "MISMATCH" }
        )),
        _ => out.push_str("  checksum: not available (sector shorter than 2 bytes)\n"),
    }
}

fn append_profile_fields(
    out: &mut String,
    entry: &DirectoryEntry,
    data: &[u8],
    active: bool,
    profile_format: u8,
    button_count: u8,
    current_dpi_index: u8,
) {
    out.push_str(&format!(
        "\n--- {} (sector 0x{:04X}, directory position {}) ---\n",
        entry_label(entry, active),
        entry.sector,
        entry.position
    ));
    out.push_str(&format!(
        "  enabled: {} (directory byte 0x{:02X}); directory reserved byte 0x{:02X}\n",
        if entry.enabled { "yes" } else { "no" },
        u8::from(entry.enabled),
        entry.reserved
    ));
    append_checksum(out, data);
    if !checksum(data).2 {
        out.push_str("  warning: checksum MISMATCH; decoded fields below are unverified\n");
    }

    if data.len() < 32 {
        out.push_str("  profile fields: unknown (sector too short)\n");
        return;
    }

    if profile_format == 0x07 {
        append_format7_fields(
            out,
            data,
            button_count,
            current_dpi_index,
        );
        return;
    }

    let report_rate = data[0];
    if report_rate == 0 {
        out.push_str(&format!(
            "  report rate: raw 0x00 -> unknown (0 ms interval is invalid)\n"
        ));
    } else {
        out.push_str(&format!(
            "  report rate: raw 0x{:02X} -> {} ms -> {} Hz\n",
            report_rate,
            1000 / u16::from(report_rate),
            1000 / u16::from(report_rate)
        ));
    }

    let default_index = data[1];
    let shifted_index = data[2];
    out.push_str(&format!(
        "  default DPI index: {}; shifted DPI index: {}\n",
        default_index, shifted_index
    ));

    let mut dpi = Vec::new();
    for index in 0..5 {
        dpi.push(le16(data, 3 + index * 2));
    }
    out.push_str(&format!(
        "  DPI list (5 x u16 little-endian at 0x03..0x0C): {}\n",
        dpi.iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if let Some(value) = dpi.get(default_index as usize) {
        out.push_str(&format!(
            "  DPI at default index {}: {}\n",
            default_index, value
        ));
    } else {
        out.push_str(&format!(
            "  DPI at default index {}: unknown (index outside 0..4)\n",
            default_index
        ));
    }

    out.push_str(&format!(
        "  color (Solaar r/g/b, libratbag red/green/blue): {} / {} / {}\n",
        data[13], data[14], data[15]
    ));
    out.push_str(&format!(
        "  power mode: 0x{:02X}; angle snapping: 0x{:02X}\n",
        data[16], data[17]
    ));
    out.push_str(&format!(
        "  bytes 0x12..0x13: write_count per Solaar = 0x{:04X} (libratbag: reserved)\n",
        le16(data, 18)
    ));
    out.push_str(&format!(
        "  bytes 0x14..0x1B reserved: {}\n",
        hex(&data[20..28])
    ));
    out.push_str(&format!(
        "  powersave timeout: 0x{:04X}; poweroff timeout: 0x{:04X}\n",
        le16(data, 28),
        le16(data, 30)
    ));

    if data.len() >= 160 {
        out.push_str(&format!(
            "  buttons 0x20..0x5F (16 x 4 bytes, behaviour not decoded): {}\n",
            hex(&data[32..96])
        ));
        out.push_str(&format!(
            "  g-buttons 0x60..0x9F (16 x 4 bytes, behaviour not decoded): {}\n",
            hex(&data[96..160])
        ));
    } else {
        out.push_str("  buttons/g-buttons: unknown (sector too short)\n");
    }

    if data.len() >= 208 {
        out.push_str(&format!(
            "  name (UTF-16LE at 0xA0..0xCF): {:?}\n",
            decode_name(&data[160..208])
        ));
    } else {
        out.push_str("  name: unknown (sector too short)\n");
    }

    if data.len() >= 252 {
        out.push_str(&format!(
            "  lighting 0xD0..0xFB (4 x 11 bytes, not decoded): {}\n",
            hex(&data[208..252])
        ));
    } else {
        out.push_str("  lighting: unknown (sector too short)\n");
    }

    if data.len() >= 2 {
        out.push_str(&format!(
            "  CRC position: last 2 bytes (0x{:02X}..0x{:02X}), big-endian\n",
            data.len() - 2,
            data.len() - 1
        ));
    }
}

fn append_format7_fields(out: &mut String, data: &[u8], button_count: u8, current_dpi_index: u8) {
    if data.len() < 48 {
        out.push_str("  format 7 fields: unknown (sector shorter than 48 bytes)\n");
        return;
    }
    let report_rate = data[0];
    let report_rate_note = match report_rate {
        1 => "1 ms interval / 1000 Hz".to_string(),
        2 => "2 ms interval / 500 Hz".to_string(),
        4 => "4 ms interval / 250 Hz".to_string(),
        8 => "8 ms interval / 125 Hz".to_string(),
        3 => "raw 3: interval-ms reading would be 3 ms (not a valid rate); rate-index reading would be index 3 = 1000 Hz (matches the live rate)".to_string(),
        _ => "unknown encoding".to_string(),
    };
    out.push_str(&format!(
        "  report rate: raw 0x{report_rate:02X} -> {report_rate_note}\n"
    ));
    out.push_str(&format!(
        "  default DPI index: {}; shifted DPI index: {}; active DPI index byte: {} (live fn 0xB0 = {})\n",
        data[1], data[2], data[3], current_dpi_index
    ));

    let mut stages = Vec::new();
    for stage in 0..5 {
        let at = 4 + stage * 5;
        let x = le16(data, at);
        let y = le16(data, at + 2);
        let lod = data[at + 4];
        stages.push((x, y, lod));
    }
    for (stage, (x, y, lod)) in stages.iter().enumerate() {
        out.push_str(&format!(
            "  DPI stage {}: X {} / Y {} / LOD {} (X u16 LE at 0x{:02X}, Y at 0x{:02X}, LOD at 0x{:02X})\n",
            stage,
            x,
            y,
            lod,
            4 + stage * 5,
            6 + stage * 5,
            8 + stage * 5
        ));
    }

    let default_index = usize::from(data[1]);
    match stages.get(default_index) {
        Some((x, y, lod)) => out.push_str(&format!(
            "  DPI at default index {}: X {} / Y {} / LOD {}\n",
            default_index, x, y, lod
        )),
        None => out.push_str(&format!(
            "  DPI at default index {}: unknown (index outside 0..4); raw bytes: {}\n",
            default_index,
            hex(&data[4..29])
        )),
    }
    let active_index = usize::from(data[3]);
    match stages.get(active_index) {
        Some((x, y, lod)) => out.push_str(&format!(
            "  DPI at active index {}: X {} / Y {} / LOD {}\n",
            active_index, x, y, lod
        )),
        None => out.push_str(&format!(
            "  DPI at active index {}: unknown (index outside 0..4); raw bytes: {}\n",
            active_index,
            hex(&data[4..29])
        )),
    }

    out.push_str(&format!(
        "  reserved 0x1D..0x2B (15 bytes): {}\n",
        hex(&data[29..44])
    ));
    out.push_str(&format!(
        "  powersave timeout: 0x{:04X}; poweroff timeout: 0x{:04X}\n",
        le16(data, 44),
        le16(data, 46)
    ));

    let slots = usize::from(button_count).min(16);
    for slot in 0..slots {
        let at = 48 + slot * 4;
        out.push_str(&format!(
            "  button {}: {} (raw, behaviour not decoded)\n",
            slot + 1,
            hex(&data[at..at + 4])
        ));
    }
    out.push_str(&format!(
        "  buttons 0x30..0x6F (16 x 4 bytes): {}\n",
        hex(&data[48..112])
    ));
    out.push_str(&format!(
        "  unknown tail 0x70..0xFC ({} bytes, raw): {}\n",
        data.len() - 114,
        hex(&data[112..data.len() - 2])
    ));
    out.push_str("  name/lighting: not present in the format 7 layout (unknown tail above)\n");
}

fn entry_label(entry: &DirectoryEntry, active: bool) -> String {
    let source = if entry.sector >= ROM_DIRECTORY_SECTOR {
        "ROM"
    } else {
        "user"
    };
    if active {
        format!("{source} profile {} [ACTIVE]", entry.position)
    } else {
        format!("{source} profile {}", entry.position)
    }
}

fn timestamp_utc() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        year + i64::from(month <= 2),
        month as u32,
        day as u32,
    )
}

fn write_binary(
    path: &Path,
    description: &Description,
    mode_reply: &[u8; 20],
    current_profile_reply: &[u8; 20],
    current_dpi_reply: &[u8; 20],
    sectors: &[SectorData],
) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(b"GPXPROF1")?;
    file.write_all(&[1, 0])?;
    file.write_all(&description.sector_size.to_le_bytes())?;
    file.write_all(&[
        mode_reply[4],
        current_profile_reply[5],
        current_dpi_reply[4],
        0,
    ])?;
    file.write_all(&description.raw)?;
    file.write_all(mode_reply)?;
    file.write_all(current_profile_reply)?;
    file.write_all(current_dpi_reply)?;
    file.write_all(&(sectors.len() as u16).to_le_bytes())?;
    for sector in sectors {
        file.write_all(&sector.sector.to_le_bytes())?;
        file.write_all(&(sector.data.len() as u16).to_le_bytes())?;
        file.write_all(&sector.data)?;
    }
    Ok(())
}

pub fn dump() -> Result<(), Error> {
    let mut last_error = Error::MouseNotFound;
    let mut opened = None;
    for attempt in 0..3 {
        match Device::open_reconnect(false) {
            Ok(device) => {
                opened = Some(device);
                break;
            }
            Err(error) => {
                last_error = error;
                if attempt < 2 {
                    sleep(Duration::from_millis(500));
                }
            }
        }
    }
    let device = match opened {
        Some(device) => device,
        None => match Device::open() {
            Ok(device) => device,
            Err(_) => return Err(last_error),
        },
    };

    let description_reply = device.onboard_description_raw()?;
    let description = Description::parse(&description_reply);
    let mode_reply = device.onboard_mode_raw()?;
    let current_profile_reply = device.onboard_current_profile_raw()?;
    let current_dpi_reply = device.onboard_current_dpi_index_raw()?;

    let mode = mode_reply[4];
    let current_profile = current_profile_reply[5];
    let current_dpi_index = current_dpi_reply[4];

    if description.sector_size < 16 {
        return Err(Error::InvalidReply);
    }

    let mut sectors: Vec<SectorData> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut stopped = false;
    let mut directories: Vec<(u16, Vec<DirectoryEntry>)> = Vec::new();

    for (sector, label) in [
        (USER_DIRECTORY_SECTOR, "user profile directory"),
        (ROM_DIRECTORY_SECTOR, "ROM profile directory"),
    ] {
        if stopped {
            errors.push(format!("sector 0x{sector:04X} ({label}): not read (earlier no-answer)"));
            continue;
        }
        match read_sector_with_retry(&device, sector, description.sector_size) {
            Ok(data) => {
                let entries = parse_directory(&data);
                directories.push((sector, entries));
                sectors.push(SectorData {
                    sector,
                    data,
                    label: label.to_string(),
                });
            }
            Err(error) => {
                errors.push(format!("sector 0x{sector:04X} ({label}): {error}"));
                if error.is_no_answer() && sector == USER_DIRECTORY_SECTOR {
                    stopped = true;
                }
            }
        }
    }

    let mut referenced: Vec<(u16, usize, bool, u8, u16)> = Vec::new();
    for (directory_sector, entries) in &directories {
        for entry in entries {
            if !referenced.iter().any(|(sector, _, _, _, _)| *sector == entry.sector) {
                referenced.push((
                    entry.sector,
                    entry.position,
                    entry.enabled,
                    entry.reserved,
                    *directory_sector,
                ));
            }
        }
    }

    for (sector, position, _, _, directory_sector) in &referenced {
        let label = if *directory_sector == ROM_DIRECTORY_SECTOR {
            format!("ROM profile {position}")
        } else {
            format!("user profile {position}")
        };
        if stopped {
            errors.push(format!("sector 0x{sector:04X} ({label}): not read (earlier no-answer)"));
            continue;
        }
        match read_sector_with_retry(&device, *sector, description.sector_size) {
            Ok(data) => sectors.push(SectorData {
                sector: *sector,
                data,
                label,
            }),
            Err(error) => {
                errors.push(format!("sector 0x{sector:04X} ({label}): {error}"));
                if error.is_no_answer() {
                    stopped = true;
                }
            }
        }
    }

    let stamp = timestamp_utc();
    let mut out = String::new();
    out.push_str("GPX TOOL onboard profile dump (read-only)\n");
    out.push_str(&format!(
        "UTC timestamp: {}-{}-{} {}:{}:{}\n",
        &stamp[0..4],
        &stamp[4..6],
        &stamp[6..8],
        &stamp[9..11],
        &stamp[11..13],
        &stamp[13..15]
    ));

    out.push_str("\n== Description (0x8100 fn 0) ==\n");
    out.push_str(&format!("  raw: {}\n", hex(&description.raw)));
    out.push_str(&format!(
        "  memory model id: 0x{:02X}\n  profile format id: 0x{:02X}\n  macro format id: 0x{:02X}\n",
        description.memory_model_id, description.profile_format_id, description.macro_format_id
    ));
    out.push_str(&format!(
        "  profile count: {}; out-of-box/ROM profile count: {}; button count: {}; sector count: {}\n",
        description.profile_count,
        description.profile_count_oob,
        description.button_count,
        description.sector_count
    ));
    out.push_str(&format!(
        "  sector size: {} (0x{:04X}); mechanical layout: 0x{:02X}; various info: 0x{:02X}; reserved: {}\n",
        description.sector_size,
        description.sector_size,
        description.mechanical_layout,
        description.various_info,
        hex(&description.reserved)
    ));
    out.push_str(
        "  source note: libratbag and Solaar master document profile formats 1..5 only.\n\
         \x20 The format 7 field offsets used below come from the public Solaar PR #3345\n\
         \x20 (open, not merged) and are cross-checked against this dump: 5-byte DPI stages\n\
         \x20 at 0x04..0x1C, active DPI index at 0x03, buttons at 0x30, CRC at 0xFD..0xFE.\n\
         \x20 The ROM/factory layout is not documented for format 7; sectors 0x0100 and\n\
         \x20 0x0101 below are read as candidates and their checksums do not match.\n",
    );
    out.push_str(&format!(
        "  current mode (fn 2): {} ({}); raw: {}\n",
        mode,
        match mode {
            1 => "onboard",
            2 => "host",
            _ => "unknown",
        },
        hex(&mode_reply)
    ));
    out.push_str(&format!(
        "  current profile (fn 0x40): parameters[0] (unknown) = 0x{:02X}, parameters[1] (active profile index) = {}; raw: {}\n",
        current_profile_reply[4], current_profile, hex(&current_profile_reply)
    ));
    out.push_str(&format!(
        "  current DPI index (fn 0xB0): {}; raw: {}\n",
        current_dpi_index,
        hex(&current_dpi_reply)
    ));

    out.push_str("\n== Profile directories ==\n");
    for sector in &sectors {
        if sector.sector != USER_DIRECTORY_SECTOR && sector.sector != ROM_DIRECTORY_SECTOR {
            continue;
        }
        out.push_str(&format!(
            "\n--- {} (sector 0x{:04X}) ---\n",
            sector.label, sector.sector
        ));
        append_checksum(&mut out, &sector.data);
        if !checksum(&sector.data).2 {
            out.push_str("  warning: checksum MISMATCH; directory contents below are unverified\n");
        }
        if directory_is_blank(&sector.data) {
            out.push_str("  directory is blank (first 4 bytes are 00 00 00 00 or FF FF FF FF)\n");
        }
        let entries = parse_directory(&sector.data);
        if entries.is_empty() {
            out.push_str("  entries: none\n");
        } else {
            for entry in entries {
                out.push_str(&format!(
                    "  entry {}: sector 0x{:04X}, enabled {}, reserved 0x{:02X}\n",
                    entry.position,
                    entry.sector,
                    u8::from(entry.enabled),
                    entry.reserved
                ));
            }
        }
        out.push_str("  raw sector hex dump:\n");
        append_hex_dump(&mut out, &sector.data);
    }

    out.push_str("\n== Profile sectors ==\n");
    for sector in &sectors {
        if sector.sector == USER_DIRECTORY_SECTOR || sector.sector == ROM_DIRECTORY_SECTOR {
            continue;
        }
        let entry = referenced
            .iter()
            .find(|(candidate, _, _, _, _)| *candidate == sector.sector);
        let (position, enabled, reserved, directory_sector) = match entry {
            Some((_, position, enabled, reserved, directory_sector)) => {
                (*position, *enabled, *reserved, *directory_sector)
            }
            None => (0, false, 0, USER_DIRECTORY_SECTOR),
        };
        let entry = DirectoryEntry {
            position,
            sector: sector.sector,
            enabled,
            reserved,
        };
        let active = directory_sector == USER_DIRECTORY_SECTOR
            && position == current_profile as usize;
        append_profile_fields(
            &mut out,
            &entry,
            &sector.data,
            active,
            description.profile_format_id,
            description.button_count,
            current_dpi_index,
        );
        out.push_str("  raw sector hex dump:\n");
        append_hex_dump(&mut out, &sector.data);
    }

    if !errors.is_empty() {
        out.push_str("\n== Missing / failed reads ==\n");
        for error in &errors {
            out.push_str(&format!("  {error}\n"));
        }
    }

    out.push_str("\n== Summary ==\n");
    for sector in &sectors {
        let (stored, computed, matches) = checksum(&sector.data);
        let text = match (stored, computed) {
            (Some(stored), Some(computed)) => format!(
                "sector 0x{:04X} ({}): stored 0x{stored:04X} computed 0x{computed:04X} {}",
                sector.sector,
                sector.label,
                if matches { "MATCH" } else { "MISMATCH" }
            ),
            _ => format!(
                "sector 0x{:04X} ({}): checksum unavailable",
                sector.sector, sector.label
            ),
        };
        out.push_str(&format!("  {text}\n"));
    }
    out.push_str(
        "  note: ROM/factory sectors may store 0x0000 or 0xFFFF instead of a CRC\n\
         \x20 (documented by Omalogi for G502 X factory sectors), so a ROM MISMATCH is\n\
         \x20 not by itself a read error.\n",
    );

    let stamp = timestamp_utc();
    let backup_dir = PathBuf::from("backups");
    fs::create_dir_all(&backup_dir)?;
    let stem = format!("profiles-{stamp}");
    let bin_path = backup_dir.join(format!("{stem}.bin"));
    let txt_path = backup_dir.join(format!("{stem}.txt"));

    write_binary(
        &bin_path,
        &description,
        &mode_reply,
        &current_profile_reply,
        &current_dpi_reply,
        &sectors,
    )?;
    fs::write(&txt_path, &out)?;

    print!("{out}");
    println!("\nBackup written:");
    println!("  {}", bin_path.display());
    println!("  {}", txt_path.display());
    if !errors.is_empty() {
        println!("Some sectors could not be read; see the missing-read list above.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Session 16: controlled onboard-profile writes (format 7, 255-byte sectors)
// ---------------------------------------------------------------------------

const WRITE_LOG_PATH: &str = "profile_write_log.txt";
const SECTOR_SIZE: usize = 255;
const CRC_OFFSET: usize = SECTOR_SIZE - 2;
const USER_PROFILE_1: u16 = 0x0001;

fn write_log(line: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(WRITE_LOG_PATH)
    {
        let _ = writeln!(file, "{line}");
    }
}

pub fn sector_crc_matches(data: &[u8]) -> bool {
    data.len() >= 3 && crc16_ccitt(&data[..data.len() - 2]) == be16(data, data.len() - 2)
}

fn add_crc(image: &[u8]) -> Vec<u8> {
    let mut out = image.to_vec();
    let length = out.len();
    let crc = crc16_ccitt(&out[..length - 2]);
    out[length - 2..].copy_from_slice(&crc.to_be_bytes());
    out
}

fn check_profile_format(device: &Device) -> Result<u16, Error> {
    let reply = device.onboard_description_raw()?;
    let description = Description::parse(&reply);
    if description.profile_format_id != 0x07 || description.sector_size != SECTOR_SIZE as u16 {
        return Err(Error::ProfileFormatUnsupported(
            description.profile_format_id,
            description.sector_size,
        ));
    }
    Ok(description.sector_size)
}

fn ensure_onboard(device: &Device) -> Result<(), Error> {
    let mode = device.onboard_mode()?;
    if mode != 1 {
        write_log(&format!("set mode 1 (onboard); previous mode {mode}"));
        device.write_mode(1)?;
        let confirmed = device.onboard_mode()?;
        if confirmed != 1 {
            return Err(Error::ModeNotConfirmed(confirmed));
        }
    }
    Ok(())
}

fn read_and_check_crc(device: &Device, sector: u16, size: u16) -> Result<Vec<u8>, Error> {
    let mut last = Error::InvalidReply;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_millis(200));
        }
        match read_sector_with_retry(device, sector, size) {
            Ok(data) => {
                if sector_crc_matches(&data) {
                    return Ok(data);
                }
                last = Error::ProfileWriteNotVerified { sector };
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn verify_bytes(device: &Device, sector: u16, expected: &[u8]) -> Result<Vec<u8>, Error> {
    let mut last = Error::ProfileWriteNotVerified { sector };
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_millis(200));
        }
        match read_sector_with_retry(device, sector, expected.len() as u16) {
            Ok(data) => {
                if data.len() == expected.len() && sector_crc_matches(&data) && data == expected {
                    return Ok(data);
                }
                last = Error::ProfileWriteNotVerified { sector };
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn write_sector_raw(device: &Device, sector: u16, image: &[u8]) -> Result<(), Error> {
    let count = image.len() as u16;
    let reply = device.onboard_write_address(sector, 0, count)?;
    write_log(&format!(
        "write address sector=0x{sector:04X} offset=0 length={count} reply={}",
        hex(&reply)
    ));

    let mut offset = 0usize;
    while offset < image.len() {
        let mut chunk = [0u8; 16];
        let end = (offset + 16).min(image.len());
        chunk[..end - offset].copy_from_slice(&image[offset..end]);
        let reply = device.onboard_write_chunk(&chunk)?;
        write_log(&format!(
            "write chunk sector=0x{sector:04X} offset={offset} length=16 reply={}",
            hex(&reply)
        ));
        offset += 16;
    }

    let reply = device.onboard_write_end()?;
    write_log(&format!(
        "write end sector=0x{sector:04X} offset={count} length=0 reply={}",
        hex(&reply)
    ));
    sleep(Duration::from_millis(100));
    Ok(())
}

fn restore_sector(device: &Device, sector: u16, backup: &[u8]) -> Result<(), Error> {
    if backup.len() != SECTOR_SIZE || !sector_crc_matches(backup) {
        return Err(Error::BackupInvalid);
    }
    ensure_onboard(device)?;
    let mut last = Error::ProfileRestoreFailed { sector };
    for attempt in 0..2 {
        if attempt > 0 {
            sleep(Duration::from_millis(300));
        }
        match write_sector_raw(device, sector, backup)
            .and_then(|_| verify_bytes(device, sector, backup))
        {
            Ok(_) => {
                write_log(&format!(
                    "restore sector=0x{sector:04X} verified byte-exact crc=0x{:04X}",
                    be16(backup, CRC_OFFSET)
                ));
                return Ok(());
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Writes one full sector (CRC recomputed), then reads it back and requires a
/// byte-exact match. `expected_before` is the fresh/backup image the sector must
/// still contain; on any mismatch the original is written back immediately.
fn write_profile_sector(
    device: &Device,
    sector: u16,
    image: &[u8],
    expected_before: &[u8],
) -> Result<Vec<u8>, Error> {
    let size = check_profile_format(device)? as usize;
    if image.len() != size || expected_before.len() != size {
        return Err(Error::InvalidReply);
    }
    if !sector_crc_matches(expected_before) {
        return Err(Error::BackupInvalid);
    }

    let fresh = read_and_check_crc(device, sector, size as u16)?;
    if fresh != expected_before {
        return Err(Error::ProfileChanged { sector });
    }
    ensure_onboard(device)?;

    let intended = add_crc(image);
    write_sector_raw(device, sector, &intended)?;
    match verify_bytes(device, sector, &intended) {
        Ok(read_back) => {
            write_log(&format!(
                "verify sector=0x{sector:04X} OK crc=0x{:04X}",
                be16(&read_back, CRC_OFFSET)
            ));
            Ok(read_back)
        }
        Err(error) => {
            write_log(&format!(
                "verify sector=0x{sector:04X} FAILED: {error}; restoring backup"
            ));
            restore_sector(device, sector, expected_before)?;
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadStep {
    None,
    ModeCycle,
    SetCurrentProfile,
    Failed,
}

impl ReloadStep {
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadStep::None => "none",
            ReloadStep::ModeCycle => "host -> onboard",
            ReloadStep::SetCurrentProfile => "set current profile 1",
            ReloadStep::Failed => "none worked",
        }
    }
}

fn dpi_matches(dpi: &Dpi, x: u16, y: u16, lod: u8) -> bool {
    dpi.x == x && dpi.y == y && dpi.lod == lod
}

/// Tries, in order: use the live value, a host->onboard cycle, then selecting
/// profile 1. Assumes the mouse is in onboard mode.
fn reload_live(
    device: &Device,
    x: u16,
    y: u16,
    lod: u8,
) -> Result<(ReloadStep, Dpi), Error> {
    ensure_onboard(device)?;

    let first = device.dpi()?;
    if dpi_matches(&first, x, y, lod) {
        return Ok((ReloadStep::None, first));
    }

    device.write_mode(2)?;
    let mode = device.onboard_mode()?;
    if mode != 2 {
        return Err(Error::ModeNotConfirmed(mode));
    }
    device.write_mode(1)?;
    let mode = device.onboard_mode()?;
    if mode != 1 {
        return Err(Error::ModeNotConfirmed(mode));
    }
    sleep(Duration::from_millis(100));

    let cycled = device.dpi()?;
    if dpi_matches(&cycled, x, y, lod) {
        return Ok((ReloadStep::ModeCycle, cycled));
    }

    let reply = device.onboard_set_current_profile(1)?;
    write_log(&format!(
        "set current profile 1 reply={}",
        hex(&reply)
    ));
    let current = device.onboard_current_profile_raw()?;
    if current[5] != 1 {
        return Err(Error::InvalidReply);
    }
    sleep(Duration::from_millis(100));

    let selected = device.dpi()?;
    if dpi_matches(&selected, x, y, lod) {
        return Ok((ReloadStep::SetCurrentProfile, selected));
    }
    Ok((ReloadStep::Failed, selected))
}

fn format_dpi(dpi: &Dpi) -> String {
    format!("X {} / Y {} / LOD {}", dpi.x, dpi.y, dpi.lod)
}

fn raw_sector_backup_path(stamp: &str) -> PathBuf {
    PathBuf::from("backups").join(format!("profile-{stamp}-sector0001.bin"))
}

const MAX_SECTOR_BACKUPS: usize = 20;

/// Keeps at most the 20 newest `profile-*-sector0001.bin` files.
fn prune_sector_backups() {
    let Ok(entries) = fs::read_dir("backups") else {
        return;
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("profile-") && name.ends_with("-sector0001.bin") {
                Some((name, entry.path()))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.len() > MAX_SECTOR_BACKUPS {
        for (_, path) in files.iter().take(files.len() - MAX_SECTOR_BACKUPS) {
            let _ = fs::remove_file(path);
        }
    }
}

fn write_sector_backup(image: &[u8], label: &str) -> Result<PathBuf, Error> {
    fs::create_dir_all("backups")?;
    let stamp = timestamp_utc();
    let path = raw_sector_backup_path(&stamp);
    fs::write(&path, image)?;
    prune_sector_backups();
    write_log(&format!(
        "backup {label} sector=0x0001 path={} crc=0x{:04X}",
        path.display(),
        be16(image, CRC_OFFSET)
    ));
    Ok(path)
}

/// One quick test: writes byte 0x00 = 0x04 into sector 0x0001 and reads the live
/// rate back, then restores the sector byte-exact. Only byte 0x00 and the CRC
/// differ during the test.
fn open_for_write() -> Result<Device, Error> {
    let mut last = Error::MouseNotFound;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_secs(1));
        }
        match Device::open_reconnect(attempt == 1) {
            Ok(device) => return Ok(device),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Restores user sector 0x0001 byte-for-byte from a raw 255-byte backup.
pub fn restore_profile_1(path: &Path, yes: bool) -> Result<(), Error> {
    let image = fs::read(path)?;
    if image.len() != SECTOR_SIZE || !sector_crc_matches(&image) {
        return Err(Error::BackupInvalid);
    }
    let device = open_for_write()?;
    check_profile_format(&device)?;
    let fresh = read_and_check_crc(&device, USER_PROFILE_1, SECTOR_SIZE as u16)?;
    println!(
        "Current sector 0x0001 CRC 0x{:04X}; backup {} CRC 0x{:04X}",
        be16(&fresh, CRC_OFFSET),
        path.display(),
        be16(&image, CRC_OFFSET)
    );
    if !yes {
        println!("Dry run (no writes). Pass --yes to restore sector 0x0001 byte-exact.");
        return Ok(());
    }
    if fresh == image {
        println!("Sector 0x0001 already matches the backup; nothing to write.");
    } else {
        restore_sector(&device, USER_PROFILE_1, &image)?;
        println!(
            "Restored sector 0x0001 byte-exact from {} (CRC 0x{:04X})",
            path.display(),
            be16(&image, CRC_OFFSET)
        );
    }
    device.apply_onboard_mode()?;
    let dpi = device.dpi()?;
    let (connection, rate) = device.report_rate_current_raw()?;
    let hz = crate::hidpp::decode_report_rate_index(rate[4]);
    println!(
        "Live: mode {}, {}, rate {:?} Hz (conn {connection})",
        device.onboard_mode()?,
        format_dpi(&dpi),
        hz
    );
    Ok(())
}

pub const PS_TIMEOUT_OFFSET: usize = 0x2C;
pub const PO_TIMEOUT_OFFSET: usize = 0x2E;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepTimers {
    pub ps_timeout: u16,
    pub po_timeout: u16,
}

impl SleepTimers {
    fn from_sector(data: &[u8]) -> Self {
        Self {
            ps_timeout: le16(data, PS_TIMEOUT_OFFSET),
            po_timeout: le16(data, PO_TIMEOUT_OFFSET),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveFields {
    pub dpi: Option<u16>,
    pub lod: Option<u8>,
    pub rate: Option<u16>,
    pub ps_timeout: Option<u16>,
    pub po_timeout: Option<u16>,
}

impl SaveFields {
    pub fn is_empty(&self) -> bool {
        self.dpi.is_none()
            && self.lod.is_none()
            && self.rate.is_none()
            && self.ps_timeout.is_none()
            && self.po_timeout.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedProfile {
    pub dpi: u16,
    pub lod: u8,
    pub rate: Option<u16>,
    pub sleep: SleepTimers,
    pub changed: bool,
}

pub fn read_sleep_timers() -> Result<SleepTimers, Error> {
    let device = open_for_write()?;
    read_sleep_timers_device(&device)
}

pub fn read_sleep_timers_device(device: &Device) -> Result<SleepTimers, Error> {
    let size = check_profile_format(device)?;
    let fresh = read_and_check_crc(device, USER_PROFILE_1, size)?;
    Ok(SleepTimers::from_sector(&fresh))
}

pub fn save_dpi(dpi: u16, lod: Option<u8>, yes: bool) -> Result<(), Error> {
    save_to_mouse(
        SaveFields {
            dpi: Some(dpi),
            lod,
            ..SaveFields::default()
        },
        yes,
    )
    .map(|_| ())
}

pub fn save_to_mouse(fields: SaveFields, yes: bool) -> Result<SavedProfile, Error> {
    let device = open_for_write()?;
    save_to_mouse_device(&device, fields, yes)
}

/// Changes only the fields that are present in `fields`: stage 0 X/Y and LOD,
/// the report-rate byte (0x00 = 0x8061 rate index, verified in Session 17) and
/// the power-save/power-off (sleep/deep-sleep) timeouts at 0x2C/0x2E of user
/// sector 0x0001. Fresh read, timestamped backup, byte-exact read-back and
/// automatic restore on any problem. Leaves the mouse in onboard mode.
pub fn save_to_mouse_device(
    device: &Device,
    fields: SaveFields,
    yes: bool,
) -> Result<SavedProfile, Error> {
    if fields.is_empty() {
        return Err(Error::NoProfileChanges);
    }
    if let Some(lod) = fields.lod {
        if !crate::hidpp::LOD_VALUES.contains(&lod) {
            return Err(Error::InvalidLod(lod));
        }
    }
    if let Some(dpi) = fields.dpi {
        let ranges = device.dpi_ranges()?;
        if !crate::hidpp::is_valid_dpi(&ranges, dpi) {
            return Err(Error::InvalidDpi(dpi));
        }
    }
    if matches!(fields.ps_timeout, Some(0)) || matches!(fields.po_timeout, Some(0)) {
        return Err(Error::InvalidSleep {
            ps: fields.ps_timeout.unwrap_or(0),
            po: fields.po_timeout.unwrap_or(0),
        });
    }
    let rate_index = match fields.rate {
        Some(hz) => {
            let supported = device.report_rate_supported()?;
            if !supported.contains(&hz) {
                return Err(Error::InvalidReportRate(hz));
            }
            Some(
                crate::hidpp::REPORT_RATE_HZ
                    .iter()
                    .position(|&value| value == hz)
                    .ok_or(Error::InvalidReportRate(hz))? as u8,
            )
        }
        None => None,
    };
    let size = check_profile_format(device)?;
    if device.onboard_mode()? != 1 {
        ensure_onboard(device)?;
    }
    let current = device.onboard_current_profile_raw()?[5];
    if current != 1 {
        return Err(Error::ProfileChanged { sector: USER_PROFILE_1 });
    }

    let fresh = read_and_check_crc(device, USER_PROFILE_1, size)?;
    let current_sleep = SleepTimers::from_sector(&fresh);
    let target_sleep = SleepTimers {
        ps_timeout: fields.ps_timeout.unwrap_or(current_sleep.ps_timeout),
        po_timeout: fields.po_timeout.unwrap_or(current_sleep.po_timeout),
    };
    if (fields.ps_timeout.is_some() || fields.po_timeout.is_some())
        && (target_sleep.ps_timeout == 0
            || target_sleep.po_timeout == 0
            || target_sleep.po_timeout < target_sleep.ps_timeout)
    {
        return Err(Error::InvalidSleep {
            ps: target_sleep.ps_timeout,
            po: target_sleep.po_timeout,
        });
    }

    let mut image = fresh.clone();
    if let Some(dpi) = fields.dpi {
        image[4..6].copy_from_slice(&dpi.to_le_bytes());
        image[6..8].copy_from_slice(&dpi.to_le_bytes());
    }
    if let Some(lod) = fields.lod {
        image[8] = lod;
    }
    if let Some(index) = rate_index {
        image[0] = index;
    }
    image[PS_TIMEOUT_OFFSET..PS_TIMEOUT_OFFSET + 2]
        .copy_from_slice(&target_sleep.ps_timeout.to_le_bytes());
    image[PO_TIMEOUT_OFFSET..PO_TIMEOUT_OFFSET + 2]
        .copy_from_slice(&target_sleep.po_timeout.to_le_bytes());

    let target_dpi = le16(&image, 4);
    let target_lod = image[8];
    let changed = image[..CRC_OFFSET] != fresh[..CRC_OFFSET];

    let saved = SavedProfile {
        dpi: target_dpi,
        lod: target_lod,
        rate: fields.rate,
        sleep: target_sleep,
        changed,
    };

    if !yes {
        println!(
            "Dry run (no writes). Pass --yes to save these values to the mouse profile."
        );
        println!(
            "Sector 0x0001: X {} / Y {} / LOD {} -> X {} / Y {} / LOD {}; rate byte 0x{:02X} -> \
             0x{:02X}; sleep {} s -> {} s; deep sleep {} s -> {} s",
            le16(&fresh, 4),
            le16(&fresh, 6),
            fresh[8],
            target_dpi,
            le16(&image, 6),
            target_lod,
            fresh[0],
            image[0],
            current_sleep.ps_timeout,
            target_sleep.ps_timeout,
            current_sleep.po_timeout,
            target_sleep.po_timeout
        );
        println!("New CRC would be 0x{:04X}.", crc16_ccitt(&image[..CRC_OFFSET]));
        return Ok(saved);
    }

    if !changed {
        println!("Profile already stores the requested values; nothing to write.");
        return Ok(saved);
    }

    let backup_path = write_sector_backup(&fresh, "save-to-mouse")?;
    println!(
        "Backup: {} (255 raw sector bytes, CRC 0x{:04X})",
        backup_path.display(),
        be16(&fresh, CRC_OFFSET)
    );

    let read_back = match write_profile_sector(device, USER_PROFILE_1, &image, &fresh) {
        Ok(read_back) => read_back,
        Err(error) => {
            println!("Write FAILED: {error}; sector 0x0001 restored from the fresh backup.");
            return Err(error);
        }
    };
    println!(
        "Write: sector 0x0001 -> X {} / Y {} / LOD {} / rate byte 0x{:02X} / sleep {} s / \
         deep sleep {} s (CRC 0x{:04X}); read-back byte-exact",
        le16(&read_back, 4),
        le16(&read_back, 6),
        read_back[8],
        read_back[0],
        le16(&read_back, PS_TIMEOUT_OFFSET),
        le16(&read_back, PO_TIMEOUT_OFFSET),
        be16(&read_back, CRC_OFFSET)
    );

    if fields.dpi.is_some() || fields.lod.is_some() {
        let (step, applied) = match reload_live(
            device,
            le16(&image, 4),
            le16(&image, 6),
            target_lod,
        ) {
            Ok(result) => result,
            Err(error) => {
                println!("Live reload FAILED: {error}; restoring sector 0x0001.");
                restore_sector(device, USER_PROFILE_1, &fresh)?;
                return Err(error);
            }
        };
        println!(
            "Live DPI: {} (reload step: {})",
            format_dpi(&applied),
            step.as_str()
        );
        if step == ReloadStep::Failed {
            println!("The mouse did not use the new profile values; restoring sector 0x0001.");
            restore_sector(device, USER_PROFILE_1, &fresh)?;
            let (_, restored) = reload_live(
                device,
                le16(&fresh, 4),
                le16(&fresh, 6),
                fresh[8],
            )?;
            println!("Restored live DPI: {}", format_dpi(&restored));
            return Err(Error::ProfileWriteNotVerified {
                sector: USER_PROFILE_1,
            });
        }
    }

    if let Some(hz) = fields.rate {
        sleep(Duration::from_millis(150));
        let (_, current_rate) = device.report_rate_current_raw()?;
        let got = crate::hidpp::decode_report_rate_index(current_rate[4]);
        println!("Live rate: {:?} Hz (wanted {hz} Hz)", got);
        if got != Some(hz) {
            println!("The mouse did not use the saved rate; restoring sector 0x0001.");
            restore_sector(device, USER_PROFILE_1, &fresh)?;
            return Err(Error::ReportRateNotApplied {
                requested: hz,
                got,
            });
        }
    }

    device.apply_onboard_mode()?;
    println!(
        "Saved sector 0x0001: {} DPI / LOD {} / rate {} / sleep {} s / deep sleep {} s.",
        target_dpi,
        target_lod,
        fields
            .rate
            .map(|hz| format!("{hz} Hz"))
            .unwrap_or_else(|| String::from("unchanged")),
        target_sleep.ps_timeout,
        target_sleep.po_timeout
    );
    Ok(saved)
}
