//! Values as the user writes them on the command line: brightness, white and color.

use std::fmt;
use std::str::FromStr;

use dirigera::TemperatureRange;

/// Brightness in percent, or a change of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    To(u8),
    By(i16),
}

impl Level {
    /// The brightness to set on a light at `current`, kept within 1-100 %.
    pub fn from(self, current: u8) -> u8 {
        match self {
            Self::To(level) => level,
            Self::By(change) => {
                let level = (i16::from(current) + change).clamp(1, 100);
                u8::try_from(level).expect("clamped to 1-100")
            }
        }
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid =
            || format!("expected a brightness of 1-100 %, or a change like +10 or -10, not {text}");
        let number = text.trim().trim_end_matches('%');
        if let Some(change) = number
            .strip_prefix('+')
            .or_else(|| number.starts_with('-').then_some(number))
        {
            let change: i16 = change.parse().map_err(|_| invalid())?;
            return if (-100..=100).contains(&change) {
                Ok(Self::By(change))
            } else {
                Err(invalid())
            };
        }
        match number.parse::<u8>() {
            Ok(level @ 1..=100) => Ok(Self::To(level)),
            _ => Err(invalid()),
        }
    }
}

/// White in kelvin, or the warmest, middle or coolest white a light can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum White {
    Kelvin(u32),
    Warm,
    Neutral,
    Cool,
}

impl White {
    pub fn kelvin(self, range: TemperatureRange) -> u32 {
        match self {
            Self::Kelvin(kelvin) => kelvin,
            Self::Warm => range.warmest,
            Self::Neutral => range.warmest.midpoint(range.coolest),
            Self::Cool => range.coolest,
        }
    }
}

impl FromStr for White {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_lowercase().as_str() {
            "warm" => Ok(Self::Warm),
            "neutral" => Ok(Self::Neutral),
            "cool" | "cold" => Ok(Self::Cool),
            other => other
                .trim_end_matches('k')
                .trim()
                .parse()
                .map(Self::Kelvin)
                .map_err(|_| {
                    format!("expected kelvin like 2700, or warm, neutral or cool, not {text}")
                }),
        }
    }
}

/// A color as a hue in degrees and a saturation from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub hue: f64,
    pub saturation: f64,
}

/// Named colors, the same as the TUI offers.
pub const PRESETS: [(&str, Color); 8] = [
    (
        "red",
        Color {
            hue: 0.0,
            saturation: 1.0,
        },
    ),
    (
        "orange",
        Color {
            hue: 28.0,
            saturation: 1.0,
        },
    ),
    (
        "yellow",
        Color {
            hue: 50.0,
            saturation: 1.0,
        },
    ),
    (
        "green",
        Color {
            hue: 120.0,
            saturation: 1.0,
        },
    ),
    (
        "cyan",
        Color {
            hue: 180.0,
            saturation: 1.0,
        },
    ),
    (
        "blue",
        Color {
            hue: 230.0,
            saturation: 1.0,
        },
    ),
    (
        "purple",
        Color {
            hue: 275.0,
            saturation: 1.0,
        },
    ),
    (
        "pink",
        Color {
            hue: 320.0,
            saturation: 0.6,
        },
    ),
];

impl Color {
    /// Hue and saturation of an RGB color; its lightness is the light's brightness, which is
    /// set apart.
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        if max == 0 || max == min {
            return Self {
                hue: 0.0,
                saturation: 0.0,
            };
        }
        let [r, g, b] = [red, green, blue].map(f64::from);
        let delta = f64::from(max - min);
        let hue = if max == red {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == green {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        Self {
            hue: hue.rem_euclid(360.0),
            saturation: delta / f64::from(max),
        }
    }
}

impl FromStr for Color {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let invalid = || {
            let names: Vec<&str> = PRESETS.iter().map(|(name, _)| *name).collect();
            format!(
                "expected a color name ({}), a hue with an optional saturation from 0 to 1 \
                 (30 or 30,0.8), or #rrggbb, not {text}",
                names.join(", ")
            )
        };
        if let Some((_, color)) = PRESETS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(text))
        {
            return Ok(*color);
        }
        if let Some(hex) = text.strip_prefix('#') {
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| invalid());
            return Ok(Self::from_rgb(channel(0)?, channel(2)?, channel(4)?));
        }
        let (hue, saturation) = text.split_once(',').unwrap_or((text, "1"));
        let hue: f64 = hue.trim().parse().map_err(|_| invalid())?;
        let saturation: f64 = saturation.trim().parse().map_err(|_| invalid())?;
        if !(0.0..360.0).contains(&hue) || !(0.0..=1.0).contains(&saturation) {
            return Err(invalid());
        }
        Ok(Self { hue, saturation })
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.0}° {:.0} %", self.hue, self.saturation * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_absolute_or_relative() {
        assert_eq!("40".parse(), Ok(Level::To(40)));
        assert_eq!("40%".parse(), Ok(Level::To(40)));
        assert_eq!("+10".parse(), Ok(Level::By(10)));
        assert_eq!("-10".parse(), Ok(Level::By(-10)));
        assert!("0".parse::<Level>().is_err());
        assert!("101".parse::<Level>().is_err());
        assert!("+200".parse::<Level>().is_err());
        assert!("bright".parse::<Level>().is_err());
        assert_eq!(Level::By(-10).from(5), 1);
        assert_eq!(Level::By(30).from(90), 100);
        assert_eq!(Level::By(-10).from(60), 50);
        assert_eq!(Level::To(40).from(60), 40);
    }

    #[test]
    fn whites_are_kelvin_or_names_for_the_range() {
        let range = TemperatureRange {
            warmest: 2202,
            coolest: 4000,
        };
        assert_eq!("2700".parse(), Ok(White::Kelvin(2700)));
        assert_eq!("2700K".parse(), Ok(White::Kelvin(2700)));
        assert_eq!("warm".parse::<White>().unwrap().kelvin(range), 2202);
        assert_eq!("Neutral".parse::<White>().unwrap().kelvin(range), 3101);
        assert_eq!("cool".parse::<White>().unwrap().kelvin(range), 4000);
        assert!("daylight".parse::<White>().is_err());
    }

    #[test]
    fn colors_are_names_hues_or_hex() {
        assert_eq!(
            "Orange".parse(),
            Ok(Color {
                hue: 28.0,
                saturation: 1.0
            })
        );
        assert_eq!(
            "30,0.8".parse(),
            Ok(Color {
                hue: 30.0,
                saturation: 0.8
            })
        );
        assert_eq!(
            "290".parse(),
            Ok(Color {
                hue: 290.0,
                saturation: 1.0
            })
        );
        let orange: Color = "#ff8800".parse().unwrap();
        assert!((orange.hue - 32.0).abs() < 0.1, "{orange:?}");
        assert!((orange.saturation - 1.0).abs() < 1e-9);
        let blue: Color = "#0000FF".parse().unwrap();
        assert!((blue.hue - 240.0).abs() < 1e-9);
        let grey: Color = "#808080".parse().unwrap();
        assert!(grey.saturation.abs() < 1e-9);
        for bad in ["360", "30,1.5", "#12345", "#gggggg", "teal"] {
            assert!(bad.parse::<Color>().is_err(), "{bad}");
        }
        assert_eq!(
            Color {
                hue: 30.0,
                saturation: 0.8
            }
            .to_string(),
            "30° 80 %"
        );
    }
}
