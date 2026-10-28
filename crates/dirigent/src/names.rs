//! Finding what the user means by a name.
//!
//! Names match regardless of case and diacritics, so `kupelna` finds "Kúpeľňa". An exact name
//! wins, then a name that starts with the text, then one that contains it; each step has to
//! find exactly one thing.

use dirigera::{Device, DeviceKind, Home, Room, Scene};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::error::AppError;

/// What a name can stand for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Device,
    Room,
    Scene,
}

impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Room => "room",
            Self::Scene => "scene",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Target<'a> {
    Device(&'a Device),
    Room(&'a Room),
    Scene(&'a Scene),
}

impl Target<'_> {
    pub fn name(&self) -> &str {
        match self {
            Self::Device(device) => device.name(),
            Self::Room(room) => &room.name,
            Self::Scene(scene) => scene.name(),
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Device(device) => device.id.as_str(),
            Self::Room(room) => room.id.as_str(),
            Self::Scene(scene) => scene.id.as_str(),
        }
    }
}

/// Lower case without diacritics.
pub fn fold(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Everything in `home` of one of `kinds` that a name can stand for. The hub itself is left
/// out: there is nothing to change on it.
fn candidates<'a>(home: &'a Home, kinds: &[Kind]) -> Vec<Target<'a>> {
    let mut all = Vec::new();
    if kinds.contains(&Kind::Device) {
        all.extend(
            home.devices
                .iter()
                .filter(|device| device.kind() != DeviceKind::Gateway)
                .map(Target::Device),
        );
    }
    if kinds.contains(&Kind::Room) {
        all.extend(home.rooms.iter().map(Target::Room));
    }
    if kinds.contains(&Kind::Scene) {
        all.extend(home.scenes.iter().map(Target::Scene));
    }
    all
}

/// The one thing of `kinds` that `query` names.
pub fn resolve<'a>(home: &'a Home, query: &str, kinds: &[Kind]) -> Result<Target<'a>, AppError> {
    let all = candidates(home, kinds);
    if let Some(target) = all.iter().find(|target| target.id() == query) {
        return Ok(*target);
    }
    let wanted = fold(query.trim());
    if wanted.is_empty() {
        return Err(AppError::Usage("the name is empty".into()));
    }
    let steps: [&dyn Fn(&str) -> bool; 3] = [
        &|name| name == wanted,
        &|name| name.starts_with(&wanted),
        &|name| name.contains(&wanted),
    ];
    for matches in steps {
        let found: Vec<Target<'a>> = all
            .iter()
            .filter(|target| matches(&fold(target.name())))
            .copied()
            .collect();
        match found.as_slice() {
            [] => {}
            [one] => return Ok(*one),
            several => return Err(ambiguous(query, several, kinds)),
        }
    }
    let nouns: Vec<&str> = kinds.iter().map(|kind| kind.noun()).collect();
    Err(AppError::Usage(format!(
        "no {} named \"{query}\"; `dirigent list` shows the names",
        nouns.join(" or ")
    )))
}

fn ambiguous(query: &str, found: &[Target<'_>], kinds: &[Kind]) -> AppError {
    let names: Vec<String> = found
        .iter()
        .map(|target| match target {
            Target::Room(room) if kinds.len() > 1 => format!("{} (room)", room.name),
            other => other.name().to_owned(),
        })
        .collect();
    let same = found
        .windows(2)
        .all(|pair| fold(pair[0].name()) == fold(pair[1].name()));
    let hint = if same && kinds.len() > 1 {
        "add --room or --device"
    } else {
        "use more of the name"
    };
    AppError::Usage(format!("\"{query}\" matches {}; {hint}", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn home() -> Home {
        let device = |id: &str, name: &str, kind: &str| json!({ "id": id, "deviceType": kind, "attributes": { "customName": name } });
        serde_json::from_value(json!({
            "devices": [
                device("d1", "desk lamp", "outlet"),
                device("d2", "floor lamp", "outlet"),
                device("d3", "Led strip", "light"),
                device("d4", "Kúpeľňa", "outlet"),
                device("d5", "Kitchen", "light"),
                device("hub", "Hub", "gateway"),
            ],
            "rooms": [
                { "id": "r1", "name": "Living room" },
                { "id": "r2", "name": "Kitchen" },
            ],
            "scenes": [{ "id": "s1", "info": { "name": "Evening" } }],
        }))
        .unwrap()
    }

    const DEVICES_AND_ROOMS: &[Kind] = &[Kind::Device, Kind::Room];

    fn name(query: &str, kinds: &[Kind]) -> Result<String, String> {
        let home = home();
        resolve(&home, query, kinds)
            .map(|target| target.name().to_owned())
            .map_err(|error| error.to_string())
    }

    #[test]
    fn names_match_by_prefix_then_part_without_case_or_diacritics() {
        assert_eq!(name("led", DEVICES_AND_ROOMS).unwrap(), "Led strip");
        assert_eq!(name("STRIP", DEVICES_AND_ROOMS).unwrap(), "Led strip");
        assert_eq!(name("kupelna", DEVICES_AND_ROOMS).unwrap(), "Kúpeľňa");
        assert_eq!(name("kup", DEVICES_AND_ROOMS).unwrap(), "Kúpeľňa");
        assert_eq!(name("living", DEVICES_AND_ROOMS).unwrap(), "Living room");
        assert_eq!(name("d3", DEVICES_AND_ROOMS).unwrap(), "Led strip");
        assert_eq!(name("even", &[Kind::Scene]).unwrap(), "Evening");
    }

    #[test]
    fn exact_names_win_over_longer_ones() {
        assert_eq!(name("desk lamp", DEVICES_AND_ROOMS).unwrap(), "desk lamp");
    }

    #[test]
    fn several_matches_are_refused_with_a_hint() {
        assert_eq!(
            name("lamp", DEVICES_AND_ROOMS).unwrap_err(),
            "\"lamp\" matches desk lamp, floor lamp; use more of the name"
        );
        assert_eq!(
            name("kitchen", DEVICES_AND_ROOMS).unwrap_err(),
            "\"kitchen\" matches Kitchen, Kitchen (room); add --room or --device"
        );
        assert_eq!(name("kitchen", &[Kind::Room]).unwrap(), "Kitchen");
    }

    #[test]
    fn unknown_names_and_the_hub_are_not_found() {
        assert_eq!(
            name("garage", DEVICES_AND_ROOMS).unwrap_err(),
            "no device or room named \"garage\"; `dirigent list` shows the names"
        );
        assert!(name("hub", &[Kind::Device]).is_err());
        assert!(name("evening", DEVICES_AND_ROOMS).is_err());
    }
}
