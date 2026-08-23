use std::fmt;
use std::str::FromStr;

use gpui::{
    KeyDownEvent, Modifiers as GpuiModifiers, MouseButton, MouseDownEvent, NavigationDirection,
};
use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumMessage};

/// User-rebindable actions. Variants that carry data (e.g. `RunCommand`)
/// render an extra editable field in the keybind settings UI. `EnumIter`
/// drives the variant list in the settings dropdown; payload variants use
/// `Default::default()` as their placeholder there.
///
/// This is the template's demo action set — add your own variants here and a
/// matching arm in `AppView::dispatch`.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default, EnumIter, EnumMessage,
)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    #[strum(message = "Toggle Settings")]
    ToggleSettings,
    #[strum(message = "Close")]
    Close,
    /// Payload-carrying action: the string renders as an inline, editable
    /// field in the keybind settings row. Demonstrates how a bound action can
    /// carry configuration alongside its trigger.
    #[strum(message = "Run Command")]
    RunCommand(String),
}

impl Action {
    /// Display label for the action.
    pub fn label(&self) -> &'static str {
        self.get_message()
            .expect("every Action variant has a strum message")
    }

    /// True if `self` and `other` are the same variant, ignoring payload.
    pub fn same_kind(&self, other: &Action) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BindModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl BindModifiers {
    pub fn from_gpui(m: GpuiModifiers) -> Self {
        Self {
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
            super_key: m.platform,
        }
    }

    pub fn matches(self, m: GpuiModifiers) -> bool {
        self.ctrl == m.control
            && self.alt == m.alt
            && self.shift == m.shift
            && self.super_key == m.platform
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseTrigger {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseTrigger {
    fn from_button(button: MouseButton) -> Self {
        match button {
            MouseButton::Left => Self::Left,
            MouseButton::Right => Self::Right,
            MouseButton::Middle => Self::Middle,
            MouseButton::Navigate(NavigationDirection::Back) => Self::Back,
            MouseButton::Navigate(NavigationDirection::Forward) => Self::Forward,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Left => "Mouse Left",
            Self::Right => "Mouse Right",
            Self::Middle => "Mouse Middle",
            Self::Back => "Mouse Back",
            Self::Forward => "Mouse Forward",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }

    fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "left" => Self::Left,
            "right" => Self::Right,
            "middle" => Self::Middle,
            "back" => Self::Back,
            "forward" => Self::Forward,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Trigger {
    #[default]
    None,
    Key(String),
    Mouse(MouseTrigger),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyBind {
    pub modifiers: BindModifiers,
    pub trigger: Trigger,
}

impl KeyBind {
    pub fn unbound() -> Self {
        Self::default()
    }

    pub fn is_unbound(&self) -> bool {
        matches!(self.trigger, Trigger::None)
    }

    pub fn from_key(event: &KeyDownEvent) -> Self {
        Self {
            modifiers: BindModifiers::from_gpui(event.keystroke.modifiers),
            trigger: Trigger::Key(event.keystroke.key.clone()),
        }
    }

    pub fn from_mouse(event: &MouseDownEvent) -> Self {
        Self {
            modifiers: BindModifiers::from_gpui(event.modifiers),
            trigger: Trigger::Mouse(MouseTrigger::from_button(event.button)),
        }
    }

    pub fn matches_key(&self, event: &KeyDownEvent) -> bool {
        match &self.trigger {
            Trigger::Key(k) => {
                self.modifiers.matches(event.keystroke.modifiers) && &event.keystroke.key == k
            }
            _ => false,
        }
    }

    pub fn matches_mouse(&self, event: &MouseDownEvent) -> bool {
        match self.trigger {
            Trigger::Mouse(m) => {
                MouseTrigger::from_button(event.button) == m
                    && self.modifiers.matches(event.modifiers)
            }
            _ => false,
        }
    }

    /// Serialize as a hyphen-joined token string, matching `FromStr`. An
    /// unbound binding round-trips as `""`.
    pub fn unparse(&self) -> String {
        if self.is_unbound() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.modifiers.ctrl {
            parts.push("ctrl".into());
        }
        if self.modifiers.alt {
            parts.push("alt".into());
        }
        if self.modifiers.shift {
            parts.push("shift".into());
        }
        if self.modifiers.super_key {
            parts.push("super".into());
        }
        match &self.trigger {
            Trigger::None => unreachable!(),
            Trigger::Key(k) => parts.push(k.clone()),
            Trigger::Mouse(t) => {
                parts.push("mouse".into());
                parts.push(t.token().into());
            }
        }
        parts.join("-")
    }
}

impl fmt::Display for KeyBind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unbound() {
            return f.write_str("Unbound");
        }
        let mut parts: Vec<String> = Vec::new();
        if self.modifiers.ctrl {
            parts.push("Ctrl".into());
        }
        if self.modifiers.alt {
            parts.push("Alt".into());
        }
        if self.modifiers.shift {
            parts.push("Shift".into());
        }
        if self.modifiers.super_key {
            parts.push("Super".into());
        }
        match &self.trigger {
            Trigger::None => unreachable!(),
            Trigger::Key(k) => parts.push(pretty_key(k)),
            Trigger::Mouse(t) => parts.push(t.label().into()),
        }
        f.write_str(&parts.join(" + "))
    }
}

fn pretty_key(k: &str) -> String {
    if k.len() == 1 {
        k.to_ascii_uppercase()
    } else {
        let mut chars = k.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_ascii_uppercase(), chars.as_str()),
            None => String::new(),
        }
    }
}

impl FromStr for KeyBind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(Self::unbound());
        }
        let mut tokens: Vec<&str> = s.split('-').collect();
        let mut mods = BindModifiers::default();
        while let Some(&first) = tokens.first() {
            match first {
                "ctrl" => {
                    mods.ctrl = true;
                    tokens.remove(0);
                }
                "alt" => {
                    mods.alt = true;
                    tokens.remove(0);
                }
                "shift" => {
                    mods.shift = true;
                    tokens.remove(0);
                }
                "super" => {
                    mods.super_key = true;
                    tokens.remove(0);
                }
                _ => break,
            }
        }
        let trigger = match tokens.as_slice() {
            [] => return Err(format!("empty keybind: {s}")),
            ["mouse", btn] => Trigger::Mouse(
                MouseTrigger::from_token(btn)
                    .ok_or_else(|| format!("unknown mouse button: {btn}"))?,
            ),
            [key] => Trigger::Key((*key).to_string()),
            _ => return Err(format!("unrecognized keybind: {s}")),
        };
        Ok(Self {
            modifiers: mods,
            trigger,
        })
    }
}

impl Serialize for KeyBind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.unparse())
    }
}

impl<'de> Deserialize<'de> for KeyBind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        KeyBind::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// A single action ↔ trigger pair. Multiple `Binding`s may share the same
/// `action` (multiple keys/buttons for one action) or the same `bind` (one
/// key shouldn't, since dispatch picks the first match).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Binding {
    pub action: Action,
    pub bind: KeyBind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybinds {
    pub bindings: Vec<Binding>,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
            bindings: vec![
                Binding {
                    action: Action::ToggleSettings,
                    bind: KeyBind {
                        modifiers: BindModifiers {
                            ctrl: true,
                            ..Default::default()
                        },
                        trigger: Trigger::Key(",".into()),
                    },
                },
                Binding {
                    action: Action::Close,
                    bind: KeyBind {
                        modifiers: BindModifiers {
                            ctrl: true,
                            ..Default::default()
                        },
                        trigger: Trigger::Key("q".into()),
                    },
                },
                Binding {
                    action: Action::RunCommand("echo hello".into()),
                    bind: KeyBind {
                        modifiers: BindModifiers {
                            ctrl: true,
                            ..Default::default()
                        },
                        trigger: Trigger::Key("r".into()),
                    },
                },
            ],
        }
    }
}

impl Keybinds {
    pub fn match_key(&self, event: &KeyDownEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|b| b.bind.matches_key(event))
            .map(|b| b.action.clone())
    }

    pub fn match_mouse(&self, event: &MouseDownEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|b| b.bind.matches_mouse(event))
            .map(|b| b.action.clone())
    }
}

/// True if a key event is the "unbind & exit recording" gesture (Ctrl+Esc).
pub fn is_unbind_gesture(event: &KeyDownEvent) -> bool {
    let m = event.keystroke.modifiers;
    event.keystroke.key == "escape" && m.control && !m.alt && !m.shift && !m.platform
}

/// True if a key event is the "cancel recording" gesture (bare Esc).
pub fn is_cancel_gesture(event: &KeyDownEvent) -> bool {
    !event.keystroke.modifiers.modified() && event.keystroke.key == "escape"
}
