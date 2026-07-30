//! Deserialization and normalization for the checked-in IRIS catalog snapshot.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::completion::{
    CommandSpec, FilesystemGenerator, GeneratorKind, GeneratorSpec, GeneratorTarget, OptionSpec,
};

const RAW_CATALOG: &str = include_str!("iris.json");

#[derive(Debug, Deserialize)]
pub(super) struct ImportedCatalog {
    pub(super) inventory: Vec<ImportedInventoryEntry>,
    pub(super) commands: Vec<ImportedCommand>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ImportedInventoryEntry {
    pub(super) category: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) source: String,
    #[serde(default)]
    pub(super) merged: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ImportedCommand {
    pub(super) name: String,
    #[serde(default)]
    aliases: Vec<String>,
    pub(super) description: String,
    #[serde(default)]
    subcommands: Vec<ImportedCommand>,
    #[serde(default)]
    options: Vec<ImportedOption>,
    #[serde(default)]
    generator: String,
    #[serde(default)]
    max_args: usize,
    #[serde(default)]
    priority: usize,
}

#[derive(Debug, Deserialize)]
struct ImportedOption {
    name: String,
    description: String,
    #[serde(default)]
    priority: usize,
}

static CATALOG: OnceLock<Result<ImportedCatalog, String>> = OnceLock::new();

pub(super) fn catalog() -> Result<&'static ImportedCatalog, String> {
    CATALOG
        .get_or_init(|| {
            serde_json::from_str(RAW_CATALOG)
                .map_err(|error| format!("cannot parse imported IRIS catalog: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl ImportedCommand {
    pub(super) fn command_spec(&self) -> CommandSpec {
        let path = vec![self.name.as_str()];
        self.convert(&path)
            .expect("top-level imported command names are audited before conversion")
    }

    pub(super) fn generator_symbols<'a>(
        &'a self,
        parent: &str,
        symbols: &mut Vec<(String, &'a str)>,
    ) {
        let path = if parent.is_empty() {
            self.name.clone()
        } else {
            format!("{parent} {}", self.name)
        };
        if !self.generator.is_empty() {
            symbols.push((path.clone(), &self.generator));
        }
        for child in &self.subcommands {
            child.generator_symbols(&path, symbols);
        }
    }

    fn convert(&self, path: &[&str]) -> Option<CommandSpec> {
        if !valid_command_name(&self.name) {
            return None;
        }

        let mut command = CommandSpec::new(&self.name, &self.description);
        let mut names = BTreeSet::from([self.name.as_str()]);
        for alias in &self.aliases {
            if valid_command_name(alias) && names.insert(alias) {
                command = command.with_alias(alias);
            }
        }
        if self.max_args > 0 {
            command = command.with_max_positionals(self.max_args);
        }
        if self.priority > 0 {
            command = command.with_priority(normalize_priority(self.priority));
        }

        let mut option_names = BTreeSet::new();
        let mut value_commands = Vec::new();
        for imported in &self.options {
            if let Some(option) = convert_option(imported) {
                if option_names.insert(option.name.clone()) {
                    command = command.with_option(option);
                }
            } else if valid_command_name(&imported.name) {
                value_commands.push(CommandSpec::new(&imported.name, &imported.description));
            }
        }

        let mut child_names = BTreeSet::new();
        for child in &self.subcommands {
            let mut child_path = path.to_vec();
            child_path.push(&child.name);
            let Some(converted) = child.convert(&child_path) else {
                continue;
            };
            if child_names.insert(converted.name.clone()) {
                command = command.with_subcommand(converted);
            }
        }
        for value in value_commands {
            if child_names.insert(value.name.clone()) {
                command = command.with_subcommand(value);
            }
        }

        let owned_path = path
            .iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>();
        for generator in generators_for(&self.generator, &owned_path) {
            command = command.with_generator(generator);
        }
        Some(command)
    }
}

pub(super) fn generator_is_mapped(symbol: &str, path: &str) -> bool {
    symbol.is_empty()
        || symbol == "commands/fs.modeGenerator.func1"
        || !generators_for(
            symbol,
            &path
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
        .is_empty()
}

fn convert_option(imported: &ImportedOption) -> Option<OptionSpec> {
    let (name, attached_value) = imported
        .name
        .split_once('=')
        .map_or((imported.name.as_str(), false), |(name, _)| (name, true));
    if !valid_option_name(name) {
        return None;
    }
    let mut option = OptionSpec::new(name, &imported.description);
    if attached_value || imported.description.contains(&format!("{name}=")) {
        option = option.takes_value(true);
    }
    if imported.priority > 0 {
        option = option.with_priority(normalize_priority(imported.priority));
    }
    Some(option)
}

fn generators_for(symbol: &str, path: &[String]) -> Vec<GeneratorSpec> {
    let joined = path.join(" ");
    let range = |kind| GeneratorSpec::new(kind, GeneratorTarget::PositionalsFrom(0));
    let positional = |kind, index| GeneratorSpec::new(kind, GeneratorTarget::Positional(index));
    let filesystem = |directory_only| {
        range(GeneratorKind::Filesystem(FilesystemGenerator {
            directory_only,
            ..FilesystemGenerator::default()
        }))
    };

    match symbol {
        "" | "commands/fs.modeGenerator.func1" => Vec::new(),
        "spec.FileGenerator.func1" => vec![filesystem(false)],
        "commands/fs.init.1.func1" => vec![filesystem(true)],
        symbol if symbol.contains("ZoxideGenerator") => {
            vec![filesystem(true), range(GeneratorKind::ZoxideDirectories)]
        }
        "commands/git.GitBranchGenerator" => vec![range(GeneratorKind::GitBranches)],
        "commands/git.GitRemoteGenerator" => vec![range(GeneratorKind::GitRemotes)],
        "commands/git.GitStashGenerator" => vec![range(GeneratorKind::GitStashes)],
        "commands/git.GitTagGenerator" => vec![range(GeneratorKind::GitTags)],
        "commands/git.GitCommitGenerator" => vec![range(GeneratorKind::GitCommits)],
        "commands/git.GitPushPullGenerator" => vec![
            positional(GeneratorKind::GitRemotes, 0),
            positional(GeneratorKind::GitBranches, 1),
        ],
        symbol if symbol.starts_with("commands/git.init.1.func") => match joined.as_str() {
            "git checkout" => vec![
                range(GeneratorKind::GitBranches),
                range(GeneratorKind::GitFiles),
            ],
            "git show" => vec![
                range(GeneratorKind::GitCommits),
                range(GeneratorKind::GitTags),
            ],
            "git reset" => vec![range(GeneratorKind::GitCommits)],
            _ => Vec::new(),
        },
        "commands/js.NpmScriptGenerator" => vec![range(GeneratorKind::PackageScripts)],
        "commands/python.pipPackageGenerator" => vec![range(GeneratorKind::Packages)],
        symbol if symbol.contains("installedPackageGenerator") => {
            vec![range(GeneratorKind::Packages)]
        }
        "commands/sys.envVarGenerator" => vec![range(GeneratorKind::EnvironmentVariables)],
        "commands/sys.processGenerator" => vec![range(GeneratorKind::Processes)],
        "commands/ops.sshHostGenerator" => vec![range(GeneratorKind::SshHosts)],
        "commands/ops.dockerContainerGenerator"
        | "commands/ops.dockerRunningContainerGenerator" => {
            vec![range(GeneratorKind::DockerContainers)]
        }
        "commands/ops.dockerImageGenerator" => vec![range(GeneratorKind::DockerImages)],
        "commands/ops.init.32.func1" if joined == "docker inspect" => vec![
            range(GeneratorKind::DockerContainers),
            range(GeneratorKind::DockerImages),
        ],
        "commands/runner.init.7.func1" if joined == "just" => {
            vec![range(GeneratorKind::JustRecipes)]
        }
        "commands/runner.init.10.func1" if joined == "make" => {
            vec![range(GeneratorKind::MakeTargets)]
        }
        _ => Vec::new(),
    }
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

fn valid_option_name(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with('-')
        && !matches!(name, "-" | "--")
        && !name.contains('=')
        && !name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

fn normalize_priority(priority: usize) -> f64 {
    f64::from(u32::try_from(priority.min(100)).unwrap_or(100)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_snapshot_has_expected_inventory_and_runtime_counts() {
        let catalog = catalog().unwrap();
        assert_eq!(catalog.inventory.len(), 567);
        assert_eq!(catalog.commands.len(), 571);
    }

    #[test]
    fn every_imported_generator_symbol_has_a_bounded_mapping() {
        let catalog = catalog().unwrap();
        let mut unmapped = Vec::new();
        for command in &catalog.commands {
            let mut symbols = Vec::new();
            command.generator_symbols("", &mut symbols);
            unmapped.extend(
                symbols
                    .into_iter()
                    .filter(|(path, symbol)| !generator_is_mapped(symbol, path)),
            );
        }
        assert!(unmapped.is_empty(), "unmapped generators: {unmapped:?}");
    }

    #[test]
    fn invalid_argument_placeholders_are_not_promoted_to_commands() {
        assert!(!valid_command_name("WORKING DIRECTORY"));
        assert!(!valid_command_name("--verbose"));
        assert!(valid_command_name("aux"));
    }
}
