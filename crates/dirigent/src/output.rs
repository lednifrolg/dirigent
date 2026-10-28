//! What dirigent prints. Colors use the terminal's ANSI palette, so they follow the terminal
//! theme, and disappear when the output is not a terminal or `NO_COLOR` is set. A light's own
//! color is shown as it is, in true color.

use std::error::Error as _;
use std::fmt::Display;
use std::io::Write;

use anstream::{eprintln, println};
use anstyle::{AnsiColor, RgbColor, Style};
use dirigera::{Device, DeviceKind, Home, Room};
use serde::Serialize;

use crate::error::AppError;
use crate::theme;

pub const DIM: Style = Style::new().dimmed();
pub const BOLD: Style = Style::new().bold();
pub const GREEN: Style = AnsiColor::Green.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const RED: Style = AnsiColor::Red.on_default();
const BLUE: Style = AnsiColor::Blue.on_default();
pub use crate::theme::LOW_BATTERY;

/// The color a light shows, for the terminal.
pub fn light_color(device: &Device) -> Option<RgbColor> {
    theme::light(device).map(|theme::Rgb(r, g, b)| RgbColor(r, g, b))
}

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
    fn padding_counts_characters() {
        assert_eq!(pad("Kuchyňa", 9), "Kuchyňa  ");
        assert_eq!(pad("too long", 3), "too long");
    }
}
