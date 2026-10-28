//! Device attributes, kept as the hub sends them.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The `attributes` of a device or of a state change: every field the hub sent, read through
/// typed accessors. Nothing is dropped or reformatted, so the JSON round-trips unchanged, and a
/// value of an unexpected type reads as missing instead of failing the whole answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Attributes(Map<String, Value>);

/// Range of white a light can show, in kelvin. The hub reports it in
/// `colorTemperatureMin`/`colorTemperatureMax`, whose order follows mireds on some lights, so the
/// range is normalized here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemperatureRange {
    pub warmest: u32,
    pub coolest: u32,
}

impl TemperatureRange {
    pub fn contains(&self, kelvin: u32) -> bool {
        (self.warmest..=self.coolest).contains(&kelvin)
    }

    pub fn clamp(&self, kelvin: u32) -> u32 {
        kelvin.clamp(self.warmest, self.coolest)
    }
}

impl fmt::Display for TemperatureRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{} K", self.warmest, self.coolest)
    }
}

/// What a color light shows: a hue and saturation, or a white.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorMode {
    Color,
    Temperature,
    /// A mode this client does not know.
    Other(String),
}

impl Attributes {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.0.insert(key.into(), value.into());
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Overwrites the fields `changes` carries, as a state change from the hub does.
    pub fn apply(&mut self, changes: &Attributes) {
        for (key, value) in &changes.0 {
            self.0.insert(key.clone(), value.clone());
        }
    }

    /// The name the user gave the device in the IKEA app; empty if it has none.
    pub fn custom_name(&self) -> &str {
        self.text("customName").unwrap_or_default()
    }

    pub fn model(&self) -> Option<&str> {
        self.text("model")
    }

    pub fn manufacturer(&self) -> Option<&str> {
        self.text("manufacturer")
    }

    pub fn product_code(&self) -> Option<&str> {
        self.text("productCode")
    }

    pub fn firmware_version(&self) -> Option<&str> {
        self.text("firmwareVersion")
    }

    /// `upToDate`, `updateAvailable`, …
    pub fn ota_status(&self) -> Option<&str> {
        self.text("otaStatus")
    }

    pub fn is_on(&self) -> Option<bool> {
        self.0.get("isOn")?.as_bool()
    }

    /// Brightness in percent.
    pub fn light_level(&self) -> Option<u8> {
        self.whole("lightLevel", 100)
            .and_then(|level| u8::try_from(level).ok())
    }

    /// White in kelvin.
    pub fn color_temperature(&self) -> Option<u32> {
        self.whole("colorTemperature", u32::MAX)
    }

    pub fn temperature_range(&self) -> Option<TemperatureRange> {
        let a = self.whole("colorTemperatureMin", u32::MAX)?;
        let b = self.whole("colorTemperatureMax", u32::MAX)?;
        Some(TemperatureRange {
            warmest: a.min(b),
            coolest: a.max(b),
        })
    }

    /// Hue in degrees, 0 up to 360.
    pub fn color_hue(&self) -> Option<f64> {
        self.number("colorHue")
    }

    /// Saturation from 0 to 1.
    pub fn color_saturation(&self) -> Option<f64> {
        self.number("colorSaturation")
    }

    pub fn color_mode(&self) -> Option<ColorMode> {
        Some(match self.text("colorMode")? {
            "color" => ColorMode::Color,
            "temperature" => ColorMode::Temperature,
            other => ColorMode::Other(other.to_owned()),
        })
    }

    pub fn battery_percentage(&self) -> Option<u8> {
        self.whole("batteryPercentage", 100)
            .and_then(|level| u8::try_from(level).ok())
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.as_str()
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.0.get(key)?.as_f64().filter(|n| n.is_finite())
    }

    /// The number at `key` rounded into `0..=max`; the hub sends whole numbers, but a fraction
    /// should not hide the value.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is rounded and clamped into the u32 range first"
    )]
    fn whole(&self, key: &str, max: u32) -> Option<u32> {
        let n = self.number(key)?;
        Some(n.round().clamp(0.0, f64::from(max)) as u32)
    }
}

impl From<Map<String, Value>> for Attributes {
    fn from(fields: Map<String, Value>) -> Self {
        Self(fields)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn attributes(value: Value) -> Attributes {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn temperature_range_is_normalized() {
        let swapped = attributes(json!({"colorTemperatureMin": 4000, "colorTemperatureMax": 2202}));
        let range = swapped.temperature_range().unwrap();
        assert_eq!((range.warmest, range.coolest), (2202, 4000));
        assert!(range.contains(2700) && !range.contains(2000));
        assert_eq!(range.clamp(5000), 4000);
        let ordered = attributes(json!({"colorTemperatureMin": 2202, "colorTemperatureMax": 4000}));
        assert_eq!(ordered.temperature_range(), Some(range));
        assert_eq!(range.to_string(), "2202-4000 K");
    }

    #[test]
    fn numbers_read_leniently() {
        let a = attributes(json!({
            "lightLevel": 42.6,
            "colorTemperature": "warm",
            "batteryPercentage": 140,
            "colorHue": 289.99,
            "isOn": 1,
        }));
        assert_eq!(a.light_level(), Some(43));
        assert_eq!(a.color_temperature(), None);
        assert_eq!(a.battery_percentage(), Some(100));
        assert_eq!(a.color_hue(), Some(289.99));
        assert_eq!(a.is_on(), None);
        assert_eq!(a.custom_name(), "");
    }

    #[test]
    fn changes_overwrite_only_their_fields() {
        let mut a = attributes(json!({"isOn": true, "lightLevel": 100, "customName": "Strip"}));
        a.apply(&attributes(json!({"isOn": false})));
        assert_eq!(a.is_on(), Some(false));
        assert_eq!(a.light_level(), Some(100));
        assert_eq!(a.custom_name(), "Strip");
    }

    #[test]
    fn color_modes() {
        assert_eq!(
            attributes(json!({"colorMode": "color"})).color_mode(),
            Some(ColorMode::Color)
        );
        assert_eq!(
            attributes(json!({"colorMode": "temperature"})).color_mode(),
            Some(ColorMode::Temperature)
        );
        assert_eq!(
            attributes(json!({"colorMode": "xy"})).color_mode(),
            Some(ColorMode::Other("xy".into()))
        );
    }
}
