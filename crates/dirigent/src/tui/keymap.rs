//! The TUI's keys: one flat table that also drives the help overlay and the hint bar.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    NextTab,
    /// Jump to the tab with this index.
    Tab(usize),
    Resync,
    Help,
    Quit,
    /// Enter: what a dialog offers.
    Confirm,
    /// Escape: close or cancel.
    Back,
}

/// When a binding is shown in the hint bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Always,
    /// Only in the narrow layout, where the panes are tabs.
    Tabs,
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
        rank: 2,
        codes: &[KeyCode::Up, Char('k')],
        action: Action::Up,
    },
    also(&[KeyCode::Down, Char('j')], Action::Down),
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
        hint: Hint::Always,
        rank: 4,
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
    also(&[KeyCode::Enter], Action::Confirm),
    also(&[KeyCode::Esc], Action::Back),
];

/// The action for a key press, if it has one. Ctrl-C quits from anywhere.
pub fn action(key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == Char('c')).then_some(Action::Quit);
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
        assert_eq!(action(KeyCode::Up.into()), Some(Action::Up));
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
        for binding in BINDINGS.iter().filter(|b| b.hint != Hint::Never) {
            assert!(
                !binding.keys.is_empty() && !binding.help.is_empty() && !binding.label.is_empty()
            );
        }
    }
}
