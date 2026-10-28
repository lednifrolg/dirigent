//! Validated changes to a device.

use std::fmt;

use serde_json::{Value, json};

use crate::attributes::{Attributes, TemperatureRange};
use crate::home::Device;

/// Hue and saturation that differ by less than this count as the same color. Zigbee lights keep
/// saturation in 254 steps and hue in finer ones, so the hub reports 0.8 back as 0.7992.
const HUE_TOLERANCE: f64 = 0.5;
const SATURATION_TOLERANCE: f64 = 0.005;

/// White as Zigbee lights keep it: whole mireds. The hub reports 2700 K back as 2702 K.
fn mireds(kelvin: u32) -> u32 {
    (1_000_000 + kelvin / 2) / kelvin.max(1)
}

/// One change to one device. The hub takes attributes one at a time: some cannot be combined in
/// a request, so switching a light on and dimming it are two changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    Power(bool),
    /// Brightness in percent, 1 to 100.
    Brightness(u8),
    /// White in kelvin, within the light's [`TemperatureRange`].
    White(u32),
    /// Hue in degrees (0 up to 360) and saturation (0 to 1).
    Color {
        hue: f64,
        saturation: f64,
    },
}

/// Why a [`Change`] does not fit a device.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ChangeError {
    #[error("{device} cannot {what}")]
    NotSupported { device: String, what: &'static str },
    #[error("brightness {0} % is outside 1-100 %")]
    Brightness(u8),
    #[error("{value} K is outside {range} for {device}")]
    White {
        device: String,
        value: u32,
        range: TemperatureRange,
    },
    #[error("hue {0}° is outside 0-359°")]
    Hue(f64),
    #[error("saturation {0} is outside 0-1")]
    Saturation(f64),
}

impl Change {
    /// Checks that `device` can take the change, with the value in its range.
    pub fn check(&self, device: &Device) -> Result<(), ChangeError> {
        let controls = device.controls();
        let unsupported = |what| ChangeError::NotSupported {
            device: device.name().to_owned(),
            what,
        };
        match *self {
            Self::Power(_) if !controls.power => Err(unsupported("switch on and off")),
            Self::Brightness(_) if !controls.brightness => Err(unsupported("dim")),
            Self::Brightness(level) if !(1..=100).contains(&level) => {
                Err(ChangeError::Brightness(level))
            }
            Self::White(kelvin) => match controls.white {
                None => Err(unsupported("show white")),
                Some(range) if !range.contains(kelvin) => Err(ChangeError::White {
                    device: device.name().to_owned(),
                    value: kelvin,
                    range,
                }),
                Some(_) => Ok(()),
            },
            Self::Color { .. } if !controls.color => Err(unsupported("show colors")),
            Self::Color { hue, .. } if !(0.0..360.0).contains(&hue) => Err(ChangeError::Hue(hue)),
            Self::Color { saturation, .. } if !(0.0..=1.0).contains(&saturation) => {
                Err(ChangeError::Saturation(saturation))
            }
            _ => Ok(()),
        }
    }

    /// The attributes the change sets.
    pub fn attributes(&self) -> Attributes {
        let mut attributes = Attributes::default();
        match *self {
            Self::Power(on) => attributes.insert("isOn", on),
            Self::Brightness(level) => attributes.insert("lightLevel", level),
            Self::White(kelvin) => attributes.insert("colorTemperature", kelvin),
            Self::Color { hue, saturation } => {
                attributes.insert("colorHue", hue);
                attributes.insert("colorSaturation", saturation);
            }
        }
        attributes
    }

    /// Whether `attributes`, e.g. from a state change, show the change applied.
    pub fn is_shown_by(&self, attributes: &Attributes) -> bool {
        match *self {
            Self::Power(on) => attributes.is_on() == Some(on),
            Self::Brightness(level) => attributes.light_level() == Some(level),
            Self::White(kelvin) => attributes
                .color_temperature()
                .is_some_and(|shown| mireds(shown) == mireds(kelvin)),
            Self::Color { hue, saturation } => {
                let close = |a: Option<f64>, b: f64, tolerance| {
                    a.is_some_and(|a| (a - b).abs() < tolerance)
                };
                close(attributes.color_hue(), hue, HUE_TOLERANCE)
                    && close(
                        attributes.color_saturation(),
                        saturation,
                        SATURATION_TOLERANCE,
                    )
            }
        }
    }

    /// Body of `PATCH /v1/devices/{id}`.
    pub(crate) fn body(&self) -> Value {
        json!([{ "attributes": self.attributes() }])
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Power(on) => f.write_str(if on { "on" } else { "off" }),
            Self::Brightness(level) => write!(f, "brightness {level} %"),
            Self::White(kelvin) => write!(f, "white {kelvin} K"),
            Self::Color { hue, saturation } => {
                write!(f, "color {hue:.0}° {:.0} %", saturation * 100.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn device(value: Value) -> Device {
        serde_json::from_value(value).unwrap()
    }

    fn strip() -> Device {
        device(json!({
            "id": "light_1",
            "deviceType": "light",
            "attributes": {
                "customName": "Strip",
                "isOn": true,
                "lightLevel": 60,
                "colorTemperatureMin": 4000,
                "colorTemperatureMax": 2202,
            },
            "capabilities": {
                "canSend": [],
                "canReceive": ["customName", "isOn", "lightLevel", "colorTemperature", "colorHue", "colorSaturation"],
            },
        }))
    }

    fn outlet() -> Device {
        device(json!({
            "id": "outlet_1",
            "deviceType": "outlet",
            "attributes": { "customName": "Kitchen", "isOn": false, "lightLevel": 100 },
            "capabilities": { "canSend": [], "canReceive": ["customName", "isOn", "lightLevel"] },
        }))
    }

    #[test]
    fn lights_take_every_change_in_range() {
        let strip = strip();
        for change in [
            Change::Power(false),
            Change::Brightness(1),
            Change::Brightness(100),
            Change::White(2202),
            Change::White(4000),
            Change::Color {
                hue: 359.9,
                saturation: 1.0,
            },
        ] {
            assert_eq!(change.check(&strip), Ok(()), "{change}");
        }
        assert_eq!(
            Change::Brightness(0).check(&strip),
            Err(ChangeError::Brightness(0))
        );
        assert!(matches!(
            Change::White(2000).check(&strip),
            Err(ChangeError::White { value: 2000, .. })
        ));
        assert_eq!(
            Change::Color {
                hue: 360.0,
                saturation: 0.5
            }
            .check(&strip),
            Err(ChangeError::Hue(360.0))
        );
        assert!(
            Change::Color {
                hue: f64::NAN,
                saturation: 0.5
            }
            .check(&strip)
            .is_err()
        );
        assert_eq!(
            Change::Color {
                hue: 30.0,
                saturation: 1.5
            }
            .check(&strip),
            Err(ChangeError::Saturation(1.5))
        );
    }

    #[test]
    fn outlets_only_switch_although_they_list_light_level() {
        let outlet = outlet();
        assert_eq!(Change::Power(true).check(&outlet), Ok(()));
        let error = Change::Brightness(30).check(&outlet).unwrap_err();
        assert_eq!(error.to_string(), "Kitchen cannot dim");
        assert!(Change::White(2700).check(&outlet).is_err());
    }

    #[test]
    fn remotes_take_no_changes() {
        let remote = device(json!({
            "id": "remote_1",
            "deviceType": "lightController",
            "attributes": { "customName": "Remote", "batteryPercentage": 90 },
            "capabilities": { "canSend": ["isOn", "lightLevel"], "canReceive": ["customName"] },
        }));
        assert!(!remote.controls().any());
        assert!(Change::Power(true).check(&remote).is_err());
    }

    #[test]
    fn bodies_carry_one_attribute_set() {
        assert_eq!(
            Change::Power(true).body(),
            json!([{"attributes": {"isOn": true}}])
        );
        assert_eq!(
            Change::Brightness(40).body(),
            json!([{"attributes": {"lightLevel": 40}}])
        );
        assert_eq!(
            Change::White(2700).body(),
            json!([{"attributes": {"colorTemperature": 2700}}])
        );
        assert_eq!(
            Change::Color {
                hue: 30.0,
                saturation: 0.8
            }
            .body(),
            json!([{"attributes": {"colorHue": 30.0, "colorSaturation": 0.8}}])
        );
    }

    #[test]
    fn state_changes_confirm_with_tolerance() {
        let shown: Attributes = serde_json::from_value(
            json!({"isOn": true, "lightLevel": 40, "colorHue": 30.2, "colorSaturation": 0.801}),
        )
        .unwrap();
        assert!(Change::Power(true).is_shown_by(&shown));
        assert!(!Change::Power(false).is_shown_by(&shown));
        assert!(Change::Brightness(40).is_shown_by(&shown));
        assert!(
            Change::Color {
                hue: 30.0,
                saturation: 0.8
            }
            .is_shown_by(&shown)
        );
        assert!(
            !Change::Color {
                hue: 31.0,
                saturation: 0.8
            }
            .is_shown_by(&shown)
        );
        assert!(!Change::White(2700).is_shown_by(&shown));
    }

    #[test]
    fn white_confirms_in_whole_mireds() {
        // Seen live: 2700 K came back as 2702 K, 2638 K as 2638 K.
        let shown = |kelvin: u32| -> Attributes {
            serde_json::from_value(json!({ "colorTemperature": kelvin })).unwrap()
        };
        assert!(Change::White(2700).is_shown_by(&shown(2702)));
        assert!(Change::White(2638).is_shown_by(&shown(2638)));
        assert!(!Change::White(2700).is_shown_by(&shown(2710)));
        assert!(Change::White(4000).is_shown_by(&shown(4000)));
        assert!(!Change::White(4000).is_shown_by(&shown(3984)));
    }

    #[test]
    fn changes_read_well() {
        assert_eq!(Change::Power(false).to_string(), "off");
        assert_eq!(Change::Brightness(40).to_string(), "brightness 40 %");
        assert_eq!(Change::White(2700).to_string(), "white 2700 K");
        assert_eq!(
            Change::Color {
                hue: 30.0,
                saturation: 0.8
            }
            .to_string(),
            "color 30° 80 %"
        );
    }
}
