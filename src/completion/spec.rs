use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::ops::Range;

use super::{
    CompletionQuery, GeneratorSpec, GeneratorTarget, InsertionBehavior, QuoteKind, ShellToken,
    Suggestion, SuggestionSource, TextEdit, TokenizedLine, tokenize,
};

/// One recursively nested command in a curated completion specification.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandSpec {
    /// Canonical command or subcommand name.
    pub name: String,
    /// Exact alternative names accepted while traversing the tree.
    pub aliases: Vec<String>,
    /// Short user-facing description.
    pub description: String,
    /// Child commands available at this node.
    pub subcommands: Vec<CommandSpec>,
    /// Flags and options accepted at this node.
    pub options: Vec<OptionSpec>,
    /// Bounded dynamic values declared at argument locations on this node.
    pub generators: Vec<GeneratorSpec>,
    /// Maximum positional values accepted by this node, or no fixed maximum.
    pub max_positionals: Option<usize>,
    /// Static ranking priority in the inclusive range zero to one.
    pub priority: f64,
    /// Exact spacing behavior after insertion.
    pub insertion: InsertionBehavior,
}

impl CommandSpec {
    /// Creates a command with no aliases, children, options, or positional cap.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            description: description.into(),
            subcommands: Vec::new(),
            options: Vec::new(),
            generators: Vec::new(),
            max_positionals: None,
            priority: 0.5,
            insertion: InsertionBehavior::AppendSpace,
        }
    }

    /// Adds an exact command alias.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Adds a recursively nested subcommand.
    #[must_use]
    pub fn with_subcommand(mut self, subcommand: Self) -> Self {
        self.subcommands.push(subcommand);
        self
    }

    /// Adds a flag or option.
    #[must_use]
    pub fn with_option(mut self, option: OptionSpec) -> Self {
        self.options.push(option);
        self
    }

    /// Adds a bounded dynamic value declaration.
    #[must_use]
    pub fn with_generator(mut self, generator: GeneratorSpec) -> Self {
        self.generators.push(generator);
        self
    }

    /// Sets the maximum number of positional values accepted at this node.
    #[must_use]
    pub const fn with_max_positionals(mut self, maximum: usize) -> Self {
        self.max_positionals = Some(maximum);
        self
    }

    /// Sets the static ranking priority.
    #[must_use]
    pub const fn with_priority(mut self, priority: f64) -> Self {
        self.priority = priority;
        self
    }

    /// Sets the exact insertion behavior for this command.
    #[must_use]
    pub const fn with_insertion(mut self, insertion: InsertionBehavior) -> Self {
        self.insertion = insertion;
        self
    }

    /// Validates this command and all descendants.
    ///
    /// # Errors
    ///
    /// Returns the first invalid name, duplicate, inherited-option conflict, or
    /// non-finite/out-of-range priority with its command path.
    pub fn validate(&self) -> Result<(), SpecError> {
        validate_command(self, "", &BTreeMap::new())
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// One flag or value-taking option in a command specification.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionSpec {
    /// Canonical spelling, including its leading dash or dashes.
    pub name: String,
    /// Alternative spellings for the same option.
    pub aliases: Vec<String>,
    /// Short user-facing description.
    pub description: String,
    /// Whether a following token (or `=value`) belongs to this option.
    pub takes_value: bool,
    /// Whether the option may be suggested after it has already appeared.
    pub repeatable: bool,
    /// Whether descendants inherit this option.
    pub global: bool,
    /// Static ranking priority in the inclusive range zero to one.
    pub priority: f64,
    /// Exact spacing behavior after insertion.
    pub insertion: InsertionBehavior,
}

impl OptionSpec {
    /// Creates a non-repeatable flag local to its command node.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            description: description.into(),
            takes_value: false,
            repeatable: false,
            global: false,
            priority: 0.5,
            insertion: InsertionBehavior::AppendSpace,
        }
    }

    /// Adds another spelling for this option.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Sets whether this option consumes a value.
    #[must_use]
    pub const fn takes_value(mut self, takes_value: bool) -> Self {
        self.takes_value = takes_value;
        self
    }

    /// Sets whether this option may occur more than once.
    #[must_use]
    pub const fn repeatable(mut self, repeatable: bool) -> Self {
        self.repeatable = repeatable;
        self
    }

    /// Sets whether descendants inherit this option.
    #[must_use]
    pub const fn global(mut self, global: bool) -> Self {
        self.global = global;
        self
    }

    /// Sets the static ranking priority.
    #[must_use]
    pub const fn with_priority(mut self, priority: f64) -> Self {
        self.priority = priority;
        self
    }

    /// Sets the exact insertion behavior for this option.
    #[must_use]
    pub const fn with_insertion(mut self, insertion: InsertionBehavior) -> Self {
        self.insertion = insertion;
        self
    }

    /// Returns all accepted spellings, canonical first.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// Validation failure for one command definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecError {
    /// Canonical command path containing the failure.
    pub path: String,
    /// Human-readable reason suitable for catalog diagnostics.
    pub message: String,
}

impl SpecError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

impl Error for SpecError {}

/// Validated top-level command specifications indexed by exact names and aliases.
#[derive(Clone, Debug)]
pub struct SpecIndex {
    roots: Vec<CommandSpec>,
    root_names: BTreeMap<String, usize>,
}

impl SpecIndex {
    /// Validates and indexes root command definitions.
    ///
    /// # Errors
    ///
    /// Returns the first malformed definition or conflicting root name/alias.
    pub fn new(roots: impl IntoIterator<Item = CommandSpec>) -> Result<Self, SpecError> {
        let roots = roots.into_iter().collect::<Vec<_>>();
        let mut root_names = BTreeMap::new();

        for (index, root) in roots.iter().enumerate() {
            root.validate()?;
            for name in root.names() {
                if let Some(previous) = root_names.insert(name.to_lowercase(), index) {
                    return Err(SpecError::new(
                        root.name.clone(),
                        format!(
                            "root name or alias {name:?} conflicts with {}",
                            roots[previous].name
                        ),
                    ));
                }
            }
        }

        Ok(Self { roots, root_names })
    }

    /// Returns the validated root definitions in declaration order.
    #[must_use]
    pub fn roots(&self) -> &[CommandSpec] {
        &self.roots
    }

    /// Resolves a submitted command to its canonical command/subcommand path.
    ///
    /// Flags, their declared values, positionals, and leading shell environment
    /// assignments are excluded. An unknown executable falls back to its cooked
    /// first-token name.
    #[must_use]
    pub fn command_skeleton(&self, command: &str) -> Option<String> {
        let mut buffer = command.to_owned();
        if !buffer.chars().last().is_some_and(char::is_whitespace) {
            buffer.push(' ');
        }
        let parsed = tokenize(&buffer, buffer.len()).ok()?;
        let root = parsed
            .committed_tokens()
            .iter()
            .find(|token| !is_shell_assignment(&token.cooked))?;
        let command = &buffer[root.raw.start..];
        let parsed = tokenize(command, command.len()).ok()?;
        self.resolve(&parsed)
            .map(|resolution| resolution.path.join(" "))
            .or_else(|| {
                parsed
                    .committed_tokens()
                    .first()
                    // An unresolved token containing an equals sign is most
                    // likely an assignment shape the strict predicate did not
                    // recognize; its value must not become a ranking key.
                    .filter(|token| !token.cooked.contains('='))
                    .map(|token| token.cooked.clone())
            })
    }

    /// Resolves committed tokens to the deepest exact command node.
    ///
    /// The final active token is retained as a partial value. Unknown positional
    /// arguments disable deeper subcommand traversal but keep the current node so
    /// generators can inspect its positional state.
    #[must_use]
    pub fn resolve<'a>(&'a self, line: &TokenizedLine) -> Option<SpecResolution<'a>> {
        let committed = line.committed_tokens();
        let root_token = committed.first()?;
        let root_index = *self.root_names.get(&root_token.cooked.to_lowercase())?;
        let mut node = &self.roots[root_index];
        let mut path = vec![node.name.clone()];
        let mut inherited_options = Vec::new();
        let mut inherited_generators = Vec::new();
        let mut used_options = BTreeSet::new();
        let mut positional_count = 0;
        let mut subcommands_allowed = true;
        let mut options_ended = false;
        let mut awaiting_option = None;

        for token in &committed[1..] {
            if awaiting_option.take().is_some() {
                continue;
            }

            if !options_ended {
                if token.cooked == "--" {
                    options_ended = true;
                    subcommands_allowed = false;
                    continue;
                }

                if token.cooked.starts_with('-') {
                    let (option_name, attached_value) = split_option_value(&token.cooked);
                    if let Some(option) = find_option(node, &inherited_options, option_name) {
                        used_options.insert(option.name.clone());
                        if option.takes_value && !attached_value {
                            awaiting_option = Some(option);
                        }
                    } else {
                        // Its value arity is unknown, so treating a later token as
                        // a subcommand could fabricate a deeper traversal.
                        subcommands_allowed = false;
                    }
                    // Unknown dash-prefixed tokens are still flags, never subcommands.
                    continue;
                }
            }

            if subcommands_allowed {
                if let Some(child) = find_subcommand(node, &token.cooked) {
                    inherited_generators.extend(node.generators.iter().filter(|generator| {
                        let GeneratorTarget::OptionValue(name) = &generator.target else {
                            return false;
                        };
                        node.options.iter().any(|option| {
                            option.global && option.names().any(|candidate| candidate == name)
                        })
                    }));
                    inherited_options.extend(node.options.iter().filter(|option| option.global));
                    node = child;
                    path.push(node.name.clone());
                    continue;
                }
                subcommands_allowed = false;
            }

            positional_count += 1;
        }

        let active = line.active_token();
        let mut partial = active.cooked.clone();
        let mut replacement = active.raw.clone();

        if awaiting_option.is_none() && !options_ended && partial.starts_with('-') {
            let (option_name, attached_value) = split_option_value(&partial);
            if attached_value {
                if let Some(option) = find_option(node, &inherited_options, option_name) {
                    if option.takes_value {
                        awaiting_option = Some(option);
                        used_options.insert(option.name.clone());
                        let equals = line
                            .line
                            .get(active.raw.clone())
                            .and_then(|raw| raw.find('='))
                            .map_or(active.raw.end, |offset| active.raw.start + offset + 1);
                        replacement = equals..active.raw.end;
                        partial = partial
                            .split_once('=')
                            .map_or_else(String::new, |(_, value)| value.to_string());
                    }
                }
            }
        }

        let can_accept_positional = node
            .max_positionals
            .is_none_or(|maximum| positional_count < maximum);

        Some(SpecResolution {
            node,
            path,
            partial,
            replacement,
            quote: active.quote,
            positional_count,
            positional_index: positional_count,
            can_accept_positional,
            subcommands_allowed,
            options_ended,
            used_options,
            awaiting_option,
            inherited_options,
            inherited_generators,
        })
    }

    /// Produces inert static spec suggestions for a query.
    #[must_use]
    pub fn suggestions(&self, query: &CompletionQuery) -> Vec<Suggestion> {
        let Ok(line) = tokenize(&query.line, query.cursor) else {
            return Vec::new();
        };
        let active = line.active_token();
        let full_active = line.full_active_token();

        if line.committed_tokens().is_empty() {
            return root_suggestions(&self.roots, active, &full_active, &query.line, query.cursor);
        }

        let Some(resolution) = self.resolve(&line) else {
            return Vec::new();
        };
        if resolution.awaiting_option.is_some() {
            return Vec::new();
        }

        node_suggestions(&resolution, active, &full_active, &query.line, query.cursor)
    }

    /// Alias for [`Self::suggestions`] for provider-style callers.
    #[must_use]
    pub fn complete(&self, query: &CompletionQuery) -> Vec<Suggestion> {
        self.suggestions(query)
    }
}

/// Inspectable result of traversing a tokenized line through a spec tree.
#[derive(Clone, Debug)]
pub struct SpecResolution<'a> {
    /// Deepest exactly resolved command node.
    pub node: &'a CommandSpec,
    /// Canonical root-to-node command path.
    pub path: Vec<String>,
    /// Cooked active token, or an attached option's partial value.
    pub partial: String,
    /// Byte range a value provider should replace without touching the suffix.
    pub replacement: Range<usize>,
    /// Quote style of the original active token.
    pub quote: QuoteKind,
    /// Number of committed positional arguments at this node.
    pub positional_count: usize,
    /// Zero-based position currently being completed.
    pub positional_index: usize,
    /// Whether the positional maximum permits another value.
    pub can_accept_positional: bool,
    /// Whether an exact active/next token may still select a child node.
    pub subcommands_allowed: bool,
    /// Whether a committed `--` has disabled option parsing.
    pub options_ended: bool,
    /// Canonical names of options already present in the line.
    pub used_options: BTreeSet<String>,
    /// Value-taking option whose value is currently active.
    pub awaiting_option: Option<&'a OptionSpec>,
    inherited_options: Vec<&'a OptionSpec>,
    inherited_generators: Vec<&'a GeneratorSpec>,
}

impl<'a> SpecResolution<'a> {
    /// Returns local and inherited global options in stable declaration order.
    #[must_use]
    pub fn available_options(&self) -> Vec<&'a OptionSpec> {
        let mut options = self.node.options.iter().collect::<Vec<_>>();
        options.extend(self.inherited_options.iter().copied());
        options
    }

    /// Returns dynamic generators applicable to the argument at the cursor.
    #[must_use]
    pub fn active_generators(&self) -> Vec<&'a GeneratorSpec> {
        self.node
            .generators
            .iter()
            .chain(self.inherited_generators.iter().copied())
            .filter(|generator| match &generator.target {
                GeneratorTarget::Positional(index) => {
                    self.awaiting_option.is_none()
                        && self.can_accept_positional
                        && *index == self.positional_index
                }
                GeneratorTarget::PositionalsFrom(index) => {
                    self.awaiting_option.is_none()
                        && self.can_accept_positional
                        && self.positional_index >= *index
                }
                GeneratorTarget::OptionValue(name) => self
                    .awaiting_option
                    .is_some_and(|option| option.names().any(|candidate| candidate == name)),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
struct InheritedOption {
    path: String,
    takes_value: bool,
}

fn validate_command(
    command: &CommandSpec,
    parent_path: &str,
    inherited_global_names: &BTreeMap<String, InheritedOption>,
) -> Result<(), SpecError> {
    validate_command_name(&command.name, parent_path)?;
    let path = if parent_path.is_empty() {
        command.name.clone()
    } else {
        format!("{parent_path} {}", command.name)
    };

    if !command.priority.is_finite() || !(0.0..=1.0).contains(&command.priority) {
        return Err(SpecError::new(
            &path,
            format!("priority {} is outside 0..=1", command.priority),
        ));
    }

    let mut command_names = BTreeSet::new();
    for name in command.names() {
        validate_command_name(name, &path)?;
        if !command_names.insert(name) {
            return Err(SpecError::new(
                &path,
                format!("duplicate command name or alias {name:?}"),
            ));
        }
    }

    let mut option_names = BTreeMap::new();
    for option in &command.options {
        validate_option(option, &path)?;
        for name in option.names() {
            if let Some(previous) = inherited_global_names.get(name) {
                return Err(SpecError::new(
                    &path,
                    format!(
                        "option {name:?} conflicts with inherited option from {}",
                        previous.path
                    ),
                ));
            }
            if let Some(previous) = option_names.insert(name, option.name.as_str()) {
                return Err(SpecError::new(
                    &path,
                    format!(
                        "option name {name:?} is shared by {previous:?} and {:?}",
                        option.name
                    ),
                ));
            }
        }
    }

    validate_generators(command, &path, inherited_global_names)?;

    let mut child_names = BTreeMap::new();
    for child in &command.subcommands {
        for name in child.names() {
            validate_command_name(name, &path)?;
            if let Some(previous) = child_names.insert(name, child.name.as_str()) {
                return Err(SpecError::new(
                    &path,
                    format!(
                        "subcommand name {name:?} is shared by {previous:?} and {:?}",
                        child.name
                    ),
                ));
            }
        }
    }

    let mut globals = inherited_global_names.clone();
    for option in command.options.iter().filter(|option| option.global) {
        for name in option.names() {
            globals.insert(
                name.to_string(),
                InheritedOption {
                    path: path.clone(),
                    takes_value: option.takes_value,
                },
            );
        }
    }
    for child in &command.subcommands {
        validate_command(child, &path, &globals)?;
    }

    Ok(())
}

fn validate_generators(
    command: &CommandSpec,
    path: &str,
    inherited_global_names: &BTreeMap<String, InheritedOption>,
) -> Result<(), SpecError> {
    for (index, generator) in command.generators.iter().enumerate() {
        generator
            .validate()
            .map_err(|error| SpecError::new(path, format!("invalid generator {index}: {error}")))?;
        if command.generators[..index].contains(generator) {
            return Err(SpecError::new(
                path,
                format!("duplicate generator declaration at index {index}"),
            ));
        }
        match &generator.target {
            GeneratorTarget::Positional(position) | GeneratorTarget::PositionalsFrom(position) => {
                if command
                    .max_positionals
                    .is_some_and(|maximum| *position >= maximum)
                {
                    return Err(SpecError::new(
                        path,
                        format!(
                            "generator targets positional {position}, beyond the maximum of {}",
                            command.max_positionals.unwrap_or_default()
                        ),
                    ));
                }
            }
            GeneratorTarget::OptionValue(name) => {
                validate_generator_option_target(command, path, inherited_global_names, name)?;
            }
        }
    }
    Ok(())
}

fn validate_generator_option_target(
    command: &CommandSpec,
    path: &str,
    inherited_global_names: &BTreeMap<String, InheritedOption>,
    name: &str,
) -> Result<(), SpecError> {
    let local = command
        .options
        .iter()
        .find(|option| option.names().any(|candidate| candidate == name));
    let takes_value = local.map(|option| option.takes_value).or_else(|| {
        inherited_global_names
            .get(name)
            .map(|option| option.takes_value)
    });
    match takes_value {
        Some(true) => Ok(()),
        Some(false) => Err(SpecError::new(
            path,
            format!("generator target {name:?} is not a value-taking option"),
        )),
        None => Err(SpecError::new(
            path,
            format!("generator targets unknown option {name:?}"),
        )),
    }
}

fn validate_command_name(name: &str, path: &str) -> Result<(), SpecError> {
    if name.is_empty() {
        return Err(SpecError::new(
            path,
            "command names and aliases may not be empty",
        ));
    }
    if name.starts_with('-')
        || name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(SpecError::new(
            path,
            format!("invalid command name or alias {name:?}"),
        ));
    }
    Ok(())
}

fn validate_option(option: &OptionSpec, path: &str) -> Result<(), SpecError> {
    if !option.priority.is_finite() || !(0.0..=1.0).contains(&option.priority) {
        return Err(SpecError::new(
            path,
            format!(
                "priority {} for option {:?} is outside 0..=1",
                option.priority, option.name
            ),
        ));
    }

    let mut names = BTreeSet::new();
    for name in option.names() {
        if name.len() < 2
            || !name.starts_with('-')
            || name.contains('=')
            || name
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(SpecError::new(
                path,
                format!("invalid option name {name:?}"),
            ));
        }
        if !names.insert(name) {
            return Err(SpecError::new(
                path,
                format!("duplicate spelling {name:?} for option {:?}", option.name),
            ));
        }
    }
    Ok(())
}

fn find_subcommand<'a>(command: &'a CommandSpec, name: &str) -> Option<&'a CommandSpec> {
    command.subcommands.iter().find(|child| {
        child
            .names()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    })
}

fn find_option<'a>(
    command: &'a CommandSpec,
    inherited: &[&'a OptionSpec],
    name: &str,
) -> Option<&'a OptionSpec> {
    command
        .options
        .iter()
        .chain(inherited.iter().copied())
        .find(|option| option.names().any(|candidate| candidate == name))
}

fn split_option_value(value: &str) -> (&str, bool) {
    value
        .split_once('=')
        .map_or((value, false), |(name, _)| (name, true))
}

fn is_shell_assignment(value: &str) -> bool {
    let Some((target, _)) = value.split_once('=') else {
        return false;
    };
    // Appending (name+=value) and element (name[index]=value) forms carry
    // their value exactly like the plain form and must be skipped the same
    // way; treating one as a command root would leak the value into the
    // ranking keyspace.
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = match target.split_once('[') {
        Some((name, index)) if index.ends_with(']') => name,
        _ => target,
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn root_suggestions(
    roots: &[CommandSpec],
    active: &ShellToken,
    full_active: &ShellToken,
    line: &str,
    cursor: usize,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    for root in roots {
        for name in root.names() {
            if prefix_matches(name, &active.cooked) {
                let Some(mut suggestion) = spec_suggestion(
                    (active, full_active),
                    (line, cursor),
                    name,
                    &root.description,
                    "command",
                    root.insertion,
                    format!("spec:root:{}:{name}", root.name),
                ) else {
                    continue;
                };
                suggestion.static_priority = root.priority;
                suggestions.push(suggestion);
            }
        }
    }
    suggestions
}

fn node_suggestions(
    resolution: &SpecResolution<'_>,
    active: &ShellToken,
    full_active: &ShellToken,
    line: &str,
    cursor: usize,
) -> Vec<Suggestion> {
    let mut commands = Vec::new();
    if resolution.subcommands_allowed {
        for child in &resolution.node.subcommands {
            for name in child.names() {
                if prefix_matches(name, &resolution.partial) {
                    let Some(mut suggestion) = spec_suggestion(
                        (active, full_active),
                        (line, cursor),
                        name,
                        &child.description,
                        "subcommand",
                        child.insertion,
                        format!("spec:{}:subcommand:{name}", resolution.path.join("/")),
                    ) else {
                        continue;
                    };
                    suggestion.static_priority = child.priority;
                    commands.push(suggestion);
                }
            }
        }
    }

    let mut options = Vec::new();
    if !resolution.options_ended {
        for option in resolution.available_options() {
            if !option.repeatable && resolution.used_options.contains(&option.name) {
                continue;
            }
            for name in option.names() {
                if option_prefix_matches(name, &resolution.partial) {
                    let Some(mut suggestion) = spec_suggestion(
                        (active, full_active),
                        (line, cursor),
                        name,
                        &option.description,
                        "option",
                        option.insertion,
                        format!("spec:{}:option:{name}", resolution.path.join("/")),
                    ) else {
                        continue;
                    };
                    suggestion.static_priority = if resolution.partial.starts_with('-') {
                        option.priority.max(0.75)
                    } else {
                        option.priority.min(0.35)
                    };
                    options.push(suggestion);
                }
            }
        }
    }

    if resolution.partial.starts_with('-') {
        options.extend(commands);
        options
    } else {
        commands.extend(options);
        commands
    }
}

fn spec_suggestion(
    tokens: (&ShellToken, &ShellToken),
    buffer: (&str, usize),
    candidate: &str,
    description: &str,
    icon: &str,
    insertion: InsertionBehavior,
    identity: String,
) -> Option<Suggestion> {
    let (active, full_active) = tokens;
    let (line, cursor) = buffer;
    let edit = spec_edit(active, full_active, line, cursor, candidate)?;
    Some(Suggestion::new(
        edit,
        candidate,
        description,
        icon,
        SuggestionSource::Spec,
        insertion,
        identity,
    ))
}

fn spec_edit(
    active: &ShellToken,
    full_active: &ShellToken,
    line: &str,
    cursor: usize,
    candidate: &str,
) -> Option<TextEdit> {
    let raw_is_plain = active
        .raw_text(line)
        .is_some_and(|raw| raw == active.cooked);
    if active.quote != QuoteKind::Unquoted || !raw_is_plain {
        if full_active.raw.end > cursor {
            return None;
        }
        return Some(TextEdit {
            range: active.raw.clone(),
            replacement: render_replacement(active, line, candidate),
        });
    }

    let suffix = full_active.cooked.strip_prefix(&active.cooked)?;
    let candidate_chars: Vec<_> = candidate.chars().collect();
    let prefix_len = active.cooked.chars().count();
    // The matched tail is counted in candidate characters: case folding can
    // change a character's length (one dotted capital I folds to two
    // characters), so counting typed suffix characters would misalign the
    // slice below.
    let matched_tail = case_folded_tail_length(&candidate_chars, suffix)?;
    if candidate_chars.len() < prefix_len + matched_tail {
        return None;
    }
    let replacement: String = candidate_chars[prefix_len..candidate_chars.len() - matched_tail]
        .iter()
        .collect();
    Some(TextEdit {
        range: cursor..cursor,
        replacement,
    })
}

/// Counts trailing `candidate` characters whose case-folded form equals the
/// case-folded `suffix`, or none when the tail does not match.
fn case_folded_tail_length(candidate: &[char], suffix: &str) -> Option<usize> {
    if suffix.is_empty() {
        return Some(0);
    }
    let folded_suffix = suffix.to_lowercase();
    let mut folded_tail = String::new();
    for (consumed, character) in candidate.iter().rev().enumerate() {
        let mut folded: String = character.to_lowercase().collect();
        folded.push_str(&folded_tail);
        folded_tail = folded;
        if folded_tail.len() >= folded_suffix.len() {
            return (folded_tail == folded_suffix).then_some(consumed + 1);
        }
    }
    None
}

fn prefix_matches(candidate: &str, partial: &str) -> bool {
    candidate
        .to_lowercase()
        .starts_with(&partial.to_lowercase())
}

fn option_prefix_matches(candidate: &str, partial: &str) -> bool {
    if partial.starts_with('-') {
        return prefix_matches(candidate, partial);
    }
    prefix_matches(candidate.trim_start_matches('-'), partial)
}

fn render_replacement(active: &ShellToken, line: &str, candidate: &str) -> String {
    let completed = preserve_prefix_case(&active.cooked, candidate);
    match active.quote {
        QuoteKind::Unquoted => {
            let Some(raw) = active.raw_text(line) else {
                return completed;
            };
            if raw == active.cooked {
                completed
            } else {
                shell_quote(&completed)
            }
        }
        QuoteKind::Single => single_quote(&completed),
        QuoteKind::Double => double_quote(&completed),
        QuoteKind::Mixed => shell_quote(&completed),
    }
}

fn preserve_prefix_case(partial: &str, candidate: &str) -> String {
    if !prefix_matches(candidate, partial) {
        return candidate.to_string();
    }
    let characters = partial.chars().count();
    let suffix_start = candidate
        .char_indices()
        .nth(characters)
        .map_or(candidate.len(), |(offset, _)| offset);
    format!("{partial}{}", &candidate[suffix_start..])
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        single_quote(value)
    }
}

fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn double_quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::completion::{FilesystemGenerator, GeneratorKind};

    fn git_index() -> SpecIndex {
        let commit = CommandSpec::new("commit", "record changes")
            .with_alias("ci")
            .with_option(
                OptionSpec::new("-m", "commit message")
                    .with_alias("--message")
                    .takes_value(true),
            )
            .with_option(OptionSpec::new("--amend", "amend the previous commit"))
            .with_option(OptionSpec::new("--trailer", "add a trailer").repeatable(true));
        let remote = CommandSpec::new("remote", "manage remotes")
            .with_subcommand(CommandSpec::new("add", "add a remote"))
            .with_subcommand(CommandSpec::new("remove", "remove a remote").with_alias("rm"));
        let git = CommandSpec::new("git", "version control")
            .with_alias("g")
            .with_option(OptionSpec::new("--no-pager", "disable pager").global(true))
            .with_option(
                OptionSpec::new("-C", "run in a directory")
                    .takes_value(true)
                    .global(true),
            )
            .with_subcommand(commit)
            .with_subcommand(remote)
            .with_subcommand(CommandSpec::new("checkout", "switch branches"));
        SpecIndex::new([git]).unwrap()
    }

    fn query(line: &str, cursor: usize) -> CompletionQuery {
        CompletionQuery::new(line, cursor, Path::new("/tmp"), 1).unwrap()
    }

    fn displays(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.display.as_str())
            .collect()
    }

    #[test]
    fn git_com_suggests_commit_with_a_partial_token_edit() {
        let index = git_index();
        let suggestions = index.suggestions(&query("git com", 7));
        let commit = suggestions
            .iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();

        assert_eq!(commit.edit.range, 7..7);
        assert_eq!(commit.edit.replacement, "mit");
        assert_eq!(commit.insertion, InsertionBehavior::AppendSpace);
    }

    #[test]
    fn git_remote_trailing_space_advances_to_remote_children() {
        let index = git_index();
        let suggestions = index.suggestions(&query("git remote ", 11));

        assert_eq!(displays(&suggestions)[..2], ["add", "remove"]);
        assert!(displays(&suggestions).contains(&"rm"));
    }

    #[test]
    fn a_quoted_flag_value_remains_one_argument() {
        let index = git_index();
        let line = "git commit -m 'Troy joins Greendale' ";
        let parsed = tokenize(line, line.len()).unwrap();
        let resolution = index.resolve(&parsed).unwrap();

        assert_eq!(resolution.path, ["git", "commit"]);
        assert_eq!(resolution.positional_count, 0);
        assert!(resolution.awaiting_option.is_none());
        assert!(displays(&index.suggestions(&query(line, line.len()))).contains(&"--amend"));
    }

    #[test]
    fn global_flags_and_values_before_a_subcommand_do_not_break_traversal() {
        let index = git_index();
        let line = "git --no-pager -C Greendale remote ";
        let parsed = tokenize(line, line.len()).unwrap();
        let resolution = index.resolve(&parsed).unwrap();

        assert_eq!(resolution.path, ["git", "remote"]);
        assert_eq!(resolution.positional_count, 0);
        assert!(displays(&index.suggestions(&query(line, line.len()))).contains(&"add"));
    }

    #[test]
    fn submitted_commands_resolve_to_canonical_learning_skeletons() {
        let index = git_index();
        assert_eq!(
            index.command_skeleton("git --no-pager -C Greendale remote add origin URL"),
            Some("git remote add".into())
        );
        assert_eq!(
            index.command_skeleton("COURSE=anthropology g ci -m 'Save Greendale'"),
            Some("git commit".into())
        );
        assert_eq!(
            index.command_skeleton("greendale-tool --verbose Troy"),
            Some("greendale-tool".into())
        );
        assert_eq!(index.command_skeleton("COURSE=anthropology"), None);
    }

    #[test]
    fn already_used_nonrepeatable_options_are_hidden() {
        let index = git_index();
        let suggestions = index.suggestions(&query("git commit --amend ", 19));
        let names = displays(&suggestions);

        assert!(!names.contains(&"--amend"));
        assert!(names.contains(&"--trailer"));

        let repeated = index.suggestions(&query("git commit --trailer Greendale ", 31));
        assert!(displays(&repeated).contains(&"--trailer"));
    }

    #[test]
    fn a_cursor_in_the_middle_preserves_the_authoritative_suffix() {
        let index = git_index();
        let line = "git com --no-pager";
        let suggestion = index
            .suggestions(&query(line, 7))
            .into_iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();

        assert_eq!(suggestion.edit.range, 7..7);
        assert_eq!(
            suggestion.edit.apply(line).unwrap(),
            "git commit --no-pager"
        );
    }

    #[test]
    fn attached_option_values_and_double_dash_are_not_subcommands() {
        let root = CommandSpec::new("tool", "tool")
            .with_option(
                OptionSpec::new("--config", "config")
                    .takes_value(true)
                    .global(true),
            )
            .with_option(OptionSpec::new("--verbose", "verbose"))
            .with_subcommand(CommandSpec::new("child", "child"));
        let index = SpecIndex::new([root]).unwrap();

        let attached = tokenize("tool --config=file child ", 25).unwrap();
        assert_eq!(index.resolve(&attached).unwrap().path, ["tool", "child"]);

        let ended = tokenize("tool -- child ", 14).unwrap();
        let resolution = index.resolve(&ended).unwrap();
        assert_eq!(resolution.path, ["tool"]);
        assert!(resolution.options_ended);
        assert!(!resolution.subcommands_allowed);
        assert!(index.suggestions(&query("tool -- child -", 15)).is_empty());
    }

    #[test]
    fn active_attached_value_exposes_a_value_only_edit() {
        let index = SpecIndex::new([CommandSpec::new("tool", "tool")
            .with_option(OptionSpec::new("--config", "config").takes_value(true))])
        .unwrap();
        let parsed = tokenize("tool --config=Green", 19).unwrap();
        let resolution = index.resolve(&parsed).unwrap();

        assert_eq!(resolution.awaiting_option.unwrap().name, "--config");
        assert_eq!(resolution.partial, "Green");
        assert_eq!(resolution.replacement, 14..19);
    }

    #[test]
    fn unknown_positionals_stop_deeper_tree_traversal_and_respect_the_cap() {
        let root = CommandSpec::new("tool", "tool")
            .with_max_positionals(1)
            .with_subcommand(
                CommandSpec::new("known", "known")
                    .with_subcommand(CommandSpec::new("deeper", "deeper")),
            );
        let index = SpecIndex::new([root]).unwrap();
        let parsed = tokenize("tool unknown known ", 19).unwrap();
        let resolution = index.resolve(&parsed).unwrap();

        assert_eq!(resolution.path, ["tool"]);
        assert_eq!(resolution.positional_count, 2);
        assert!(!resolution.subcommands_allowed);
        assert!(!resolution.can_accept_positional);
        assert!(!displays(&index.suggestions(&query("tool unknown k", 14))).contains(&"known"));
    }

    #[test]
    fn traverses_exact_aliases_but_not_partial_committed_names() {
        let index = git_index();
        let alias = tokenize("g ci ", 5).unwrap();
        assert_eq!(index.resolve(&alias).unwrap().path, ["git", "commit"]);

        let partial = tokenize("git rem ", 8).unwrap();
        assert_eq!(index.resolve(&partial).unwrap().path, ["git"]);
    }

    #[test]
    fn validation_rejects_recursive_name_and_option_conflicts() {
        let duplicate_children = CommandSpec::new("tool", "tool")
            .with_subcommand(CommandSpec::new("run", "run"))
            .with_subcommand(CommandSpec::new("execute", "execute").with_alias("run"));
        assert!(SpecIndex::new([duplicate_children]).is_err());

        let inherited_conflict = CommandSpec::new("tool", "tool")
            .with_option(OptionSpec::new("--verbose", "verbose").global(true))
            .with_subcommand(
                CommandSpec::new("run", "run")
                    .with_option(OptionSpec::new("--verbose", "different")),
            );
        let error = SpecIndex::new([inherited_conflict]).unwrap_err();
        assert!(error.to_string().contains("inherited option"));
    }

    #[test]
    fn suggestions_preserve_typed_case_and_quote_incomplete_tokens() {
        let index = git_index();
        let upper = index.suggestions(&query("Git COM", 7));
        let commit = upper
            .iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();
        assert_eq!(commit.edit.replacement, "mit");

        let quoted = index.suggestions(&query("git 'com", 8));
        let commit = quoted
            .iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();
        assert_eq!(commit.edit.replacement, "'commit'");
    }

    #[test]
    fn same_token_suffix_is_not_duplicated() {
        let index = git_index();
        let complete_line = "git commit";
        let complete_query = query(complete_line, 7);
        let complete = index
            .suggestions(&complete_query)
            .into_iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();
        assert_eq!(complete.edit.range, 7..7);
        assert!(complete.edit.replacement.is_empty());
        assert_eq!(
            complete.resulting_line(&complete_query).unwrap(),
            complete_line
        );

        let partial_line = "git comit";
        let partial_query = query(partial_line, 6);
        let partial = index
            .suggestions(&partial_query)
            .into_iter()
            .find(|suggestion| suggestion.display == "commit")
            .unwrap();
        assert_eq!(partial.edit.replacement, "m");
        assert_eq!(
            partial.resulting_line(&partial_query).unwrap(),
            "git commit"
        );
    }

    #[test]
    fn root_names_are_case_insensitive_for_lookup_and_conflicts() {
        let conflict = SpecIndex::new([
            CommandSpec::new("git", "git"),
            CommandSpec::new("other", "other").with_alias("GIT"),
        ]);
        assert!(conflict.is_err());
    }

    #[test]
    fn metadata_controls_exact_insertion_behavior() {
        let index =
            SpecIndex::new([CommandSpec::new("logout", "end the session")
                .with_insertion(InsertionBehavior::Exact)])
            .unwrap();
        let query = query("log", 3);
        let suggestion = index.suggestions(&query).remove(0);
        assert_eq!(suggestion.resulting_line(&query).unwrap(), "logout");
    }

    #[test]
    fn unknown_option_stops_deeper_traversal() {
        let index =
            SpecIndex::new([CommandSpec::new("tool", "tool")
                .with_subcommand(CommandSpec::new("child", "child"))])
            .unwrap();
        let parsed = tokenize("tool --unknown child ", 21).unwrap();
        let resolution = index.resolve(&parsed).unwrap();
        assert_eq!(resolution.path, ["tool"]);
        assert!(!resolution.subcommands_allowed);
    }

    #[test]
    fn generator_targets_are_validated_against_node_arguments() {
        let valid = CommandSpec::new("tool", "tool")
            .with_option(OptionSpec::new("--format", "format").takes_value(true))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::FileTypes,
                GeneratorTarget::OptionValue("--format".into()),
            ))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::Processes,
                GeneratorTarget::Positional(0),
            ));
        valid.validate().unwrap();

        let unknown_option = CommandSpec::new("tool", "tool").with_generator(GeneratorSpec::new(
            GeneratorKind::FileTypes,
            GeneratorTarget::OptionValue("--format".into()),
        ));
        assert!(
            unknown_option
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unknown option")
        );

        let flag = CommandSpec::new("tool", "tool")
            .with_option(OptionSpec::new("--verbose", "verbose"))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::FileTypes,
                GeneratorTarget::OptionValue("--verbose".into()),
            ));
        assert!(flag.validate().is_err());

        let capped = CommandSpec::new("tool", "tool")
            .with_max_positionals(0)
            .with_generator(GeneratorSpec::new(
                GeneratorKind::Processes,
                GeneratorTarget::Positional(0),
            ));
        assert!(capped.validate().is_err());

        let capped_range = CommandSpec::new("tool", "tool")
            .with_max_positionals(1)
            .with_generator(GeneratorSpec::new(
                GeneratorKind::Processes,
                GeneratorTarget::PositionalsFrom(1),
            ));
        assert!(capped_range.validate().is_err());
    }

    #[test]
    fn case_folded_tail_alignment_counts_candidate_characters() {
        let chars: Vec<char> = "İstanbul".chars().collect();
        assert_eq!(case_folded_tail_length(&chars, "BUL"), Some(3));
        assert_eq!(case_folded_tail_length(&chars, ""), Some(0));
        assert_eq!(case_folded_tail_length(&chars, "xyz"), None);

        // One dotted capital I folds to two characters; the matched tail is
        // one candidate character, not two.
        let dotted: Vec<char> = "xİ".chars().collect();
        assert_eq!(case_folded_tail_length(&dotted, "i\u{307}"), Some(1));
        // A fold that crosses the suffix boundary is not a match.
        assert_eq!(case_folded_tail_length(&dotted, "\u{307}"), None);
    }

    #[test]
    fn resolution_selects_only_generators_active_at_the_cursor() {
        let index = SpecIndex::new([CommandSpec::new("tool", "tool")
            .with_option(OptionSpec::new("--format", "format").takes_value(true))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::Processes,
                GeneratorTarget::Positional(0),
            ))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::FileTypes,
                GeneratorTarget::OptionValue("--format".into()),
            ))])
        .unwrap();

        let positional = tokenize("tool ", 5).unwrap();
        let active = index.resolve(&positional).unwrap().active_generators();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].kind, GeneratorKind::Processes);

        let option_value = tokenize("tool --format ", 14).unwrap();
        let active = index.resolve(&option_value).unwrap().active_generators();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].kind, GeneratorKind::FileTypes);

        let consumed = tokenize("tool process ", 13).unwrap();
        assert!(
            index
                .resolve(&consumed)
                .unwrap()
                .active_generators()
                .is_empty()
        );

        let inherited = SpecIndex::new([CommandSpec::new("tool", "tool")
            .with_option(
                OptionSpec::new("--config", "config")
                    .takes_value(true)
                    .global(true),
            )
            .with_generator(GeneratorSpec::new(
                GeneratorKind::Filesystem(FilesystemGenerator::default()),
                GeneratorTarget::OptionValue("--config".into()),
            ))
            .with_subcommand(CommandSpec::new("child", "child"))])
        .unwrap();
        let option_value = tokenize("tool child --config ", 20).unwrap();
        let active = inherited
            .resolve(&option_value)
            .unwrap()
            .active_generators();
        assert_eq!(active.len(), 1);
        assert!(matches!(active[0].kind, GeneratorKind::Filesystem(_)));
    }

    #[test]
    fn positional_range_generator_remains_active_after_earlier_arguments() {
        let index = SpecIndex::new([CommandSpec::new("cp", "copy files").with_generator(
            GeneratorSpec::new(
                GeneratorKind::Filesystem(FilesystemGenerator::default()),
                GeneratorTarget::PositionalsFrom(0),
            ),
        )])
        .unwrap();

        for line in ["cp ", "cp first ", "cp first second "] {
            let parsed = tokenize(line, line.len()).unwrap();
            let active = index.resolve(&parsed).unwrap().active_generators();
            assert_eq!(active.len(), 1, "generator was inactive for {line:?}");
        }
    }
}
