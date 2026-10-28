//! The TUI's keys: one flat table that also drives the help overlay and the hint bar.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    /// Space or Enter: switch the selection on or off, run a scene, or what a dialog offers.
    Activate,
    /// Brightness, or the value in the color picker; `fine` for the small step.
    Brighter {
        fine: bool,
    },
    Dimmer {
        fine: bool,
    },
    Warmer,
    Cooler,
    Color,
    Identify,
    Undo,
    NextTab,
    /// Jump to the tab with this index.
    Tab(usize),
    Resync,
    Help,
    Quit,
    /// Escape: close or cancel.
    Back,
}

/// When a binding is shown in the hint bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Always,
    /// Only in the narrow layout, where the panes are tabs.
    Tabs,
    /// While the selection switches or runs: a device, a room, a scene.
    Activate,
    /// While the selection dims: a light, or a room with one.
    Dim,
    /// While the selected light shows white.
    White,
    /// While the selected light shows colors or white.
    Color,
    /// While a device that can blink is selected.
    Identify,
    /// While there is something to take back.
    Undo,
    Never,
}

pub struct Binding {
    /// The keys as the help shows them; empty for a key that shares the line above.
    pub keys: &'static str,
    /// The keys as the hint bar shows them.
    pub short: &'static str,
    pub help: &'static str,
    /// Short label for the hint bar.
    pub label: &'static str,
    pub hint: Hint,
    /// Lower stays longer in the hint bar when the window is narrow.
    pub rank: u8,
    codes: &'static [KeyCode],
    action: Action,
}

/// A second key for the action described on the line above.
const fn also(codes: &'static [KeyCode], action: Action) -> Binding {
    Binding {
        keys: "",
        short: "",
        help: "",
        label: "",
        hint: Hint::Never,
        rank: u8::MAX,
        codes,
        action,
    }
}

use KeyCode::Char;

pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "↑↓ jk",
        short: "↑↓",
        help: "select a room, device, scene or remote",
        label: "move",
        hint: Hint::Always,
        rank: 6,
        codes: &[KeyCode::Up, Char('k')],
        action: Action::Up,
    },
    also(&[KeyCode::Down, Char('j')], Action::Down),
    Binding {
        keys: "␣ ⏎",
        short: "␣",
        help: "switch on or off; a whole room; run a scene",
        label: "on/off",
        hint: Hint::Activate,
        rank: 0,
        codes: &[Char(' '), KeyCode::Enter],
        action: Action::Activate,
    },
    Binding {
        keys: "←→ hl",
        short: "←→",
        help: "brightness ±10 %, with ⇧ ±1 %; a room's lights",
        label: "brightness",
        hint: Hint::Dim,
        rank: 2,
        codes: &[KeyCode::Right, Char('l')],
        action: Action::Brighter { fine: false },
    },
    also(&[KeyCode::Left, Char('h')], Action::Dimmer { fine: false }),
    also(&[Char('L')], Action::Brighter { fine: true }),
    also(&[Char('H')], Action::Dimmer { fine: true }),
    Binding {
        keys: "[ ]",
        short: "[ ]",
        help: "warmer or cooler white",
        label: "white",
        hint: Hint::White,
        rank: 4,
        codes: &[Char('[')],
        action: Action::Warmer,
    },
    also(&[Char(']')], Action::Cooler),
    Binding {
        keys: "c",
        short: "c",
        help: "pick a color or white: presets, any hue and saturation",
        label: "color",
        hint: Hint::Color,
        rank: 3,
        codes: &[Char('c')],
        action: Action::Color,
    },
    Binding {
        keys: "i",
        short: "i",
        help: "make the device blink for 5 s",
        label: "identify",
        hint: Hint::Identify,
        rank: 5,
        codes: &[Char('i')],
        action: Action::Identify,
    },
    Binding {
        keys: "u",
        short: "u",
        help: "take back the latest change, or a scene while it allows",
        label: "undo",
        hint: Hint::Undo,
        rank: 4,
        codes: &[Char('u')],
        action: Action::Undo,
    },
    Binding {
        keys: "Tab 1 2",
        short: "Tab",
        help: "devices or activity, in a narrow window",
        label: "tabs",
        hint: Hint::Tabs,
        rank: 1,
        codes: &[KeyCode::Tab],
        action: Action::NextTab,
    },
    also(&[Char('1')], Action::Tab(0)),
    also(&[Char('2')], Action::Tab(1)),
    Binding {
        keys: "R",
        short: "R",
        help: "read everything from the hub again",
        label: "resync",
        hint: Hint::Never,
        rank: 8,
        codes: &[Char('R')],
        action: Action::Resync,
    },
    Binding {
        keys: "?",
        short: "?",
        help: "these keys",
        label: "help",
        hint: Hint::Always,
        rank: 1,
        codes: &[Char('?')],
        action: Action::Help,
    },
    Binding {
        keys: "q",
        short: "q",
        help: "quit",
        label: "quit",
        hint: Hint::Always,
        rank: 0,
        codes: &[Char('q')],
        action: Action::Quit,
    },
    also(&[KeyCode::Esc], Action::Back),
];

/// The action for a key press, if it has one. Ctrl-C quits from anywhere, and Shift makes the
/// arrows take the small step.
pub fn action(key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == Char('c')).then_some(Action::Quit);
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Right => return Some(Action::Brighter { fine: true }),
            KeyCode::Left => return Some(Action::Dimmer { fine: true }),
            _ => {}
        }
    }
    BINDINGS
        .iter()
        .find(|binding| binding.codes.contains(&key.code))
        .map(|binding| binding.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_actions() {
        assert_eq!(action(KeyCode::Char('j').into()), Some(Action::Down));
        assert_eq!(action(KeyCode::Char(' ').into()), Some(Action::Activate));
        assert_eq!(action(KeyCode::Enter.into()), Some(Action::Activate));
        assert_eq!(
            action(KeyCode::Right.into()),
            Some(Action::Brighter { fine: false })
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Action::Dimmer { fine: true })
        );
        assert_eq!(
            action(KeyEvent::new(Char('H'), KeyModifiers::SHIFT)),
            Some(Action::Dimmer { fine: true })
        );
        assert_eq!(action(KeyCode::Char(']').into()), Some(Action::Cooler));
        assert_eq!(action(KeyCode::Char('2').into()), Some(Action::Tab(1)));
        assert_eq!(
            action(KeyEvent::new(Char('R'), KeyModifiers::SHIFT)),
            Some(Action::Resync)
        );
        assert_eq!(
            action(KeyEvent::new(Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(
            action(KeyEvent::new(Char('j'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(action(KeyCode::Char('x').into()), None);
    }

    #[test]
    fn every_shown_binding_explains_itself() {
        for binding in BINDINGS.iter().filter(|b| !b.keys.is_empty()) {
            assert!(
                !binding.help.is_empty() && !binding.label.is_empty(),
                "{}",
                binding.keys
            );
        }
    }
}
