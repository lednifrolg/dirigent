//! What dirigent prints. Colors use the terminal's ANSI palette, so they follow the terminal
//! theme, and disappear when the output is not a terminal or `NO_COLOR` is set. A light's own
//! color is shown as it is, in true color.

use std::error::Error as _;
use std::fmt::Display;
use std::io::Write;

use anstream::{eprintln, println};
use anstyle::{AnsiColor, RgbColor, Style};
use dirigera::{ColorMode, Device, DeviceKind, Home, Room};
use jiff::Timestamp;
use serde::Serialize;

use crate::error::AppError;

pub const DIM: Style = Style::new().dimmed();
pub const BOLD: Style = Style::new().bold();
pub const GREEN: Style = AnsiColor::Green.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const RED: Style = AnsiColor::Red.on_default();
const BLUE: Style = AnsiColor::Blue.on_default();
/// Remotes below this battery level get a warning.
pub const LOW_BATTERY: u8 = 10;

pub fn paint(style: Style, text: impl Display) -> String {
    format!("{style}{text}{style:#}")
}

/// The error with the causes that add something, in one line. A connection failure is named by
/// its cause alone, without the request's URL: "could not reach the hub: connection refused".
pub fn message(error: &AppError) -> String {
    if let AppError::Api(dirigera::Error::Transport(e)) = error {
        let mut cause: &dyn std::error::Error = e;
        while let Some(inner) = cause.source() {
            cause = inner;
        }
        let text = cause.to_string();
        let text = text.split(" (os error").next().unwrap_or_default();
        return format!("could not reach the hub: {}", lower_first(text));
    }
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(inner) = cause {
        let text = inner.to_string();
        if !message.contains(&text) {
            message = format!("{message}: {text}");
        }
        cause = inner.source();
    }
    message
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub fn error(error: &AppError) {
    eprintln!("{} {}", paint(RED.bold(), "error:"), message(error));
}

pub fn note(text: impl Display) {
    eprintln!("{}", paint(DIM, text));
}

pub fn warn(text: impl Display) {
    eprintln!("{} {text}", paint(YELLOW.bold(), "warning:"));
}

pub fn success(text: impl Display) {
    println!("{} {text}", paint(GREEN.bold(), "✓"));
}

pub fn line(text: impl Display) {
    println!("{text}");
}

/// Prints `value` as JSON. A reader that went away, like `head`, is an error rather than a
/// panic.
pub fn json(value: &impl Serialize) -> Result<(), AppError> {
    let text = serde_json::to_string_pretty(value).expect("dirigent's views always serialize");
    writeln!(std::io::stdout().lock(), "{text}").map_err(|e| AppError::io("standard output", e))
}

/// The color a light shows, in true color: its hue and saturation, or its white.
pub fn light_color(device: &Device) -> Option<RgbColor> {
    let a = &device.attributes;
    match a.color_mode() {
        Some(ColorMode::Color) => {
            let hue = a.color_hue()?;
            let saturation = a.color_saturation()?;
            Some(hsv(hue, saturation))
        }
        _ => a.color_temperature().map(kelvin),
    }
}

/// An RGB color for a hue and saturation at full value.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0-255 first"
)]
fn hsv(hue: f64, saturation: f64) -> RgbColor {
    let sector = hue.rem_euclid(360.0) / 60.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let rising = 1.0 - (sector.rem_euclid(2.0) - 1.0).abs();
    let (red, green, blue) = match sector {
        sector if sector < 1.0 => (1.0, rising, 0.0),
        sector if sector < 2.0 => (rising, 1.0, 0.0),
        sector if sector < 3.0 => (0.0, 1.0, rising),
        sector if sector < 4.0 => (0.0, rising, 1.0),
        sector if sector < 5.0 => (rising, 0.0, 1.0),
        _ => (1.0, 0.0, rising),
    };
    let channel = |share: f64| {
        ((1.0 - saturation + saturation * share) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    RgbColor(channel(red), channel(green), channel(blue))
}

/// Roughly how a white looks: amber at 2200 K, cool blue at 4000 K and above.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0-255 first"
)]
fn kelvin(kelvin: u32) -> RgbColor {
    let t = ((f64::from(kelvin) - 2200.0) / 1800.0).clamp(0.0, 1.0);
    let mix = |warm: f64, cool: f64| (warm + (cool - warm) * t).round().clamp(0.0, 255.0) as u8;
    RgbColor(mix(255.0, 222.0), mix(176.0, 232.0), mix(102.0, 255.0))
}

/// "just now", "42 s ago", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(at: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(at).as_secs().max(0);
    match seconds {
        0..10 => "just now".to_owned(),
        10..60 => format!("{seconds} s ago"),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// How a device is listed: a dot for on or off, and what it shows.
pub fn state(device: &Device) -> String {
    let a = &device.attributes;
    if device.is_reachable == Some(false) {
        return paint(YELLOW, "unreachable");
    }
    match device.kind() {
        DeviceKind::Controller => match a.battery_percentage() {
            Some(level) if level < LOW_BATTERY => {
                paint(YELLOW, format!("battery {level} % ⚠ replace"))
            }
            Some(level) => format!("battery {level} %"),
            None => String::new(),
        },
        DeviceKind::Light | DeviceKind::Outlet => {
            let on = a.is_on() == Some(true);
            let mut text = if on {
                paint(GREEN, "on")
            } else {
                paint(DIM, "off")
            };
            if on && device.controls().brightness {
                if let Some(level) = a.light_level() {
                    text = format!("{text}  {level} %");
                }
                if let Some(color) = light_color(device) {
                    text = format!("{text}  {}", paint(color.on_default(), "■"));
                }
            }
            text
        }
        DeviceKind::Gateway | DeviceKind::Other => String::new(),
    }
}

pub fn dot(device: &Device) -> String {
    match device.attributes.is_on() {
        Some(true) => paint(GREEN, "●"),
        Some(false) => paint(DIM, "○"),
        None => " ".to_owned(),
    }
}

pub fn kind(device: &Device) -> &'static str {
    match device.kind() {
        DeviceKind::Light => "light",
        DeviceKind::Outlet => "outlet",
        DeviceKind::Controller => "remote",
        DeviceKind::Gateway => "hub",
        DeviceKind::Other => "other",
    }
}

pub fn heading(room: Option<&Room>) -> String {
    let name = room.map_or("Other devices", |room| room.name.as_str());
    paint(BLUE.bold(), name)
}

/// Names of the devices with `ids`, for remotes and what they switch.
pub fn names(home: &Home, ids: &[dirigera::DeviceId]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| home.device(id))
        .map(|device| device.name().to_owned())
        .collect()
}

/// `text` padded to `width` characters, counting characters rather than bytes.
pub fn pad(text: &str, width: usize) -> String {
    let length = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(length)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hues_and_whites_become_colors() {
        assert_eq!(hsv(0.0, 1.0), RgbColor(255, 0, 0));
        assert_eq!(hsv(120.0, 1.0), RgbColor(0, 255, 0));
        assert_eq!(hsv(240.0, 0.5), RgbColor(128, 128, 255));
        assert_eq!(hsv(30.0, 0.0), RgbColor(255, 255, 255));
        assert_eq!(kelvin(2200), RgbColor(255, 176, 102));
        assert_eq!(kelvin(6500), RgbColor(222, 232, 255));
    }

    #[test]
    fn times_read_as_ago() {
        let now: Timestamp = "2026-01-01T12:00:00Z".parse().unwrap();
        let at = |seconds: i64| now - jiff::SignedDuration::from_secs(seconds);
        assert_eq!(ago(at(3), now), "just now");
        assert_eq!(ago(at(42), now), "42 s ago");
        assert_eq!(ago(at(300), now), "5 min ago");
        assert_eq!(ago(at(3 * 3600), now), "3 h ago");
        assert_eq!(ago(at(2 * 86_400), now), "2 d ago");
    }

    #[test]
    fn padding_counts_characters() {
        assert_eq!(pad("Kuchyňa", 9), "Kuchyňa  ");
        assert_eq!(pad("too long", 3), "too long");
    }
}
