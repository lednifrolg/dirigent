//! Colors and words shared by the command line and the TUI, so both show a home the same way.

use dirigera::{ColorMode, Device, DeviceUpdate};
use jiff::Timestamp;

use crate::wanted::Color;

/// Remotes below this battery level get a warning.
pub const LOW_BATTERY: u8 = 10;

/// A color in true color, for what a light shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The color a light shows, in true color: its hue and saturation, or its white.
pub fn light(device: &Device) -> Option<Rgb> {
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
pub fn hsv(hue: f64, saturation: f64) -> Rgb {
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
    Rgb(channel(red), channel(green), channel(blue))
}

/// Roughly how a white looks: amber at 2200 K, cool blue at 4000 K and above.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0-255 first"
)]
pub fn kelvin(kelvin: u32) -> Rgb {
    let t = ((f64::from(kelvin) - 2200.0) / 1800.0).clamp(0.0, 1.0);
    let mix = |warm: f64, cool: f64| (warm + (cool - warm) * t).round().clamp(0.0, 255.0) as u8;
    Rgb(mix(255.0, 222.0), mix(176.0, 232.0), mix(102.0, 255.0))
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

/// What a device shows, for the attributes changes touch.
pub fn describe_state(device: &Device) -> String {
    let a = &device.attributes;
    let mut parts = Vec::new();
    if let Some(on) = a.is_on() {
        parts.push(if on {
            "on".to_owned()
        } else {
            "off".to_owned()
        });
    }
    let controls = device.controls();
    if controls.brightness
        && let Some(level) = a.light_level()
    {
        parts.push(format!("brightness {level} %"));
    }
    match a.color_mode() {
        Some(ColorMode::Temperature) => {
            if let Some(kelvin) = a.color_temperature() {
                parts.push(format!("white {kelvin} K"));
            }
        }
        Some(ColorMode::Color) => {
            if let (Some(hue), Some(saturation)) = (a.color_hue(), a.color_saturation()) {
                parts.push(format!("color {}", Color { hue, saturation }));
            }
        }
        _ => {}
    }
    parts.join(", ")
}

/// The changes in a state update, in words where dirigent knows them.
pub fn describe_update(update: &DeviceUpdate) -> String {
    let a = &update.attributes;
    let mut parts = Vec::new();
    if let Some(on) = a.is_on() {
        parts.push(if on {
            "on".to_owned()
        } else {
            "off".to_owned()
        });
    }
    if let Some(level) = a.light_level() {
        parts.push(format!("brightness {level} %"));
    }
    if let Some(kelvin) = a.color_temperature() {
        parts.push(format!("white {kelvin} K"));
    }
    if let (Some(hue), Some(saturation)) = (a.color_hue(), a.color_saturation()) {
        parts.push(format!("color {}", Color { hue, saturation }));
    }
    if let Some(level) = a.battery_percentage() {
        parts.push(format!("battery {level} %"));
    }
    if let Some(reachable) = update.is_reachable {
        parts.push(if reachable {
            "reachable".to_owned()
        } else {
            "unreachable".to_owned()
        });
    }
    let known = [
        "isOn",
        "lightLevel",
        "colorTemperature",
        "colorHue",
        "colorSaturation",
        "colorMode",
        "batteryPercentage",
    ];
    for (key, value) in a.iter().filter(|(key, _)| !known.contains(key)) {
        parts.push(format!("{key}={value}"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hues_and_whites_become_colors() {
        assert_eq!(hsv(0.0, 1.0), Rgb(255, 0, 0));
        assert_eq!(hsv(120.0, 1.0), Rgb(0, 255, 0));
        assert_eq!(hsv(240.0, 0.5), Rgb(128, 128, 255));
        assert_eq!(hsv(30.0, 0.0), Rgb(255, 255, 255));
        assert_eq!(kelvin(2200), Rgb(255, 176, 102));
        assert_eq!(kelvin(6500), Rgb(222, 232, 255));
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
}
