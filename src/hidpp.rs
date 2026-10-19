use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

pub const VENDOR_ID: u16 = 0x046D;
const USAGE_PAGE: u16 = 0xFF00;
const USAGE: u16 = 0x0002;
const USAGE_SHORT: u16 = 0x0001;

const SWID: u8 = 0x0A;
const LONG_REPORT: u8 = 0x11;
const REPORT_LEN: usize = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_millis(600);
const SCAN_TIMEOUT: Duration = Duration::from_millis(250);
const RECONNECT_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const WIRED_PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const PROBE_INDICES: [u8; 7] = [1, 2, 3, 4, 5, 6, 0xFF];

const WAKE_RETRIES: u32 = 3;
const WAKE_PAUSE: Duration = Duration::from_secs(1);
const QUICK_REQUEST_TIMEOUT: Duration = Duration::from_millis(200);
const QUICK_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const QUICK_PAUSE: Duration = Duration::from_millis(300);
const QUICK_DEADLINE: Duration = Duration::from_millis(1800);
const RATE_SETTLE: Duration = Duration::from_millis(150);

const MODE_ONBOARD: u8 = 1;
const MODE_HOST: u8 = 2;

const FEATURE_FEATURE_SET: u16 = 0x0001;
const FEATURE_DEVICE_NAME: u16 = 0x0005;
const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;
const FEATURE_EXTENDED_DPI: u16 = 0x2202;
const FEATURE_EXTENDED_REPORT_RATE: u16 = 0x8061;
const FEATURE_ONBOARD_PROFILES: u16 = 0x8100;

pub const REPORT_RATE_HZ: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];
pub const LOD_VALUES: [u8; 3] = [1, 2, 3];

#[derive(Debug)]
pub enum Error {
    Hid(hidapi::HidError),
    Io(io::Error),
    NoLongReportInterface,
    MouseNotFound,
    Timeout,
    Hidpp(u8),
    HidppV1(u8),
    MissingFeature(u16),
    RefusedFeature(u16),
    InvalidReply,
    InvalidDpi(u16),
    DpiNotApplied { requested: u16, got: u16 },
    InvalidReportRate(u16),
    ReportRateNotApplied { requested: u16, got: Option<u16> },
    InvalidLod(u8),
    LodUnsupported,
    LodNotApplied { requested: u8, got: u8 },
    ModeNotConfirmed(u8),
    NoValidDpi,
    ProfileFormatUnsupported(u8, u16),
    ProfileChanged { sector: u16 },
    ProfileWriteNotVerified { sector: u16 },
    ProfileRestoreFailed { sector: u16 },
    BackupInvalid,
    NoProfileChanges,
    InvalidSleep { ps: u16, po: u16 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Hid(e) => write!(f, "HID error: {e}"),
            Error::Io(e) => write!(f, "file error: {e}"),
            Error::NoLongReportInterface => write!(
                f,
                "no HID++ long-report interface (usage page 0xFF00 / usage 0x0002) found; \
                 is the Lightspeed receiver plugged in?"
            ),
            Error::MouseNotFound => write!(
                f,
                "the receiver was found but the mouse did not answer; wake it by moving or \
                 clicking it, and close G HUB / other Logitech software"
            ),
            Error::Timeout => write!(
                f,
                "timed out waiting for the mouse; wake it by moving or clicking it, and \
                 close G HUB / other Logitech software"
            ),
            Error::Hidpp(code) => write!(f, "HID++ 2.0 error: {}", error_code(*code)),
            Error::HidppV1(code) => write!(f, "HID++ 1.0 error: {}", error_code(*code)),
            Error::MissingFeature(id) => {
                write!(f, "the mouse does not support feature 0x{id:04X}")
            }
            Error::RefusedFeature(id) => write!(
                f,
                "refused to send a request to hidden/engineering feature 0x{id:04X}"
            ),
            Error::InvalidReply => write!(f, "unexpected or malformed reply from the mouse"),
            Error::InvalidDpi(value) => write!(
                f,
                "{value} is not a valid DPI value for this mouse; use the supported DPI steps"
            ),
            Error::DpiNotApplied { requested, got } => write!(
                f,
                "the mouse did not apply {requested} DPI (it reports {got} DPI)"
            ),
            Error::InvalidReportRate(hz) => write!(
                f,
                "{hz} Hz is not a supported polling rate for this mouse; use 125, 250, 500, \
                 1000, 2000, 4000 or 8000 Hz"
            ),
            Error::ReportRateNotApplied { requested, got } => match got {
                Some(got) => write!(
                    f,
                    "the mouse did not apply {requested} Hz (it reports {got} Hz)"
                ),
                None => write!(
                    f,
                    "the mouse did not apply {requested} Hz (the read-back failed)"
                ),
            },
            Error::InvalidLod(lod) => write!(
                f,
                "LOD {lod} is not supported; use 1 (Low), 2 (Medium) or 3 (High)"
            ),
            Error::LodUnsupported => {
                write!(f, "this mouse does not report LOD support")
            }
            Error::LodNotApplied { requested, got } => write!(
                f,
                "the mouse did not apply LOD {requested} (it reports LOD {got})"
            ),
            Error::ModeNotConfirmed(mode) => {
                write!(f, "the mouse did not confirm onboard mode {mode}")
            }
            Error::NoValidDpi => write!(f, "the mouse reported no usable DPI ranges"),
            Error::ProfileFormatUnsupported(format, size) => write!(
                f,
                "unsupported onboard profile format 0x{format:02X} / sector size {size}; \
                 only format 7 with 255-byte sectors is handled"
            ),
            Error::ProfileChanged { sector } => write!(
                f,
                "sector 0x{sector:04X} no longer matches the backup; nothing was written"
            ),
            Error::ProfileWriteNotVerified { sector } => write!(
                f,
                "sector 0x{sector:04X} did not read back as written"
            ),
            Error::ProfileRestoreFailed { sector } => write!(
                f,
                "sector 0x{sector:04X} could not be restored from the backup"
            ),
            Error::BackupInvalid => write!(f, "the profile backup file is invalid or incomplete"),
            Error::NoProfileChanges => write!(f, "no profile fields were selected to save"),
            Error::InvalidSleep { ps, po } => write!(
                f,
                "invalid sleep timers (sleep {ps} s, deep sleep {po} s); both must be at least \
                 1 s and deep sleep must not be shorter than sleep"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn is_no_answer(&self) -> bool {
        matches!(
            self,
            Error::MouseNotFound | Error::Timeout | Error::NoLongReportInterface
        )
    }

    /// A hidapi read/write failure (device not connected, I/O error, invalid
    /// handle), as opposed to a HID++ error reply or a timeout. The cached
    /// handle and its feature indices are no longer usable.
    pub fn is_broken_handle(&self) -> bool {
        matches!(self, Error::Hid(_) | Error::Io(_))
    }

    pub fn is_receiver_missing(&self) -> bool {
        matches!(self, Error::NoLongReportInterface)
    }
}

impl From<hidapi::HidError> for Error {
    fn from(e: hidapi::HidError) -> Self {
        Error::Hid(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub struct Trace {
    file: File,
    seq: u64,
    label: String,
}

impl Trace {
    fn create(path: &str) -> io::Result<Trace> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Trace {
            file,
            seq: 0,
            label: String::from("unlabeled"),
        })
    }

    fn set_label(&mut self, label: &str) {
        self.label = label.to_owned();
    }

    fn record(&mut self, direction: &str, data: &[u8]) -> io::Result<()> {
        self.seq += 1;
        let hex = (0..REPORT_LEN)
            .map(|i| format!("{:02X}", data.get(i).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(
            self.file,
            "{:04} {} [{}] {}",
            self.seq, direction, self.label, hex
        )?;
        self.file.flush()
    }
}

fn error_code(code: u8) -> &'static str {
    match code {
        1 => "unknown",
        2 => "invalid argument",
        3 => "out of range",
        4 => "hardware error",
        5 => "internal error",
        6 => "invalid feature index",
        7 => "invalid function",
        8 => "busy",
        9 => "unsupported",
        _ => "unknown error",
    }
}

pub struct Device {
    dev: HidDevice,
    device_index: u8,
    product_id: u16,
    timeout: Duration,
    trace: RefCell<Option<Trace>>,
    features: RefCell<HashMap<u16, u8>>,
}

/// Read-only handle to the HID++ short-report collection (usage 0x0001, report
/// ID 0x10) of the same receiver as the long-report device. Nothing is ever
/// written to this handle.
pub struct ShortReader {
    dev: HidDevice,
}

impl ShortReader {
    pub fn open_for(device: &Device) -> Result<Option<ShortReader>, Error> {
        let product_id = device.dev.get_device_info()?.product_id();
        let api = HidApi::new()?;
        for info in api.device_list().filter(|d| {
            d.vendor_id() == VENDOR_ID
                && d.product_id() == product_id
                && d.usage_page() == USAGE_PAGE
                && d.usage() == USAGE_SHORT
        }) {
            if let Ok(dev) = api.open_path(info.path()) {
                return Ok(Some(ShortReader { dev }));
            }
        }
        Ok(None)
    }

    pub fn read(&self, timeout: Duration) -> Result<Option<Vec<u8>>, Error> {
        let mut buf = [0u8; 64];
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let count = self.dev.read_timeout(&mut buf, millis)?;
        if count == 0 {
            Ok(None)
        } else {
            Ok(Some(buf[..count].to_vec()))
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Battery {
    pub percent: u8,
    pub level_flags: u8,
    pub status: u8,
    pub external_power: u8,
}

impl Battery {
    pub fn status_text(&self) -> &'static str {
        match self.status {
            0 if self.external_power != 0 => "on external power",
            0 => "discharging",
            1 => "charging",
            2 => "charging slow",
            3 => "charge complete",
            4 => "error",
            _ => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Dpi {
    pub x: u16,
    pub default_x: u16,
    pub y: u16,
    pub default_y: u16,
    pub lod: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpiRange {
    pub from: u16,
    pub to: u16,
    pub step: u16,
}

#[derive(Debug, Clone)]
pub struct ReportRate {
    pub current_hz: u16,
    pub supported_hz: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub name: String,
    pub battery: Battery,
    pub dpi: Dpi,
    pub ranges: Vec<DpiRange>,
    pub mode: u8,
    pub rate: Option<ReportRate>,
}

#[derive(Debug, Clone, Copy)]
pub struct FeatureInfo {
    pub index: u8,
    pub id: u16,
    pub type_byte: u8,
}

#[derive(Debug, Clone)]
pub struct ProbedDevice {
    pub index: u8,
    pub name: Option<String>,
    pub battery: bool,
    pub dpi: bool,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub pid: u16,
    pub path_summary: String,
    pub devices: Vec<ProbedDevice>,
}

#[derive(Debug, Clone)]
pub struct SelectedDevice {
    pub pid: u16,
    pub path_summary: String,
    pub index: u8,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Discovery {
    pub interfaces: Vec<InterfaceInfo>,
    pub selected: Option<SelectedDevice>,
    pub reason: String,
}

struct Candidate {
    path: std::ffi::CString,
    pid: u16,
    path_summary: String,
    index: u8,
    name: Option<String>,
    battery: bool,
    dpi: bool,
    elapsed: Duration,
}

impl Device {
    pub fn open() -> Result<Device, Error> {
        Self::open_with(PROBE_TIMEOUT, REQUEST_TIMEOUT)
    }

    fn open_with(probe_timeout: Duration, request_timeout: Duration) -> Result<Device, Error> {
        let started = Instant::now();
        crate::dlog!("open: scanning interfaces");
        let api = HidApi::new()?;
        let (interfaces, candidates) = scan(&api, probe_timeout)?;
        crate::dlog!(
            "open: scan done in {:.1} ms ({} interfaces, {} candidates)",
            started.elapsed().as_secs_f64() * 1000.0,
            interfaces.len(),
            candidates.len()
        );

        if interfaces.is_empty() {
            return Err(Error::NoLongReportInterface);
        }

        let (best, _) = choose(&candidates);
        let Some(best) = best else {
            return Err(Error::MouseNotFound);
        };
        let candidate = &candidates[best];
        let dev = api.open_path(candidate.path.as_c_str())?;

        Ok(Device {
            dev,
            device_index: candidate.index,
            product_id: candidate.pid,
            timeout: request_timeout,
            trace: RefCell::new(None),
            features: RefCell::new(HashMap::new()),
        })
    }

    /// Fast reconnect path: enumerate the interfaces, try device index 1 first
    /// and stop as soon as a device named "PRO X 2" answers. The index-1 probe
    /// gets a 1 s timeout so a sleeping mouse can wake up and answer. When
    /// `full_scan` is false only index 1 is tried, which keeps the retry loop
    /// quick; every few attempts the caller asks for a full scan to cope with a
    /// device that moved to another index.
    pub fn open_reconnect(full_scan: bool) -> Result<Device, Error> {
        let started = Instant::now();
        let api = HidApi::new()?;
        let interfaces: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == VENDOR_ID && d.usage_page() == USAGE_PAGE && d.usage() == USAGE
            })
            .collect();

        if interfaces.is_empty() {
            return Err(Error::NoLongReportInterface);
        }

        let indices: &[u8] = if full_scan {
            &PROBE_INDICES
        } else {
            &PROBE_INDICES[..1]
        };
        let mut fallback = None;

        for &index in indices {
            let timeout = if index == 1 {
                RECONNECT_PROBE_TIMEOUT
            } else {
                SCAN_TIMEOUT
            };
            for info in &interfaces {
                let Ok(dev) = api.open_path(info.path()) else {
                    continue;
                };
                let Ok(reply) = request(&dev, index, 0x00, 0, &[0x00, 0x01], timeout, None) else {
                    continue;
                };
                if reply[4] == 0 {
                    continue;
                }

                let name = raw_name(&dev, index, timeout).ok();
                if name.as_deref() == Some("PRO X 2") {
                    crate::dlog!(
                        "reconnect: index {index:#04X} answered in {:.1} ms",
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok(Device {
                        dev,
                        device_index: index,
                        product_id: info.product_id(),
                        timeout: REQUEST_TIMEOUT,
                        trace: RefCell::new(None),
                        features: RefCell::new(HashMap::new()),
                    });
                }

                if full_scan
                    && fallback.is_none()
                    && raw_has_feature(&dev, index, FEATURE_UNIFIED_BATTERY, timeout)
                    && raw_has_feature(&dev, index, FEATURE_EXTENDED_DPI, timeout)
                {
                    fallback = Some((dev, index, info.product_id()));
                }
            }
        }

        if let Some((dev, index, product_id)) = fallback {
            crate::dlog!("reconnect: using first instance with battery and DPI features");
            return Ok(Device {
                dev,
                device_index: index,
                product_id,
                timeout: REQUEST_TIMEOUT,
                trace: RefCell::new(None),
                features: RefCell::new(HashMap::new()),
            });
        }

        Err(Error::MouseNotFound)
    }

    /// Opens the mouse as a direct/wired device (its own USB HID++ interface,
    /// device index 0xFF). Used only when a new long-report interface appears
    /// while the receiver path does not answer (charging cable connected).
    pub fn open_wired() -> Result<Device, Error> {
        let api = HidApi::new()?;
        let interfaces: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == VENDOR_ID && d.usage_page() == USAGE_PAGE && d.usage() == USAGE
            })
            .collect();

        if interfaces.is_empty() {
            return Err(Error::NoLongReportInterface);
        }

        for info in interfaces {
            let Ok(dev) = api.open_path(info.path()) else {
                continue;
            };
            let Ok(reply) = request(
                &dev,
                0xFF,
                0x00,
                0,
                &[0x00, 0x01],
                WIRED_PROBE_TIMEOUT,
                None,
            ) else {
                continue;
            };
            if reply[4] == 0 {
                continue;
            }
            let Ok(name) = raw_name(&dev, 0xFF, WIRED_PROBE_TIMEOUT) else {
                continue;
            };
            if name == "PRO X 2" {
                crate::dlog!("wired: found PRO X 2 on PID {:04X}", info.product_id());
                return Ok(Device {
                    dev,
                    device_index: 0xFF,
                    product_id: info.product_id(),
                    timeout: REQUEST_TIMEOUT,
                    trace: RefCell::new(None),
                    features: RefCell::new(HashMap::new()),
                });
            }
        }

        Err(Error::MouseNotFound)
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Reads one unsolicited input report (report ID included) without sending
    /// anything. `Ok(None)` means no report arrived within the timeout.
    pub fn read_input_report(&self, timeout: Duration) -> Result<Option<Vec<u8>>, Error> {
        let mut buf = [0u8; 64];
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let count = self.dev.read_timeout(&mut buf, millis)?;
        if count == 0 {
            Ok(None)
        } else {
            Ok(Some(buf[..count].to_vec()))
        }
    }

    pub fn device_index(&self) -> u8 {
        self.device_index
    }

    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    fn request(&self, feature: u8, function: u8, params: &[u8]) -> Result<[u8; 20], Error> {
        request(
            &self.dev,
            self.device_index,
            feature,
            function,
            params,
            self.timeout,
            Some(&self.trace),
        )
    }

    pub fn begin_trace(&self, path: &str) -> Result<(), Error> {
        *self.trace.borrow_mut() = Some(Trace::create(path)?);
        Ok(())
    }

    pub fn trace_label(&self, label: &str) {
        if let Some(trace) = self.trace.borrow_mut().as_mut() {
            trace.set_label(label);
        }
    }

    pub fn feature_index(&self, feature_id: u16) -> Result<u8, Error> {
        if let Some(index) = self.features.borrow().get(&feature_id).copied() {
            return Ok(index);
        }
        let reply = self.request(0x00, 0, &[(feature_id >> 8) as u8, feature_id as u8])?;
        let index = reply[4];
        if index == 0 {
            Err(Error::MissingFeature(feature_id))
        } else {
            self.features.borrow_mut().insert(feature_id, index);
            Ok(index)
        }
    }

    /// Sends one documented getter to a feature. Hidden/internal/engineering
    /// features (0x18xx, 0x1Exx) are refused before any request is sent.
    pub fn read_feature(
        &self,
        feature_id: u16,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 20], Error> {
        if (0x1800..=0x18FF).contains(&feature_id) || (0x1E00..=0x1EFF).contains(&feature_id) {
            return Err(Error::RefusedFeature(feature_id));
        }
        let index = self.feature_index(feature_id)?;
        self.request(index, function, params)
    }

    pub fn feature_list(&self) -> Result<Vec<FeatureInfo>, Error> {
        let feature_set = self.feature_index(FEATURE_FEATURE_SET)?;
        let count = self.request(feature_set, 0, &[])?[4];

        let mut features = Vec::new();
        for index in 0..=count.min(64) {
            if let Ok(reply) = self.request(feature_set, 1, &[index]) {
                features.push(FeatureInfo {
                    index,
                    id: be16(&reply, 4),
                    type_byte: reply[6],
                });
            }
        }
        Ok(features)
    }

    pub fn name(&self) -> Result<String, Error> {
        let feature = self.feature_index(FEATURE_DEVICE_NAME)?;
        let len = self.request(feature, 0, &[])?[4] as usize;
        if len == 0 || len > 128 {
            return Err(Error::InvalidReply);
        }

        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            let reply = self.request(feature, 1, &[bytes.len() as u8])?;
            bytes.extend_from_slice(&reply[4..20]);
        }
        bytes.truncate(len);

        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn battery_reply(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_UNIFIED_BATTERY)?;
        self.request(feature, 1, &[])
    }

    pub fn battery(&self) -> Result<Battery, Error> {
        let reply = self.battery_reply()?;

        Ok(Battery {
            percent: reply[4],
            level_flags: reply[5],
            status: reply[6],
            external_power: reply[7],
        })
    }

    pub fn sensor_count(&self) -> Result<u8, Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_DPI)?;
        Ok(self.request(feature, 0, &[])?[4])
    }

    pub fn sensor_capabilities(&self, sensor: u8) -> Result<(u8, u8), Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_DPI)?;
        let reply = self.request(feature, 1, &[sensor, 0, 0])?;
        Ok((reply[5], reply[6]))
    }

    pub fn dpi(&self) -> Result<Dpi, Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_DPI)?;
        let reply = self.request(feature, 5, &[0, 0, 0])?;

        Ok(Dpi {
            x: be16(&reply, 5),
            default_x: be16(&reply, 7),
            y: be16(&reply, 9),
            default_y: be16(&reply, 11),
            lod: reply[13],
        })
    }

    pub fn dpi_range_words(&self) -> Result<Vec<u16>, Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_DPI)?;
        let mut stream = Vec::new();

        for page in 0u8..16 {
            let reply = self.request(feature, 2, &[0, 0, page])?;
            stream.extend_from_slice(&reply[7..20]);

            let words = to_words(&stream);
            if words.len() > 1 && words[1..].contains(&0) {
                break;
            }
        }

        Ok(to_words(&stream))
    }

    pub fn dpi_ranges(&self) -> Result<Vec<DpiRange>, Error> {
        Ok(decode_dpi_ranges(&self.dpi_range_words()?))
    }

    pub fn is_valid_dpi(&self, dpi: u16) -> Result<bool, Error> {
        Ok(is_valid_dpi(&self.dpi_ranges()?, dpi))
    }

    pub fn report_rate_capabilities_raw(&self, connection: u8) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_REPORT_RATE)?;
        self.request(feature, 0, &[connection, 0, 0])
    }

    pub fn report_rate_current_for(&self, connection: u8) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_REPORT_RATE)?;
        self.request(feature, 2, &[connection, 0, 0])
    }

    pub fn report_rate_supported_raw(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_REPORT_RATE)?;
        self.request(feature, 1, &[0, 0, 0])
    }

    pub fn report_rate_current_raw(&self) -> Result<(u8, [u8; 20]), Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_REPORT_RATE)?;
        let mut last = Error::InvalidReply;
        let order = if self.device_index == 0xFF {
            [0u8, 1]
        } else {
            [1u8, 0]
        };

        for connection in order {
            match self.request(feature, 2, &[connection, 0, 0]) {
                Ok(reply) => {
                    if decode_report_rate_index(reply[4]).is_some() {
                        return Ok((connection, reply));
                    }
                    last = Error::InvalidReply;
                }
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    pub fn report_rate_supported(&self) -> Result<Vec<u16>, Error> {
        let supported = self.report_rate_supported_raw()?;
        let supported_hz = decode_report_rate_list(supported[5]);
        if supported_hz.is_empty() {
            return Err(Error::InvalidReply);
        }
        Ok(supported_hz)
    }

    pub fn report_rate(&self) -> Result<ReportRate, Error> {
        let supported_hz = self.report_rate_supported()?;
        let (_, current) = self.report_rate_current_raw()?;
        let current_hz = decode_report_rate_index(current[4]).ok_or(Error::InvalidReply)?;

        Ok(ReportRate {
            current_hz,
            supported_hz,
        })
    }

    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        Ok(Snapshot {
            name: self.name()?,
            battery: self.battery()?,
            dpi: self.dpi()?,
            ranges: self.dpi_ranges()?,
            mode: self.onboard_mode()?,
            rate: self.report_rate().ok(),
        })
    }

    /// One 0x2202 fn 6 write plus one read-back, for callers that already know the
    /// current mode and validated the value. Retries once after a mode switch when
    /// the cached host mode turned out to be stale.
    pub fn apply_dpi_lod_fast(
        &self,
        x: u16,
        y: u16,
        lod: u8,
        host_known: bool,
    ) -> Result<Dpi, Error> {
        if !LOD_VALUES.contains(&lod) {
            return Err(Error::InvalidLod(lod));
        }
        if !host_known {
            self.ensure_host_mode()?;
        }

        if let Err(error) = self.write_dpi(x, y, lod) {
            if host_known && matches!(error, Error::Hidpp(2 | 5)) {
                self.ensure_host_mode()?;
                self.write_dpi(x, y, lod)?;
            } else {
                return Err(error);
            }
        }

        let applied = self.dpi()?;
        if applied.x != x || applied.y != y {
            return Err(Error::DpiNotApplied {
                requested: x,
                got: applied.x,
            });
        }
        if applied.lod != lod {
            return Err(Error::LodNotApplied {
                requested: lod,
                got: applied.lod,
            });
        }
        Ok(applied)
    }

    /// One 0x8061 fn 3 write (skipped when the known current value already matches),
    /// then one read-back. Retries once after a mode switch when host_known was stale.
    pub fn apply_report_rate_fast(
        &self,
        hz: u16,
        host_known: bool,
        current_hz: Option<u16>,
    ) -> Result<u16, Error> {
        let Some(index) = REPORT_RATE_HZ
            .iter()
            .position(|&value| value == hz)
            .map(|index| index as u8)
        else {
            return Err(Error::InvalidReportRate(hz));
        };

        if !host_known {
            self.ensure_host_mode()?;
        }
        if current_hz == Some(hz) {
            return Ok(hz);
        }

        if let Err(error) = self.write_report_rate_raw(&[index, 0, 0]) {
            if host_known && matches!(error, Error::Hidpp(2 | 5)) {
                self.ensure_host_mode()?;
                self.write_report_rate_raw(&[index, 0, 0])?;
            } else {
                return Err(error);
            }
        }

        sleep(RATE_SETTLE);
        let (_, applied) = self.report_rate_current_raw()?;
        let got = decode_report_rate_index(applied[4]);
        if got != Some(hz) {
            return Err(Error::ReportRateNotApplied { requested: hz, got });
        }
        Ok(hz)
    }

    /// Documented V1 layout of 0x8061 fn 3: the first parameter byte is the rate index
    /// (0 = 125 Hz .. 6 = 8000 Hz). Used by `set_report_rate`.
    pub fn write_report_rate_raw(&self, params: &[u8]) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_REPORT_RATE)?;
        self.request(feature, 3, params)
    }

    pub fn write_dpi(&self, dpi_x: u16, dpi_y: u16, lod: u8) -> Result<(), Error> {
        let feature = self.feature_index(FEATURE_EXTENDED_DPI)?;
        self.request(
            feature,
            6,
            &[
                0,
                (dpi_x >> 8) as u8,
                dpi_x as u8,
                (dpi_y >> 8) as u8,
                dpi_y as u8,
                lod,
            ],
        )?;
        Ok(())
    }

    pub fn onboard_description_raw(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 0x00, &[])
    }

    pub fn onboard_mode_raw(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 2, &[])
    }

    pub fn onboard_mode(&self) -> Result<u8, Error> {
        Ok(self.onboard_mode_raw()?[4])
    }

    pub fn onboard_current_profile_raw(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 4, &[])
    }

    pub fn onboard_current_dpi_index_raw(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 0x0B, &[])
    }

    pub fn onboard_read_sector(&self, sector: u16, sector_size: u16) -> Result<Vec<u8>, Error> {
        if sector_size < 16 {
            return Err(Error::InvalidReply);
        }
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        let mut data = vec![0u8; usize::from(sector_size)];
        let mut offset = 0usize;
        while offset < usize::from(sector_size) {
            let read_offset = if usize::from(sector_size) - offset < 16 {
                usize::from(sector_size) - 16
            } else {
                offset
            };
            let reply = self.request(
                feature,
                5,
                &[
                    (sector >> 8) as u8,
                    sector as u8,
                    (read_offset >> 8) as u8,
                    read_offset as u8,
                ],
            )?;
            let available = usize::from(sector_size) - read_offset;
            let count = 16.min(available);
            data[read_offset..read_offset + count].copy_from_slice(&reply[4..4 + count]);
            offset += 16;
        }
        Ok(data)
    }

    /// 0x8100 fn 6 (wire): begin a memory write.
    /// Params `[sector hi, lo, sub-address hi, lo, count hi, lo]`.
    pub fn onboard_write_address(
        &self,
        sector: u16,
        sub_address: u16,
        count: u16,
    ) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(
            feature,
            6,
            &[
                (sector >> 8) as u8,
                sector as u8,
                (sub_address >> 8) as u8,
                sub_address as u8,
                (count >> 8) as u8,
                count as u8,
            ],
        )
    }

    /// 0x8100 fn 7 (wire): write one 16-byte chunk.
    pub fn onboard_write_chunk(&self, data: &[u8]) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 7, data)
    }

    /// 0x8100 fn 8 (wire): finish a memory write.
    pub fn onboard_write_end(&self) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 8, &[])
    }

    /// 0x8100 fn 3 (wire): select the current profile. Layout from libratbag:
    /// parameter 1 carries the one-based profile number (`parameters[0]` = 0).
    pub fn onboard_set_current_profile(&self, profile: u8) -> Result<[u8; 20], Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 3, &[0, profile])
    }

    pub fn write_mode(&self, mode: u8) -> Result<(), Error> {
        let feature = self.feature_index(FEATURE_ONBOARD_PROFILES)?;
        self.request(feature, 1, &[mode])?;
        Ok(())
    }

    fn ensure_host_mode(&self) -> Result<(), Error> {
        if self.onboard_mode()? != MODE_HOST {
            self.write_mode(MODE_HOST)?;
            let mode = self.onboard_mode()?;
            if mode != MODE_HOST {
                return Err(Error::ModeNotConfirmed(mode));
            }
        }
        Ok(())
    }

    fn validate_lod(&self, lod: u8) -> Result<(), Error> {
        if !LOD_VALUES.contains(&lod) {
            return Err(Error::InvalidLod(lod));
        }
        let (_, capabilities) = self.sensor_capabilities(0)?;
        if capabilities & 0x02 == 0 {
            return Err(Error::LodUnsupported);
        }
        Ok(())
    }

    fn apply_dpi_lod(&self, value: u16, lod: Option<u8>) -> Result<Dpi, Error> {
        let ranges = self.dpi_ranges()?;
        if !is_valid_dpi(&ranges, value) {
            return Err(Error::InvalidDpi(value));
        }
        if let Some(lod) = lod {
            self.validate_lod(lod)?;
        }

        self.ensure_host_mode()?;

        let current = self.dpi()?;
        let target_lod = lod.unwrap_or(current.lod);
        self.write_dpi(value, value, target_lod)?;

        let applied = self.dpi()?;
        if applied.x != value || applied.y != value {
            return Err(Error::DpiNotApplied {
                requested: value,
                got: applied.x,
            });
        }
        if applied.lod != target_lod {
            return Err(Error::LodNotApplied {
                requested: target_lod,
                got: applied.lod,
            });
        }
        Ok(applied)
    }

    fn apply_lod(&self, lod: u8) -> Result<Dpi, Error> {
        self.validate_lod(lod)?;
        self.ensure_host_mode()?;

        let current = self.dpi()?;
        self.write_dpi(current.x, current.y, lod)?;

        let applied = self.dpi()?;
        if applied.x != current.x || applied.y != current.y {
            return Err(Error::DpiNotApplied {
                requested: current.x,
                got: applied.x,
            });
        }
        if applied.lod != lod {
            return Err(Error::LodNotApplied {
                requested: lod,
                got: applied.lod,
            });
        }
        Ok(applied)
    }

    pub fn apply_onboard_mode(&self) -> Result<(), Error> {
        if self.onboard_mode()? == MODE_ONBOARD {
            return Ok(());
        }
        self.write_mode(MODE_ONBOARD)?;
        let mode = self.onboard_mode()?;
        if mode != MODE_ONBOARD {
            return Err(Error::ModeNotConfirmed(mode));
        }
        Ok(())
    }

    fn apply_report_rate(&self, hz: u16) -> Result<ReportRate, Error> {
        let Some(index) = REPORT_RATE_HZ
            .iter()
            .position(|&value| value == hz)
            .map(|index| index as u8)
        else {
            return Err(Error::InvalidReportRate(hz));
        };

        if self.onboard_mode()? != MODE_HOST {
            self.write_mode(MODE_HOST)?;
            let mode = self.onboard_mode()?;
            if mode != MODE_HOST {
                return Err(Error::ModeNotConfirmed(mode));
            }
        }

        let supported_hz = self.report_rate_supported()?;
        if !supported_hz.contains(&hz) {
            return Err(Error::InvalidReportRate(hz));
        }

        let (_, current) = self.report_rate_current_raw()?;
        let current_hz = decode_report_rate_index(current[4]).ok_or(Error::InvalidReply)?;
        if current_hz != hz {
            self.write_report_rate_raw(&[index, 0, 0])?;
            sleep(RATE_SETTLE);
            let (_, applied) = self.report_rate_current_raw()?;
            let got = decode_report_rate_index(applied[4]);
            if got != Some(hz) {
                return Err(Error::ReportRateNotApplied { requested: hz, got });
            }
        }

        Ok(ReportRate {
            current_hz: hz,
            supported_hz,
        })
    }
}

fn path_summary(path: &std::ffi::CStr) -> String {
    let full = path.to_string_lossy().into_owned();
    if full.chars().count() <= 80 {
        return full;
    }
    let tail: String = full
        .chars()
        .rev()
        .take(77)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
}

fn scan(
    api: &HidApi,
    probe_timeout: Duration,
) -> Result<(Vec<InterfaceInfo>, Vec<Candidate>), Error> {
    let scan_timeout = probe_timeout.min(SCAN_TIMEOUT);
    let mut interfaces = Vec::new();
    let mut candidates = Vec::new();

    for info in api.device_list().filter(|d| {
        d.vendor_id() == VENDOR_ID && d.usage_page() == USAGE_PAGE && d.usage() == USAGE
    }) {
        let path = info.path().to_owned();
        let pid = info.product_id();
        let summary = path_summary(info.path());
        let dev = match api.open_path(path.as_c_str()) {
            Ok(dev) => dev,
            Err(_) => continue,
        };
        let mut devices = Vec::new();

        for index in PROBE_INDICES {
            let started = Instant::now();
            let probe = request(&dev, index, 0x00, 0, &[0x00, 0x01], scan_timeout, None);
            let Ok(reply) = probe else {
                continue;
            };
            if reply[4] == 0 {
                continue;
            }

            let elapsed = started.elapsed();
            let name = raw_name(&dev, index, scan_timeout).ok();
            let battery = raw_has_feature(&dev, index, FEATURE_UNIFIED_BATTERY, scan_timeout);
            let dpi = raw_has_feature(&dev, index, FEATURE_EXTENDED_DPI, scan_timeout);

            devices.push(ProbedDevice {
                index,
                name: name.clone(),
                battery,
                dpi,
                elapsed_ms: elapsed.as_millis(),
            });
            candidates.push(Candidate {
                path: path.clone(),
                pid,
                path_summary: summary.clone(),
                index,
                name,
                battery,
                dpi,
                elapsed,
            });
        }

        interfaces.push(InterfaceInfo {
            pid,
            path_summary: summary,
            devices,
        });
    }

    Ok((interfaces, candidates))
}

fn choose(candidates: &[Candidate]) -> (Option<usize>, String) {
    if candidates.is_empty() {
        return (None, String::from("no device answered a HID++ root probe"));
    }

    let mut pool: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.name.as_deref() == Some("PRO X 2"))
        .map(|(index, _)| index)
        .collect();
    let mut reason = String::from("device name is \"PRO X 2\"");

    if pool.is_empty() {
        pool = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.battery && candidate.dpi)
            .map(|(index, _)| index)
            .collect();
        reason = String::from(
            "no device is named \"PRO X 2\"; first device exposing UnifiedBattery 0x1004 \
             and ExtendedAdjustableDPI 0x2202",
        );
    }

    if pool.is_empty() {
        return (
            None,
            String::from(
                "no device is named \"PRO X 2\" and no device exposes both UnifiedBattery \
                 0x1004 and ExtendedAdjustableDPI 0x2202",
            ),
        );
    }

    let best = *pool
        .iter()
        .min_by_key(|&&index| candidates[index].elapsed)
        .expect("pool is not empty");
    if pool.len() > 1 {
        let chosen = &candidates[best];
        reason.push_str(&format!(
            "; {} matching devices, chose the fastest reply ({} ms)",
            pool.len(),
            chosen.elapsed.as_millis()
        ));
        eprintln!(
            "note: {} matching mice found; using PID {:04X} index {:#04X} (fastest reply, {} ms)",
            pool.len(),
            chosen.pid,
            chosen.index,
            chosen.elapsed.as_millis()
        );
    }

    (Some(best), reason)
}

/// PIDs of all HID++ long-report interfaces currently present. This is plain
/// Windows enumeration: no HID++ request is sent, so it cannot wake the mouse.
pub fn long_report_interface_pids() -> Result<Vec<u16>, Error> {
    let api = HidApi::new()?;
    let mut pids = Vec::new();
    for info in api.device_list().filter(|d| {
        d.vendor_id() == VENDOR_ID && d.usage_page() == USAGE_PAGE && d.usage() == USAGE
    }) {
        let pid = info.product_id();
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

pub fn discover() -> Result<Discovery, Error> {
    let api = HidApi::new()?;
    let (interfaces, candidates) = scan(&api, PROBE_TIMEOUT)?;
    let (best, reason) = choose(&candidates);
    let selected = best.map(|index| {
        let candidate = &candidates[index];
        SelectedDevice {
            pid: candidate.pid,
            path_summary: candidate.path_summary.clone(),
            index: candidate.index,
            name: candidate.name.clone(),
        }
    });

    Ok(Discovery {
        interfaces,
        selected,
        reason,
    })
}

fn raw_feature_index(
    dev: &HidDevice,
    device_index: u8,
    feature_id: u16,
    timeout: Duration,
) -> Result<u8, Error> {
    let reply = request(
        dev,
        device_index,
        0x00,
        0,
        &[(feature_id >> 8) as u8, feature_id as u8],
        timeout,
        None,
    )?;
    let index = reply[4];
    if index == 0 {
        Err(Error::MissingFeature(feature_id))
    } else {
        Ok(index)
    }
}

fn raw_has_feature(dev: &HidDevice, device_index: u8, feature_id: u16, timeout: Duration) -> bool {
    raw_feature_index(dev, device_index, feature_id, timeout).is_ok()
}

fn raw_name(dev: &HidDevice, device_index: u8, timeout: Duration) -> Result<String, Error> {
    let feature = raw_feature_index(dev, device_index, FEATURE_DEVICE_NAME, timeout)?;
    let len = request(dev, device_index, feature, 0, &[], timeout, None)?[4] as usize;
    if len == 0 || len > 128 {
        return Err(Error::InvalidReply);
    }

    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        let reply = request(
            dev,
            device_index,
            feature,
            1,
            &[bytes.len() as u8],
            timeout,
            None,
        )?;
        bytes.extend_from_slice(&reply[4..20]);
    }
    bytes.truncate(len);

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn is_no_answer(error: &Error) -> bool {
    error.is_no_answer()
}

fn retry_wake<T>(mut operation: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut last = Error::Timeout;
    for attempt in 0..WAKE_RETRIES {
        if attempt > 0 {
            sleep(WAKE_PAUSE);
        }
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_no_answer(&error) => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

pub fn set_dpi(value: u16) -> Result<Dpi, Error> {
    retry_wake(|| Device::open()?.apply_dpi_lod(value, None))
}

pub fn set_dpi_and_lod(value: u16, lod: u8) -> Result<Dpi, Error> {
    retry_wake(|| Device::open()?.apply_dpi_lod(value, Some(lod)))
}

pub fn set_lod(lod: u8) -> Result<Dpi, Error> {
    retry_wake(|| Device::open()?.apply_lod(lod))
}

pub fn set_onboard_mode() -> Result<(), Error> {
    retry_wake(|| Device::open()?.apply_onboard_mode())
}

pub fn set_report_rate(hz: u16) -> Result<ReportRate, Error> {
    retry_wake(|| Device::open()?.apply_report_rate(hz))
}

pub fn set_onboard_mode_quick() -> Result<(), Error> {
    let deadline = Instant::now() + QUICK_DEADLINE;
    let mut last = Error::Timeout;

    for attempt in 0..WAKE_RETRIES {
        if attempt > 0 {
            if Instant::now() + QUICK_PAUSE >= deadline {
                break;
            }
            sleep(QUICK_PAUSE);
        }
        if Instant::now() >= deadline {
            break;
        }

        let result = Device::open_with(QUICK_PROBE_TIMEOUT, QUICK_REQUEST_TIMEOUT)
            .and_then(|device| device.apply_onboard_mode());
        match result {
            Ok(()) => return Ok(()),
            Err(error) if is_no_answer(&error) => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

pub fn snapshot() -> Result<Snapshot, Error> {
    Device::open()?.snapshot()
}

fn request(
    dev: &HidDevice,
    device_index: u8,
    feature: u8,
    function: u8,
    params: &[u8],
    timeout: Duration,
    trace: Option<&RefCell<Option<Trace>>>,
) -> Result<[u8; 20], Error> {
    let mut packet = [0u8; REPORT_LEN];
    packet[0] = LONG_REPORT;
    packet[1] = device_index;
    packet[2] = feature;
    packet[3] = (function << 4) | SWID;
    let count = params.len().min(REPORT_LEN - 4);
    packet[4..4 + count].copy_from_slice(&params[..count]);

    trace_report(trace, "REQ", &packet);
    let started = Instant::now();
    crate::dlog!(
        "hid req dev={device_index:#04X} feat={feature:#04X} fn={function} params={}",
        crate::debuglog::hex(&params[..count])
    );
    dev.write(&packet)?;

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 64];

    while Instant::now() < deadline {
        let n = dev.read_timeout(&mut buf, 100)?;
        if n == 0 {
            continue;
        }
        let data = &buf[..n];
        if n < 7 || !matches!(data[0], 0x10 | 0x11) {
            crate::dlog!(
                "hid ignore short/unrelated n={n} raw={}",
                crate::debuglog::hex(data)
            );
            continue;
        }
        if data[1] != device_index {
            crate::dlog!(
                "hid ignore other-device dev={:#04X} raw={}",
                data[1],
                crate::debuglog::hex(data)
            );
            continue;
        }

        if data[2] == 0xFF && data[3] == feature && data[4] == packet[3] {
            trace_report(trace, "REP", data);
            crate::dlog!(
                "hid err feat={feature:#04X} fn={function} code={} elapsed={:.1} ms",
                data[5],
                started.elapsed().as_secs_f64() * 1000.0
            );
            return Err(Error::Hidpp(data[5]));
        }
        if data[2] == 0x8F && data[3] == feature && data[4] == packet[3] {
            trace_report(trace, "REP", data);
            crate::dlog!(
                "hid err(v1) feat={feature:#04X} fn={function} code={} elapsed={:.1} ms",
                data[5],
                started.elapsed().as_secs_f64() * 1000.0
            );
            return Err(Error::HidppV1(data[5]));
        }
        if data[2] == feature && data[3] == packet[3] {
            let mut reply = [0u8; 20];
            let count = n.min(reply.len());
            reply[..count].copy_from_slice(&data[..count]);
            trace_report(trace, "REP", &reply);
            crate::dlog!(
                "hid rep feat={feature:#04X} fn={function} elapsed={:.1} ms raw={}",
                started.elapsed().as_secs_f64() * 1000.0,
                crate::debuglog::hex(data)
            );
            return Ok(reply);
        }
        crate::dlog!(
            "hid ignore (no match) feat={feature:#04X} fn={function} raw={}",
            crate::debuglog::hex(data)
        );
    }

    crate::dlog!(
        "hid timeout feat={feature:#04X} fn={function} elapsed={:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    Err(Error::Timeout)
}

fn trace_report(trace: Option<&RefCell<Option<Trace>>>, direction: &str, data: &[u8]) {
    if let Some(trace) = trace {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            let _ = trace.record(direction, data);
        }
    }
}

fn be16(data: &[u8], offset: usize) -> u16 {
    (u16::from(data[offset]) << 8) | u16::from(data[offset + 1])
}

fn to_words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| (u16::from(pair[0]) << 8) | u16::from(pair[1]))
        .collect()
}

pub fn decode_report_rate_list(mask: u8) -> Vec<u16> {
    (0..7)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| REPORT_RATE_HZ[bit])
        .collect()
}

pub fn decode_report_rate_index(index: u8) -> Option<u16> {
    REPORT_RATE_HZ.get(usize::from(index)).copied()
}

pub fn decode_dpi_ranges(words: &[u16]) -> Vec<DpiRange> {
    let mut ranges = Vec::new();
    let mut i = 0;

    while i + 2 < words.len() {
        let from = words[i];
        let marker = words[i + 1];
        let to = words[i + 2];

        if from == 0 || to == 0 || marker & 0xE000 != 0xE000 {
            break;
        }

        ranges.push(DpiRange {
            from,
            to,
            step: marker & 0x1FFF,
        });
        i += 2;
    }

    ranges
}

pub fn is_valid_dpi(ranges: &[DpiRange], dpi: u16) -> bool {
    ranges
        .iter()
        .any(|r| r.step != 0 && dpi >= r.from && dpi <= r.to && (dpi - r.from) % r.step == 0)
}

pub fn nearest_valid_dpi(ranges: &[DpiRange], dpi: u16) -> Option<u16> {
    let mut best: Option<(u32, u16)> = None;

    for range in ranges.iter().filter(|r| r.step != 0 && r.from <= r.to) {
        let from = u32::from(range.from);
        let to = u32::from(range.to);
        let step = u32::from(range.step);
        let target = u32::from(dpi).clamp(from, to);
        let steps = (target - from + step / 2) / step;
        let candidate = (from + steps * step).min(to) as u16;
        let distance = u32::from(candidate).abs_diff(u32::from(dpi));

        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, candidate));
        }
    }

    best.map(|(_, candidate)| candidate)
}

pub fn valid_dpi_values(ranges: &[DpiRange]) -> Vec<u16> {
    let mut values = Vec::new();
    for range in ranges.iter().filter(|r| r.step != 0 && r.from <= r.to) {
        let mut value = u32::from(range.from);
        while value <= u32::from(range.to) {
            let dpi = value as u16;
            if values.last() != Some(&dpi) {
                values.push(dpi);
            }
            value += u32::from(range.step);
        }
    }
    values
}
