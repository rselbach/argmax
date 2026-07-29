//! Parsing and validation for the two configurable terminal bindings.
//!
//! The parity-release grammar is deliberately finite:
//!
//! - `ctrl+space` (also spelled `ctrl+@`);
//! - `ctrl+r`; and
//! - `shift+tab`.
//!
//! Names are ASCII case-insensitive and are formatted canonically for footer
//! hints. Other ASCII control names and common editing keys are recognized only
//! so validation can explain that they remain fixed. Bindings are compared by
//! their terminal bytes, rather than by spelling, to catch aliases and prefix
//! ambiguity before a session starts.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// Dotted configuration path for the mode-switch binding.
pub const TOGGLE_MODE_FIELD: &str = "keybindings.toggle-mode";
/// Dotted configuration path for the menu-toggle binding.
pub const TOGGLE_MENU_FIELD: &str = "keybindings.toggle-menu";

const ESCAPE: u8 = 0x1b;
const MAX_BINDING_NAME_BYTES: usize = 32;

/// One configurable action recognized by the terminal input layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KeybindingAction {
    /// Switch between specification and history suggestion modes.
    ToggleMode,
    /// Show or hide the suggestion layer for the current session.
    ToggleMenu,
}

impl KeybindingAction {
    /// Returns the responsible dotted configuration path.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::ToggleMode => TOGGLE_MODE_FIELD,
            Self::ToggleMenu => TOGGLE_MENU_FIELD,
        }
    }
}

/// A validated binding with a canonical footer label and terminal byte sequence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyBinding {
    canonical_name: String,
    sequence: Vec<u8>,
}

impl KeyBinding {
    /// Parses one configurable key name.
    ///
    /// # Errors
    ///
    /// Returns a structured error for malformed, unsupported, or fixed controls.
    pub fn parse(name: &str) -> Result<Self, BindingParseError> {
        let candidate = parse_candidate(name)?;
        if let Some(control) = candidate.fixed_control {
            return Err(BindingParseError::new(BindingParseProblem::FixedControl {
                control,
            }));
        }
        Ok(candidate.binding)
    }

    /// Canonical key name suitable for a footer hint.
    #[must_use]
    pub fn canonical_name(&self) -> &str {
        &self.canonical_name
    }

    /// Exact bytes recognized by the terminal input layer.
    #[must_use]
    pub fn sequence(&self) -> &[u8] {
        &self.sequence
    }
}

impl FromStr for KeyBinding {
    type Err = BindingParseError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name)
    }
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical_name)
    }
}

/// The two bindings after syntax, fixed-control, duplicate, and prefix checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedKeybindings {
    toggle_mode: KeyBinding,
    toggle_menu: KeyBinding,
}

impl ResolvedKeybindings {
    /// Resolves both configurable bindings atomically.
    ///
    /// Validation collects independent field failures. A byte-identical alias or
    /// a sequence-prefix relationship is reported against the menu field and
    /// names the mode field as the other responsible key.
    ///
    /// # Errors
    ///
    /// Returns all binding validation errors found in the proposed pair.
    pub fn resolve(mode_name: &str, menu_name: &str) -> Result<Self, KeybindingValidationErrors> {
        let mut errors = Vec::new();
        let mode = validate_field(mode_name, KeybindingAction::ToggleMode, &mut errors);
        let menu = validate_field(menu_name, KeybindingAction::ToggleMenu, &mut errors);

        if let (Some(mode_candidate), Some(menu_candidate)) = (&mode, &menu) {
            let mode_bytes = mode_candidate.binding.sequence();
            let menu_bytes = menu_candidate.binding.sequence();
            let problem = if mode_bytes == menu_bytes {
                Some(KeybindingValidationProblem::Duplicate {
                    other_field: KeybindingAction::ToggleMode,
                })
            } else if is_strict_prefix(mode_bytes, menu_bytes)
                || is_strict_prefix(menu_bytes, mode_bytes)
            {
                Some(KeybindingValidationProblem::PrefixConflict {
                    other_field: KeybindingAction::ToggleMode,
                })
            } else {
                None
            };

            if let Some(problem) = problem {
                errors.push(KeybindingValidationError::new(
                    KeybindingAction::ToggleMenu,
                    problem,
                ));
            }
        }

        if errors.is_empty() {
            let (Some(mode), Some(menu)) = (mode, menu) else {
                return Err(KeybindingValidationErrors::new(errors));
            };
            return Ok(Self {
                toggle_mode: mode.binding,
                toggle_menu: menu.binding,
            });
        }

        Err(KeybindingValidationErrors::new(errors))
    }

    /// Returns the binding assigned to an action.
    #[must_use]
    pub const fn binding(&self, action: KeybindingAction) -> &KeyBinding {
        match action {
            KeybindingAction::ToggleMode => &self.toggle_mode,
            KeybindingAction::ToggleMenu => &self.toggle_menu,
        }
    }

    /// Returns the canonical footer hint for an action.
    #[must_use]
    pub fn footer_hint(&self, action: KeybindingAction) -> &str {
        self.binding(action).canonical_name()
    }
}

/// Why one key name could not become a configurable binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingParseProblem {
    /// The value contains no non-whitespace text.
    Blank,
    /// The value does not follow the finite binding-name grammar.
    InvalidSyntax,
    /// The name is syntactically plausible but not supported by this release.
    Unsupported,
    /// The key belongs to terminal or shell behavior that must remain fixed.
    FixedControl {
        /// Class of protected behavior.
        control: FixedControl,
    },
}

impl fmt::Display for BindingParseProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => formatter.write_str("binding must not be blank"),
            Self::InvalidSyntax => formatter.write_str("invalid keybinding syntax"),
            Self::Unsupported => formatter.write_str("unsupported keybinding"),
            Self::FixedControl { control } => {
                write!(
                    formatter,
                    "{control} remains fixed and cannot be reassigned"
                )
            }
        }
    }
}

/// Category of terminal behavior unavailable to configurable actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedControl {
    /// Escape hides transient UI state and restores safe pass-through behavior.
    Escape,
    /// Tab accepts or reveals a completion and otherwise belongs to the shell.
    Tab,
    /// Enter executes exactly the current shell buffer.
    Enter,
    /// Cursor and history navigation remain shell-compatible.
    CursorMovement,
    /// Shell line-editing commands remain unchanged.
    LineEditing,
}

impl fmt::Display for FixedControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Escape => "Escape",
            Self::Tab => "Tab",
            Self::Enter => "Enter",
            Self::CursorMovement => "cursor movement",
            Self::LineEditing => "line editing",
        })
    }
}

/// Error returned while parsing one binding without configuration-field context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingParseError {
    problem: BindingParseProblem,
}

impl BindingParseError {
    const fn new(problem: BindingParseProblem) -> Self {
        Self { problem }
    }

    /// Returns the structured reason parsing failed.
    #[must_use]
    pub const fn problem(self) -> BindingParseProblem {
        self.problem
    }
}

impl fmt::Display for BindingParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.problem.fmt(formatter)
    }
}

impl Error for BindingParseError {}

/// Pair-level validation failure tied to a responsible configuration field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeybindingValidationError {
    field: KeybindingAction,
    problem: KeybindingValidationProblem,
}

impl KeybindingValidationError {
    const fn new(field: KeybindingAction, problem: KeybindingValidationProblem) -> Self {
        Self { field, problem }
    }

    /// Returns the responsible binding field.
    #[must_use]
    pub const fn field(&self) -> KeybindingAction {
        self.field
    }

    /// Returns the dotted path of the responsible binding field.
    #[must_use]
    pub const fn field_path(&self) -> &'static str {
        self.field.field()
    }

    /// Returns the structured validation problem.
    #[must_use]
    pub const fn problem(&self) -> KeybindingValidationProblem {
        self.problem
    }
}

impl fmt::Display for KeybindingValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field.field(), self.problem)
    }
}

impl fmt::Display for KeybindingValidationProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => formatter.write_str("binding must not be blank"),
            Self::InvalidSyntax => formatter.write_str("invalid keybinding syntax"),
            Self::Unsupported => formatter.write_str("unsupported keybinding"),
            Self::FixedControl { control } => {
                write!(
                    formatter,
                    "{control} remains fixed and cannot be reassigned"
                )
            }
            Self::Duplicate { other_field } => {
                write!(formatter, "duplicates {}", other_field.field())
            }
            Self::PrefixConflict { other_field } => write!(
                formatter,
                "terminal sequence has a prefix conflict with {}",
                other_field.field()
            ),
        }
    }
}

/// Structured reason a proposed binding pair is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeybindingValidationProblem {
    /// The value contains no non-whitespace text.
    Blank,
    /// The value does not follow the finite binding-name grammar.
    InvalidSyntax,
    /// The value names a key this release does not support.
    Unsupported,
    /// The key belongs to terminal or shell behavior that must remain fixed.
    FixedControl {
        /// Class of protected behavior.
        control: FixedControl,
    },
    /// The terminal bytes are already assigned to the other field.
    Duplicate {
        /// Other responsible binding field.
        other_field: KeybindingAction,
    },
    /// One terminal byte sequence is a strict prefix of the other.
    PrefixConflict {
        /// Other responsible binding field.
        other_field: KeybindingAction,
    },
}

impl From<BindingParseProblem> for KeybindingValidationProblem {
    fn from(problem: BindingParseProblem) -> Self {
        match problem {
            BindingParseProblem::Blank => Self::Blank,
            BindingParseProblem::InvalidSyntax => Self::InvalidSyntax,
            BindingParseProblem::Unsupported => Self::Unsupported,
            BindingParseProblem::FixedControl { control } => Self::FixedControl { control },
        }
    }
}

/// Complete set of invalid binding fields in one proposed configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeybindingValidationErrors {
    errors: Vec<KeybindingValidationError>,
}

impl KeybindingValidationErrors {
    fn new(errors: Vec<KeybindingValidationError>) -> Self {
        Self { errors }
    }

    /// Returns the individual field-specific failures.
    #[must_use]
    pub fn errors(&self) -> &[KeybindingValidationError] {
        &self.errors
    }

    /// Consumes the collection and returns its individual failures.
    #[must_use]
    pub fn into_errors(self) -> Vec<KeybindingValidationError> {
        self.errors
    }
}

impl fmt::Display for KeybindingValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            error.fmt(formatter)?;
        }
        Ok(())
    }
}

impl Error for KeybindingValidationErrors {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BindingCandidate {
    binding: KeyBinding,
    fixed_control: Option<FixedControl>,
}

fn validate_field(
    name: &str,
    field: KeybindingAction,
    errors: &mut Vec<KeybindingValidationError>,
) -> Option<BindingCandidate> {
    match parse_candidate(name) {
        Ok(candidate) => {
            if let Some(control) = candidate.fixed_control {
                errors.push(KeybindingValidationError::new(
                    field,
                    KeybindingValidationProblem::FixedControl { control },
                ));
            }
            Some(candidate)
        }
        Err(error) => {
            errors.push(KeybindingValidationError::new(
                field,
                error.problem().into(),
            ));
            None
        }
    }
}

fn parse_candidate(name: &str) -> Result<BindingCandidate, BindingParseError> {
    if name.len() > MAX_BINDING_NAME_BYTES {
        return Err(BindingParseError::new(BindingParseProblem::InvalidSyntax));
    }
    if name.trim().is_empty() {
        return Err(BindingParseError::new(BindingParseProblem::Blank));
    }
    if name.trim() != name || name.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(BindingParseError::new(BindingParseProblem::InvalidSyntax));
    }
    if !name.is_ascii() {
        return Err(BindingParseError::new(BindingParseProblem::Unsupported));
    }

    let normalized = name.to_ascii_lowercase();
    if let Some(candidate) = parse_named_key(&normalized) {
        return Ok(candidate);
    }
    if let Some(operand) = normalized.strip_prefix("ctrl+") {
        return parse_control(operand);
    }
    let problem = if normalized.ends_with('+') || normalized.matches('+').count() > 1 {
        BindingParseProblem::InvalidSyntax
    } else {
        BindingParseProblem::Unsupported
    };
    Err(BindingParseError::new(problem))
}

fn parse_named_key(name: &str) -> Option<BindingCandidate> {
    let (canonical_name, sequence, fixed_control) = match name {
        "shift+tab" => ("shift+tab", &b"\x1b[Z"[..], None),
        "escape" => ("escape", &b"\x1b"[..], Some(FixedControl::Escape)),
        "tab" => ("tab", &b"\t"[..], Some(FixedControl::Tab)),
        "enter" => ("enter", &b"\r"[..], Some(FixedControl::Enter)),
        "up" => ("up", &b"\x1b[A"[..], Some(FixedControl::CursorMovement)),
        "down" => ("down", &b"\x1b[B"[..], Some(FixedControl::CursorMovement)),
        "right" => ("right", &b"\x1b[C"[..], Some(FixedControl::CursorMovement)),
        "left" => ("left", &b"\x1b[D"[..], Some(FixedControl::CursorMovement)),
        "home" => ("home", &b"\x1b[H"[..], Some(FixedControl::CursorMovement)),
        "end" => ("end", &b"\x1b[F"[..], Some(FixedControl::CursorMovement)),
        "backspace" => ("backspace", &b"\x7f"[..], Some(FixedControl::LineEditing)),
        "delete" => ("delete", &b"\x1b[3~"[..], Some(FixedControl::LineEditing)),
        _ => return None,
    };
    Some(BindingCandidate {
        binding: KeyBinding {
            canonical_name: canonical_name.to_owned(),
            sequence: sequence.to_vec(),
        },
        fixed_control,
    })
}

fn parse_control(operand: &str) -> Result<BindingCandidate, BindingParseError> {
    let (canonical_name, byte) = match operand {
        "space" | "@" => ("ctrl+space".to_owned(), 0),
        value if value.len() == 1 => {
            let byte = value.as_bytes()[0];
            let control = match byte {
                b'a'..=b'z' => byte - b'a' + 1,
                b'[' => ESCAPE,
                b'\\' => 0x1c,
                b']' => 0x1d,
                b'^' => 0x1e,
                b'_' => 0x1f,
                b'?' => 0x7f,
                _ => {
                    return Err(BindingParseError::new(BindingParseProblem::Unsupported));
                }
            };
            (format!("ctrl+{value}"), control)
        }
        "" => {
            return Err(BindingParseError::new(BindingParseProblem::InvalidSyntax));
        }
        _ => return Err(BindingParseError::new(BindingParseProblem::Unsupported)),
    };

    let fixed_control = match byte {
        0 | 0x12 => None,
        ESCAPE => Some(FixedControl::Escape),
        b'\t' => Some(FixedControl::Tab),
        b'\n' | b'\r' => Some(FixedControl::Enter),
        0x02 | 0x06 | 0x0e | 0x10 => Some(FixedControl::CursorMovement),
        _ => Some(FixedControl::LineEditing),
    };

    Ok(BindingCandidate {
        binding: KeyBinding {
            canonical_name,
            sequence: vec![byte],
        },
        fixed_control,
    })
}

fn is_strict_prefix(first: &[u8], second: &[u8]) -> bool {
    first.len() < second.len() && second.starts_with(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_to_expected_bytes_and_footer_hints() {
        let bindings = ResolvedKeybindings::resolve("ctrl+r", "shift+tab").unwrap();

        assert_eq!(
            bindings.binding(KeybindingAction::ToggleMode).sequence(),
            b"\x12"
        );
        assert_eq!(
            bindings.binding(KeybindingAction::ToggleMenu).sequence(),
            b"\x1b[Z"
        );
        assert_eq!(bindings.footer_hint(KeybindingAction::ToggleMode), "ctrl+r");
        assert_eq!(
            bindings.footer_hint(KeybindingAction::ToggleMenu),
            "shift+tab"
        );
    }

    #[test]
    fn names_and_aliases_are_canonicalized_deterministically() {
        let cases = [
            ("CTRL+@", "ctrl+space", &b"\0"[..]),
            ("Ctrl+Space", "ctrl+space", &b"\0"[..]),
            ("SHIFT+TAB", "shift+tab", &b"\x1b[Z"[..]),
        ];

        for (name, want_name, want_sequence) in cases {
            let binding = KeyBinding::parse(name).unwrap();
            assert_eq!(binding.canonical_name(), want_name, "{name}");
            assert_eq!(binding.sequence(), want_sequence, "{name}");
            assert_eq!(binding.to_string(), want_name, "{name}");
        }
    }

    #[test]
    fn rejects_blank_malformed_and_unsupported_names() {
        let cases = [
            ("", BindingParseProblem::Blank),
            ("  ", BindingParseProblem::Blank),
            (" ctrl+r", BindingParseProblem::InvalidSyntax),
            ("ctrl+r ", BindingParseProblem::InvalidSyntax),
            ("ctrl+", BindingParseProblem::InvalidSyntax),
            ("alt+a", BindingParseProblem::Unsupported),
            ("alt+++", BindingParseProblem::InvalidSyntax),
            ("ctrl+rr", BindingParseProblem::Unsupported),
            ("ctrl+é", BindingParseProblem::Unsupported),
            ("shift+r", BindingParseProblem::Unsupported),
            ("hyper+r", BindingParseProblem::Unsupported),
            (
                "ctrl+this-name-is-far-too-long-to-be-a-keybinding",
                BindingParseProblem::InvalidSyntax,
            ),
            (
                "                                                                ",
                BindingParseProblem::InvalidSyntax,
            ),
        ];

        for (name, want) in cases {
            let error = KeyBinding::parse(name).unwrap_err();
            assert_eq!(error.problem(), want, "{name}");
        }
    }

    #[test]
    fn fixed_controls_cannot_be_assigned() {
        let cases = [
            ("escape", FixedControl::Escape),
            ("ctrl+[", FixedControl::Escape),
            ("tab", FixedControl::Tab),
            ("ctrl+i", FixedControl::Tab),
            ("enter", FixedControl::Enter),
            ("ctrl+j", FixedControl::Enter),
            ("ctrl+m", FixedControl::Enter),
            ("left", FixedControl::CursorMovement),
            ("ctrl+b", FixedControl::CursorMovement),
            ("backspace", FixedControl::LineEditing),
            ("ctrl+a", FixedControl::LineEditing),
            ("ctrl+c", FixedControl::LineEditing),
            ("ctrl+e", FixedControl::LineEditing),
            ("ctrl+l", FixedControl::LineEditing),
            ("ctrl+u", FixedControl::LineEditing),
            ("ctrl+w", FixedControl::LineEditing),
        ];

        for (name, want) in cases {
            let error = KeyBinding::parse(name).unwrap_err();
            assert_eq!(
                error.problem(),
                BindingParseProblem::FixedControl { control: want },
                "{name}"
            );
        }
    }

    #[test]
    fn encoded_aliases_are_duplicate_even_when_text_differs() {
        let errors = ResolvedKeybindings::resolve("ctrl+space", "ctrl+@").unwrap_err();

        assert!(errors.errors().iter().any(|error| {
            error.field() == KeybindingAction::ToggleMenu
                && error.problem()
                    == KeybindingValidationProblem::Duplicate {
                        other_field: KeybindingAction::ToggleMode,
                    }
        }));
    }

    #[test]
    fn byte_prefix_conflicts_name_both_responsible_fields() {
        for (mode, menu) in [("escape", "shift+tab"), ("shift+tab", "escape")] {
            let errors = ResolvedKeybindings::resolve(mode, menu).unwrap_err();
            let prefix = errors
                .errors()
                .iter()
                .find(|error| {
                    matches!(
                        error.problem(),
                        KeybindingValidationProblem::PrefixConflict { .. }
                    )
                })
                .unwrap();

            assert_eq!(prefix.field_path(), TOGGLE_MENU_FIELD);
            assert!(prefix.to_string().contains(TOGGLE_MODE_FIELD));
            assert!(prefix.to_string().contains(TOGGLE_MENU_FIELD));
        }
    }

    #[test]
    fn independent_field_errors_are_collected() {
        let errors = ResolvedKeybindings::resolve("tab", "ctrl+rr").unwrap_err();

        assert_eq!(errors.errors().len(), 2);
        assert_eq!(errors.errors()[0].field_path(), TOGGLE_MODE_FIELD);
        assert_eq!(
            errors.errors()[0].problem(),
            KeybindingValidationProblem::FixedControl {
                control: FixedControl::Tab,
            }
        );
        assert_eq!(errors.errors()[1].field_path(), TOGGLE_MENU_FIELD);
        assert_eq!(
            errors.errors()[1].problem(),
            KeybindingValidationProblem::Unsupported
        );
    }

    #[test]
    fn fixed_and_prefix_failures_are_both_visible() {
        let errors = ResolvedKeybindings::resolve("escape", "shift+tab").unwrap_err();

        assert!(errors.errors().iter().any(|error| {
            error.field() == KeybindingAction::ToggleMode
                && error.problem()
                    == KeybindingValidationProblem::FixedControl {
                        control: FixedControl::Escape,
                    }
        }));
        assert!(errors.errors().iter().any(|error| {
            error.field() == KeybindingAction::ToggleMenu
                && error.problem()
                    == KeybindingValidationProblem::PrefixConflict {
                        other_field: KeybindingAction::ToggleMode,
                    }
        }));
    }

    #[test]
    fn all_configurable_control_names_have_stable_encodings() {
        let cases = [
            ("ctrl+space", &b"\0"[..]),
            ("ctrl+@", &b"\0"[..]),
            ("ctrl+r", &b"\x12"[..]),
        ];

        for (name, want) in cases {
            assert_eq!(KeyBinding::parse(name).unwrap().sequence(), want, "{name}");
        }
    }
}
