use std::env;
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_ACCENT: [u8; 3] = [0x3D, 0xBE, 0x6B];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub custom_dpi: Option<u16>,
    pub custom_rate: Option<u16>,
    pub custom_lod: Option<u8>,
    pub theme: ThemeMode,
    pub accent: [u8; 3],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            custom_dpi: None,
            custom_rate: None,
            custom_lod: None,
            theme: ThemeMode::System,
            accent: DEFAULT_ACCENT,
        }
    }
}

impl Settings {
    pub fn has_custom(&self) -> bool {
        self.custom_dpi.is_some() || self.custom_rate.is_some() || self.custom_lod.is_some()
    }
}

pub fn parse_hex_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

pub fn format_hex_color(color: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}

pub fn path() -> PathBuf {
    match env::var_os("APPDATA") {
        Some(dir) => PathBuf::from(dir)
            .join("Featherweight")
            .join("settings.txt"),
        None => PathBuf::from("settings.txt"),
    }
}

/// Copies the pre-rename settings file to the new folder once, without
/// deleting the old one. Runs before the first read.
pub fn migrate() {
    let Some(appdata) = env::var_os("APPDATA") else {
        return;
    };
    let old = PathBuf::from(appdata).join("GPX Tool").join("settings.txt");
    let new = path();
    if new.exists() || !old.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            crate::debuglog::record_always(&format!(
                "settings migration failed: could not create {}: {error}",
                parent.display()
            ));
            return;
        }
    }
    match fs::copy(&old, &new) {
        Ok(bytes) => crate::debuglog::record_always(&format!(
            "settings migrated: copied {} -> {} ({bytes} bytes)",
            old.display(),
            new.display()
        )),
        Err(error) => crate::debuglog::record_always(&format!(
            "settings migration failed: could not copy {} -> {}: {error}",
            old.display(),
            new.display()
        )),
    }
}

pub fn load() -> Settings {
    migrate();
    let mut settings = Settings::default();
    let Ok(text) = fs::read_to_string(path()) else {
        return settings;
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "custom_dpi" => {
                if let Ok(value) = value.trim().parse::<u16>() {
                    settings.custom_dpi = Some(value);
                }
            }
            "custom_rate" => {
                if let Ok(value) = value.trim().parse::<u16>() {
                    settings.custom_rate = Some(value);
                }
            }
            "custom_lod" => {
                if let Ok(value) = value.trim().parse::<u8>() {
                    if (1..=3).contains(&value) {
                        settings.custom_lod = Some(value);
                    }
                }
            }
            "theme" => {
                if let Some(value) = ThemeMode::parse(value.trim()) {
                    settings.theme = value;
                }
            }
            "accent" => {
                if let Some(value) = parse_hex_color(value.trim()) {
                    settings.accent = value;
                }
            }
            _ => {}
        }
    }

    settings
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    let mut text = String::new();
    if let Some(value) = settings.custom_dpi {
        text.push_str(&format!("custom_dpi={value}\n"));
    }
    if let Some(value) = settings.custom_rate {
        text.push_str(&format!("custom_rate={value}\n"));
    }
    if let Some(value) = settings.custom_lod {
        text.push_str(&format!("custom_lod={value}\n"));
    }
    text.push_str(&format!("theme={}\n", settings.theme.as_str()));
    text.push_str(&format!(
        "accent={}\n",
        format_hex_color(settings.accent)
    ));
    fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}
