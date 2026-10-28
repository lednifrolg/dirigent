//! The home as `/v1/home` describes it: devices, rooms, scenes and users.
//!
//! Every type keeps the fields it does not model in `extra`, so an answer serializes back to the
//! same JSON and nothing the hub sends is lost, also not from newer firmware.

use std::fmt;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::attributes::{Attributes, TemperatureRange};
use crate::events::DeviceUpdate;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id!(
    /// Device identifier, a UUID with a suffix such as `_1`.
    DeviceId
);
id!(
    /// Room identifier.
    RoomId
);
id!(
    /// Scene identifier.
    SceneId
);
id!(
    /// Identifier of a client paired with the hub.
    UserId
);

/// Everything the hub knows (`GET /v1/home`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Home {
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub rooms: Vec<Room>,
    #[serde(default)]
    pub scenes: Vec<Scene>,
    /// Clients paired with the hub.
    #[serde(default)]
    pub users: Vec<User>,
    /// The client making the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    /// The hub itself; it is also in `devices` as a gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<Device>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Home {
    pub fn device(&self, id: &DeviceId) -> Option<&Device> {
        self.devices.iter().find(|device| &device.id == id)
    }

    pub fn room(&self, id: &RoomId) -> Option<&Room> {
        self.rooms.iter().find(|room| &room.id == id)
    }

    pub fn scene(&self, id: &SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|scene| &scene.id == id)
    }

    /// The devices in `room`, in the hub's order.
    pub fn devices_in<'a>(&'a self, room: &'a RoomId) -> impl Iterator<Item = &'a Device> {
        self.devices
            .iter()
            .filter(move |device| device.room.as_ref().is_some_and(|r| &r.id == room))
    }

    /// Applies a state change from the event stream. Returns whether a device matched; if not,
    /// the home is out of date and should be read again.
    pub fn apply(&mut self, update: &DeviceUpdate) -> bool {
        let mut matched = false;
        let hub = self.hub.iter_mut();
        for device in self.devices.iter_mut().chain(hub) {
            if device.id == update.id {
                device.apply(update);
                matched = true;
            }
        }
        matched
    }

    /// Applies a `sceneUpdated` event's data: the scene as it is now, or the fields that changed.
    /// Returns the scene as it was before; `None` if no scene matched, and the home should be
    /// read again.
    pub fn apply_scene(&mut self, data: &Value) -> Option<Scene> {
        let id = data["id"].as_str()?;
        let scene = self
            .scenes
            .iter_mut()
            .find(|scene| scene.id.as_str() == id)?;
        let before = scene.clone();
        if let (Ok(Value::Object(mut merged)), Value::Object(fields)) =
            (serde_json::to_value(&*scene), data)
        {
            merged.extend(fields.clone());
            if let Ok(updated) = serde_json::from_value(Value::Object(merged)) {
                *scene = updated;
            }
        }
        Some(before)
    }
}

/// What a device is, from its `deviceType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Light,
    Outlet,
    /// A remote or another controller: it reports presses and its battery, it cannot be
    /// switched.
    Controller,
    /// The hub.
    Gateway,
    /// A device type this client does not control; it is shown but left alone.
    Other,
}

/// A device paired with the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: DeviceId,
    /// `light`, `outlet`, `lightController`, `gateway`, …; see [`Device::kind`].
    pub device_type: String,
    /// Broader group: `light`, `outlet`, `controller`, `gateway`, …
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_reachable: Option<bool>,
    /// As the hub sends it; see [`Device::seen_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub attributes: Attributes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<Room>,
    /// Remotes that switch this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_links: Option<Vec<DeviceId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_hidden: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Device {
    pub fn kind(&self) -> DeviceKind {
        match self.device_type.as_str() {
            "light" => DeviceKind::Light,
            "outlet" => DeviceKind::Outlet,
            "gateway" => DeviceKind::Gateway,
            other if other.ends_with("Controller") => DeviceKind::Controller,
            _ => DeviceKind::Other,
        }
    }

    /// The name from the IKEA app; empty if it has none.
    pub fn name(&self) -> &str {
        self.attributes.custom_name()
    }

    pub fn seen_at(&self) -> Option<Timestamp> {
        self.last_seen.as_deref()?.parse().ok()
    }

    pub fn remote_links(&self) -> &[DeviceId] {
        self.remote_links.as_deref().unwrap_or_default()
    }

    fn receives(&self, attribute: &str) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|caps| caps.receives(attribute))
    }

    /// What this client lets the user change on the device. It follows the device's
    /// capabilities, except that only lights dim: control outlets list `lightLevel` but are
    /// switches.
    pub fn controls(&self) -> Controls {
        let kind = self.kind();
        let switchable = matches!(kind, DeviceKind::Light | DeviceKind::Outlet);
        let light = kind == DeviceKind::Light;
        Controls {
            power: switchable && self.receives("isOn"),
            brightness: light && self.receives("lightLevel"),
            white: (light && self.receives("colorTemperature"))
                .then(|| self.attributes.temperature_range())
                .flatten(),
            color: light && self.receives("colorHue") && self.receives("colorSaturation"),
        }
    }

    pub(crate) fn apply(&mut self, update: &DeviceUpdate) {
        self.attributes.apply(&update.attributes);
        if let Some(reachable) = update.is_reachable {
            self.is_reachable = Some(reachable);
        }
        if let Some(seen) = &update.last_seen {
            self.last_seen = Some(seen.clone());
        }
    }
}

/// What the user can change on a device; see [`Device::controls`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub power: bool,
    pub brightness: bool,
    /// The range of white, if the light can show white.
    pub white: Option<TemperatureRange>,
    pub color: bool,
}

impl Controls {
    pub fn any(&self) -> bool {
        self.power || self.brightness || self.white.is_some() || self.color
    }
}

/// Attributes a device can report (`canSend`) and accept (`canReceive`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub can_send: Vec<String>,
    #[serde(default)]
    pub can_receive: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Capabilities {
    pub fn receives(&self, attribute: &str) -> bool {
        self.can_receive.iter().any(|a| a == attribute)
    }

    pub fn sends(&self, attribute: &str) -> bool {
        self.can_send.iter().any(|a| a == attribute)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    /// IKEA color name, e.g. `ikea_green_no_65`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: SceneId,
    pub info: SceneInfo,
    /// `userScene`, `wakeUpScene`, …
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub actions: Vec<SceneAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_triggered: Option<String>,
    /// Seconds after a run during which the scene can be undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_allowed_duration: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Scene {
    pub fn name(&self) -> &str {
        &self.info.name
    }

    pub fn triggered_at(&self) -> Option<Timestamp> {
        self.last_triggered.as_deref()?.parse().ok()
    }

    pub fn undo_window(&self) -> Option<Duration> {
        self.undo_allowed_duration
            .filter(|&seconds| seconds > 0)
            .map(Duration::from_secs)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a scene does to one device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Attributes>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A client paired with the hub: the IKEA app on a phone, or a client like this one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub uid: UserId,
    /// The name the client gave at pairing.
    pub name: String,
    /// `homesmart.local` for clients on the home network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl User {
    pub fn created_at(&self) -> Option<Timestamp> {
        self.created_timestamp.as_deref()?.parse().ok()
    }
}
