//! Audited source of the built-in command catalog.
//!
//! Runtime specifications and generated documentation both consume these
//! entries. The baseline counts intentionally match the migration inventory in
//! the product requirements.

mod imported;
mod representative;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::OnceLock;

use crate::completion::{CommandSpec, OptionSpec, SpecIndex};

/// One of the 14 catalog categories in the migration baseline.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Category {
    /// Cloud platforms, containers, orchestration, operations, and databases.
    CloudDevOps,
    /// JavaScript, TypeScript, frontend, and Node.js tooling.
    JavaScript,
    /// Python tooling and data-science applications.
    Python,
    /// Rust tooling and modern command-line utilities.
    Rust,
    /// Go development and project tools.
    Go,
    /// Java, Kotlin, and JVM build tools.
    Jvm,
    /// C and C++ compilers and build systems.
    Cpp,
    /// Git version control and GitHub tooling.
    Git,
    /// Operating-system package managers.
    PackageManagers,
    /// Filesystem, directory, and archive utilities.
    Filesystem,
    /// Editors, pagers, and file viewers.
    Editors,
    /// Text, JSON, and stream manipulation.
    Text,
    /// Task runners and build automation.
    TaskRunners,
    /// System administration, networking, and process management.
    System,
}

impl Category {
    /// Categories in documentation order.
    pub const ALL: [Self; 14] = [
        Self::CloudDevOps,
        Self::JavaScript,
        Self::Python,
        Self::Rust,
        Self::Go,
        Self::Jvm,
        Self::Cpp,
        Self::Git,
        Self::PackageManagers,
        Self::Filesystem,
        Self::Editors,
        Self::Text,
        Self::TaskRunners,
        Self::System,
    ];

    /// Human-readable category name used in generated documentation.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CloudDevOps => "Cloud, containers, Kubernetes, DevOps, and databases",
            Self::JavaScript => "JavaScript, TypeScript, frontend, and Node.js tools",
            Self::Python => "Python ecosystem and data science",
            Self::Rust => "Rust ecosystem and modern CLI tools",
            Self::Go => "Go development and project tools",
            Self::Jvm => "Java, Kotlin, and JVM build tools",
            Self::Cpp => "C/C++ compilers and build systems",
            Self::Git => "Git version control and GitHub tools",
            Self::PackageManagers => "System package managers",
            Self::Filesystem => "Filesystem, directory, and archive utilities",
            Self::Editors => "Editors, pagers, and file viewers",
            Self::Text => "Text processing, JSON, and stream manipulation",
            Self::TaskRunners => "Task runners and build automation",
            Self::System => "System administration, network, and process management",
        }
    }

    /// Required number of top-level entries in the migration baseline.
    #[must_use]
    pub const fn baseline_count(self) -> usize {
        match self {
            Self::CloudDevOps => 118,
            Self::JavaScript => 82,
            Self::Python => 19,
            Self::Rust => 11,
            Self::Go => 3,
            Self::Jvm => 14,
            Self::Cpp => 16,
            Self::Git => 8,
            Self::PackageManagers => 12,
            Self::Filesystem => 30,
            Self::Editors => 27,
            Self::Text => 28,
            Self::TaskRunners => 24,
            Self::System => 175,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::CloudDevOps => "cloud-devops",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Jvm => "jvm",
            Self::Cpp => "cpp",
            Self::Git => "git",
            Self::PackageManagers => "package-managers",
            Self::Filesystem => "filesystem",
            Self::Editors => "editors",
            Self::Text => "text",
            Self::TaskRunners => "task-runners",
            Self::System => "system",
        }
    }

    fn from_iris_key(key: &str) -> Option<Self> {
        match key {
            "ops" => Some(Self::CloudDevOps),
            "js" => Some(Self::JavaScript),
            "python" => Some(Self::Python),
            "rust" => Some(Self::Rust),
            "golang" => Some(Self::Go),
            "jvm" => Some(Self::Jvm),
            "cc" => Some(Self::Cpp),
            "git" => Some(Self::Git),
            "pkginstaller" => Some(Self::PackageManagers),
            "fs" => Some(Self::Filesystem),
            "view" => Some(Self::Editors),
            "text" => Some(Self::Text),
            "runner" => Some(Self::TaskRunners),
            "sys" => Some(Self::System),
            _ => None,
        }
    }
}

/// How one baseline command is represented by the Rust implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationStatus {
    /// A built-in top-level specification exists under this name.
    Migrated,
    /// The baseline name is represented by another canonical specification.
    Aliased { canonical: String },
    /// A duplicate documented record is represented by the same canonical root.
    Merged { canonical: String },
    /// Runtime inference deliberately replaces a static definition.
    Inferred,
    /// The baseline entry is deliberately unsupported.
    Retired { reason: String },
}

impl MigrationStatus {
    const fn label(&self) -> &'static str {
        match self {
            Self::Migrated => "migrated",
            Self::Aliased { .. } => "aliased",
            Self::Merged { .. } => "merged",
            Self::Inferred => "inferred",
            Self::Retired { .. } => "retired",
        }
    }
}

/// One audited top-level command from the migration inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogEntry {
    /// Executable basename or baseline identity.
    pub name: String,
    /// Concise user-facing purpose.
    pub description: String,
    /// Baseline category.
    pub category: Category,
    /// Explicit migration disposition.
    pub status: MigrationStatus,
    /// IRIS source file defining the documented record.
    pub source: String,
}

/// Successful catalog audit summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogAudit {
    /// Accounted top-level baseline entries.
    pub total: usize,
    /// Unique canonical command roots represented by the inventory.
    pub unique_roots: usize,
    /// Accounted entries by category.
    pub category_counts: BTreeMap<Category, usize>,
    /// Entries by migration disposition label.
    pub status_counts: BTreeMap<&'static str, usize>,
}

/// Catalog validation or indexing failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogError(String);

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CatalogError {}

/// One malformed root omitted from the runtime index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedSpec {
    /// Canonical root name.
    pub name: String,
    /// Validation failure with its command path.
    pub error: String,
}

struct RuntimeCatalog {
    index: SpecIndex,
    rejected: Vec<RejectedSpec>,
}

static RUNTIME_CATALOG: OnceLock<Result<RuntimeCatalog, CatalogError>> = OnceLock::new();
static INVENTORY: OnceLock<Result<Vec<CatalogEntry>, CatalogError>> = OnceLock::new();

/// Returns every baseline entry in stable category order.
fn entries() -> Result<&'static [CatalogEntry], CatalogError> {
    INVENTORY
        .get_or_init(|| {
            let catalog = imported::catalog().map_err(CatalogError)?;
            catalog
                .inventory
                .iter()
                .map(|entry| {
                    let category = Category::from_iris_key(&entry.category).ok_or_else(|| {
                        CatalogError(format!(
                            "{} uses unknown IRIS category {:?}",
                            entry.name, entry.category
                        ))
                    })?;
                    let status = if entry.merged {
                        MigrationStatus::Merged {
                            canonical: entry.name.clone(),
                        }
                    } else {
                        MigrationStatus::Migrated
                    };
                    Ok(CatalogEntry {
                        name: entry.name.clone(),
                        description: entry.description.clone(),
                        category,
                        status,
                        source: entry.source.clone(),
                    })
                })
                .collect()
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// Verifies category totals, identity uniqueness, and explicit dispositions.
///
/// # Errors
///
/// Returns a descriptive error for the first incomplete or malformed catalog
/// invariant.
pub fn audit() -> Result<CatalogAudit, CatalogError> {
    let all_entries = entries()?;
    let imported = imported::catalog().map_err(CatalogError)?;
    let inventory = audit_inventory(all_entries)?;
    audit_migration_targets(all_entries)?;
    audit_category_counts(&inventory.category_counts)?;

    let total = all_entries.len();
    if total != 567 {
        return Err(CatalogError(format!(
            "catalog contains {total} inventory records; expected 567"
        )));
    }
    let unique_roots = inventory.names.len();
    if unique_roots != 566 {
        return Err(CatalogError(format!(
            "catalog contains {unique_roots} unique roots; expected 566"
        )));
    }
    audit_runtime_commands(imported, &inventory.names)?;
    if baseline_manifest()? != include_str!("baseline.tsv") {
        return Err(CatalogError(
            "catalog identities differ from baseline.tsv; review the migration and explicitly freeze the new baseline"
                .into(),
        ));
    }

    Ok(CatalogAudit {
        total,
        unique_roots,
        category_counts: inventory.category_counts,
        status_counts: inventory.status_counts,
    })
}

struct InventoryAudit {
    names: BTreeSet<String>,
    category_counts: BTreeMap<Category, usize>,
    status_counts: BTreeMap<&'static str, usize>,
}

fn audit_inventory(all_entries: &[CatalogEntry]) -> Result<InventoryAudit, CatalogError> {
    let mut names = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut category_counts = BTreeMap::new();
    let mut status_counts = BTreeMap::new();

    for entry in all_entries {
        if !valid_executable_basename(&entry.name) {
            return Err(CatalogError(format!(
                "invalid top-level command name {:?}",
                entry.name
            )));
        }
        if entry.description.trim().is_empty() || entry.description.chars().any(char::is_control) {
            return Err(CatalogError(format!(
                "{} has an empty description",
                entry.name
            )));
        }
        if !identities.insert((entry.category, entry.name.to_lowercase())) {
            return Err(CatalogError(format!(
                "duplicate catalog identity {} in {}",
                entry.name,
                entry.category.label()
            )));
        }
        names.insert(entry.name.to_lowercase());
        if entry.source.is_empty()
            || !Path::new(&entry.source)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("go"))
            || entry.source.chars().any(char::is_control)
        {
            return Err(CatalogError(format!(
                "{} has invalid IRIS source provenance {:?}",
                entry.name, entry.source
            )));
        }
        *category_counts.entry(entry.category).or_insert(0) += 1;
        *status_counts.entry(entry.status.label()).or_insert(0) += 1;

        match &entry.status {
            MigrationStatus::Aliased { canonical } | MigrationStatus::Merged { canonical }
                if canonical.is_empty() =>
            {
                return Err(CatalogError(format!(
                    "{} has an empty canonical target",
                    entry.name
                )));
            }
            MigrationStatus::Retired { reason } if reason.trim().is_empty() => {
                return Err(CatalogError(format!(
                    "{} is retired without a reason",
                    entry.name
                )));
            }
            _ => {}
        }
    }
    Ok(InventoryAudit {
        names,
        category_counts,
        status_counts,
    })
}

fn audit_migration_targets(all_entries: &[CatalogEntry]) -> Result<(), CatalogError> {
    for entry in all_entries {
        let (MigrationStatus::Aliased { canonical } | MigrationStatus::Merged { canonical }) =
            &entry.status
        else {
            continue;
        };
        if matches!(entry.status, MigrationStatus::Aliased { .. })
            && entry.name.eq_ignore_ascii_case(canonical)
        {
            return Err(CatalogError(format!("{} aliases itself", entry.name)));
        }
        let Some(target) = all_entries.iter().find(|target| {
            target.name.eq_ignore_ascii_case(canonical)
                && matches!(&target.status, MigrationStatus::Migrated)
        }) else {
            return Err(CatalogError(format!(
                "{} aliases missing canonical command {canonical}",
                entry.name
            )));
        };
        debug_assert!(matches!(&target.status, MigrationStatus::Migrated));
    }
    Ok(())
}

fn audit_category_counts(category_counts: &BTreeMap<Category, usize>) -> Result<(), CatalogError> {
    for category in Category::ALL {
        let actual = category_counts.get(&category).copied().unwrap_or_default();
        let required = category.baseline_count();
        if actual != required {
            return Err(CatalogError(format!(
                "{} contains {actual} entries; expected {required}",
                category.label()
            )));
        }
    }
    Ok(())
}

fn audit_runtime_commands(
    imported: &imported::ImportedCatalog,
    inventory_names: &BTreeSet<String>,
) -> Result<(), CatalogError> {
    let mut runtime_names = BTreeSet::new();
    for command in &imported.commands {
        if !valid_executable_basename(&command.name) {
            return Err(CatalogError(format!(
                "invalid imported runtime root {:?}",
                command.name
            )));
        }
        if !runtime_names.insert(command.name.to_lowercase()) {
            return Err(CatalogError(format!(
                "duplicate imported runtime root {}",
                command.name
            )));
        }
        let mut symbols = Vec::new();
        command.generator_symbols("", &mut symbols);
        if let Some((path, symbol)) = symbols
            .into_iter()
            .find(|(path, symbol)| !imported::generator_is_mapped(symbol, path))
        {
            return Err(CatalogError(format!(
                "unmapped IRIS generator {symbol:?} at {path}"
            )));
        }
    }
    if let Some(missing) = inventory_names.difference(&runtime_names).next() {
        return Err(CatalogError(format!(
            "inventory root {missing} is absent from the imported runtime registry"
        )));
    }
    Ok(())
}

fn valid_executable_basename(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

/// Renders the frozen IRIS identities, source provenance, and migration status.
///
/// # Errors
///
/// Returns an error when the checked-in import cannot be decoded.
pub fn baseline_manifest() -> Result<String, CatalogError> {
    let mut catalog_entries = entries()?.iter().collect::<Vec<_>>();
    catalog_entries.sort_unstable_by_key(|entry| (entry.category, entry.name.as_str()));
    let mut output = String::from("# IRIS catalog migration inventory v1\n");
    for entry in catalog_entries {
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            entry.category.key(),
            entry.name,
            entry.source,
            entry.status.label()
        )
        .expect("writing to a String cannot fail");
    }
    Ok(output)
}

/// Builds the validated runtime completion index from the audited catalog.
///
/// # Errors
///
/// Returns an error if the inventory audit or a generated specification fails.
pub fn spec_index() -> Result<&'static SpecIndex, CatalogError> {
    runtime_catalog().map(|catalog| &catalog.index)
}

/// Returns malformed root specifications omitted from the usable runtime index.
///
/// Maintainer tests require this slice to remain empty. Keeping it at runtime
/// prevents one bad definition from crashing or disabling unrelated roots.
///
/// # Errors
///
/// Returns an inventory-level audit or indexing error.
pub fn rejected_specs() -> Result<&'static [RejectedSpec], CatalogError> {
    runtime_catalog().map(|catalog| catalog.rejected.as_slice())
}

fn runtime_catalog() -> Result<&'static RuntimeCatalog, CatalogError> {
    RUNTIME_CATALOG
        .get_or_init(build_runtime_catalog)
        .as_ref()
        .map_err(Clone::clone)
}

fn build_runtime_catalog() -> Result<RuntimeCatalog, CatalogError> {
    let audit = audit()?;
    debug_assert_eq!(audit.unique_roots, 566);
    let imported = imported::catalog().map_err(CatalogError)?;
    let generated = imported
        .commands
        .iter()
        .map(imported_command_spec)
        .chain(representative::supplemental_specs());
    index_roots(generated)
}

fn index_roots(
    generated: impl IntoIterator<Item = CommandSpec>,
) -> Result<RuntimeCatalog, CatalogError> {
    let mut roots = Vec::new();
    let mut rejected = Vec::new();
    for root in generated {
        if let Err(error) = root.validate() {
            rejected.push(RejectedSpec {
                name: root.name,
                error: error.to_string(),
            });
        } else {
            roots.push(root);
        }
    }
    let index = SpecIndex::new(roots).map_err(|error| CatalogError(error.to_string()))?;
    Ok(RuntimeCatalog { index, rejected })
}

fn imported_command_spec(command: &imported::ImportedCommand) -> CommandSpec {
    let imported = command.command_spec();
    let overlay = representative::spec(&command.name, &command.description)
        .or_else(|| (command.name == "kubectl").then(|| kubectl_spec(&command.description)));
    match overlay {
        Some(overlay) => merge_specs(imported, overlay),
        None => imported,
    }
}

fn command_spec(entry: &CatalogEntry) -> Result<CommandSpec, CatalogError> {
    let imported = imported::catalog().map_err(CatalogError)?;
    imported
        .commands
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(&entry.name))
        .map(imported_command_spec)
        .ok_or_else(|| CatalogError(format!("missing imported runtime root {}", entry.name)))
}

fn merge_specs(mut imported: CommandSpec, overlay: CommandSpec) -> CommandSpec {
    debug_assert_eq!(imported.name, overlay.name);
    imported.description = overlay.description;
    for alias in overlay.aliases {
        if !imported.aliases.contains(&alias) {
            imported.aliases.push(alias);
        }
    }
    for option in overlay.options {
        let overlay_names = std::iter::once(option.name.as_str())
            .chain(option.aliases.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        imported.options.retain(|candidate| {
            std::iter::once(candidate.name.as_str())
                .chain(candidate.aliases.iter().map(String::as_str))
                .all(|name| !overlay_names.contains(name))
        });
        imported.options.push(option);
    }
    for child in overlay.subcommands {
        let original = imported
            .subcommands
            .iter()
            .position(|candidate| candidate.name == child.name)
            .map(|index| (index, imported.subcommands.remove(index)));
        let (index, child) = if let Some((index, original)) = original {
            (index, merge_specs(original, child))
        } else {
            (imported.subcommands.len(), child)
        };
        let child_names = std::iter::once(child.name.as_str())
            .chain(child.aliases.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        imported.subcommands.retain(|candidate| {
            std::iter::once(candidate.name.as_str())
                .chain(candidate.aliases.iter().map(String::as_str))
                .all(|name| !child_names.contains(name))
        });
        imported
            .subcommands
            .insert(index.min(imported.subcommands.len()), child);
    }
    if !overlay.generators.is_empty() {
        imported.generators = overlay.generators;
    }
    if overlay.max_positionals.is_some() {
        imported.max_positionals = overlay.max_positionals;
    }
    imported.priority = overlay.priority;
    imported.insertion = overlay.insertion;
    let global_names = imported
        .options
        .iter()
        .filter(|option| option.global)
        .flat_map(|option| {
            std::iter::once(option.name.clone()).chain(option.aliases.iter().cloned())
        })
        .collect::<BTreeSet<_>>();
    for child in &mut imported.subcommands {
        remove_inherited_options(child, &global_names);
    }
    imported
}

fn remove_inherited_options(command: &mut CommandSpec, inherited: &BTreeSet<String>) {
    command.options.retain(|option| {
        std::iter::once(option.name.as_str())
            .chain(option.aliases.iter().map(String::as_str))
            .all(|name| !inherited.contains(name))
    });
    let mut descendants = inherited.clone();
    descendants.extend(
        command
            .options
            .iter()
            .filter(|option| option.global)
            .flat_map(|option| {
                std::iter::once(option.name.clone()).chain(option.aliases.iter().cloned())
            }),
    );
    for child in &mut command.subcommands {
        remove_inherited_options(child, &descendants);
    }
}

fn kubectl_spec(description: &str) -> CommandSpec {
    CommandSpec::new("kubectl", description)
        .with_option(
            OptionSpec::new("--context", "select a Kubernetes context")
                .takes_value(true)
                .global(true),
        )
        .with_option(
            OptionSpec::new("--namespace", "select a namespace")
                .with_alias("-n")
                .takes_value(true)
                .global(true),
        )
        .with_subcommand(CommandSpec::new("apply", "apply a resource configuration"))
        .with_subcommand(CommandSpec::new("delete", "delete resources"))
        .with_subcommand(CommandSpec::new("describe", "show resource details"))
        .with_subcommand(CommandSpec::new("exec", "run a command in a container"))
        .with_subcommand(CommandSpec::new("get", "display resources"))
        .with_subcommand(CommandSpec::new("logs", "print container logs"))
}

/// Renders deterministic Markdown from the runtime catalog source.
///
/// # Errors
///
/// Returns an audit error rather than documenting an incomplete inventory.
pub fn markdown() -> Result<String, CatalogError> {
    let audit = audit()?;
    let mut output = String::from(
        "# Built-in command catalog\n\n\
         Generated from the checked-in IRIS snapshot used by argmax at runtime.\n\n\
         | Category | Count |\n\
         | --- | ---: |\n",
    );
    for category in Category::ALL {
        let count = audit.category_counts[&category];
        writeln!(output, "| {} | {count} |", category.label())
            .expect("writing to a String cannot fail");
    }
    writeln!(output, "| **Total** | **{}** |\n", audit.total)
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "The {total} documented records resolve to **{unique} unique command roots**; the duplicate `find` records are explicitly merged.\n",
        total = audit.total,
        unique = audit.unique_roots
    )
    .expect("writing to a String cannot fail");

    for category in Category::ALL {
        output.push_str("## ");
        output.push_str(category.label());
        output.push_str(
            "\n\n| Command | Description | IRIS source | Status | Subcommands | Options | Generators |\n\
             | --- | --- | --- | --- | ---: | ---: | ---: |\n",
        );
        let mut category_entries = entries()?
            .iter()
            .filter(|entry| entry.category == category)
            .collect::<Vec<_>>();
        category_entries.sort_unstable_by_key(|entry| entry.name.as_str());
        for entry in category_entries {
            let status = match &entry.status {
                MigrationStatus::Migrated | MigrationStatus::Inferred => {
                    entry.status.label().to_string()
                }
                MigrationStatus::Aliased { canonical } => format!("aliased to `{canonical}`"),
                MigrationStatus::Merged { canonical } => format!("merged into `{canonical}`"),
                MigrationStatus::Retired { reason } => format!("retired: {reason}"),
            };
            let structure = if matches!(&entry.status, MigrationStatus::Migrated) {
                Some(command_spec(entry)?)
            } else {
                None
            };
            let (subcommands, options, generators) =
                structure.as_ref().map_or((0, 0, 0), structure_counts);
            writeln!(
                output,
                "| `{}` | {} | `{}` | {} | {subcommands} | {options} | {generators} |",
                entry.name,
                entry.description.replace('|', "\\|"),
                entry.source,
                status
            )
            .expect("writing to a String cannot fail");
        }
        output.push('\n');
    }
    output.push_str(
        "## Supplemental runtime specifications\n\n\
         These live IRIS registry roots were absent from its generated inventory, or are Argmax-specific local integrations.\n\n\
         | Command | Description | Subcommands | Options | Generators |\n\
         | --- | --- | ---: | ---: | ---: |\n",
    );
    let inventory_names = entries()?
        .iter()
        .map(|entry| entry.name.to_lowercase())
        .collect::<BTreeSet<_>>();
    let imported = imported::catalog().map_err(CatalogError)?;
    let mut supplemental = imported
        .commands
        .iter()
        .filter(|command| !inventory_names.contains(&command.name.to_lowercase()))
        .map(imported_command_spec)
        .chain(representative::supplemental_specs())
        .collect::<Vec<_>>();
    supplemental.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    for spec in supplemental {
        let (subcommands, options, generators) = structure_counts(&spec);
        writeln!(
            output,
            "| `{}` | {} | {subcommands} | {options} | {generators} |",
            spec.name,
            spec.description.replace('|', "\\|")
        )
        .expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn structure_counts(spec: &CommandSpec) -> (usize, usize, usize) {
    let nested = spec
        .subcommands
        .iter()
        .map(structure_counts)
        .fold((0, 0, 0), |left, right| {
            (left.0 + right.0, left.1 + right.1, left.2 + right.2)
        });
    (
        spec.subcommands.len() + nested.0,
        spec.options.len() + nested.1,
        spec.generators.len() + nested.2,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{CompletionQuery, FilesystemGenerator, GeneratorKind, tokenize};

    #[test]
    fn audit_accounts_for_the_complete_baseline() {
        let audit = audit().unwrap();
        assert_eq!(audit.total, 567);
        assert_eq!(audit.unique_roots, 566);
        assert_eq!(audit.category_counts.len(), Category::ALL.len());
        assert_eq!(audit.status_counts.values().sum::<usize>(), 567);
        assert_eq!(audit.status_counts.get("merged"), Some(&1));
    }

    #[test]
    fn every_category_contributes_runtime_specs() {
        let all = entries().unwrap();
        for category in Category::ALL {
            assert!(all.iter().any(|entry| {
                entry.category == category && matches!(&entry.status, MigrationStatus::Migrated)
            }));
        }
        spec_index().unwrap();
        let rejected = rejected_specs().unwrap();
        assert!(
            rejected.is_empty(),
            "rejected imported specs: {rejected:#?}"
        );
    }

    #[test]
    fn every_category_has_a_structured_representative() {
        let representatives = [
            (Category::CloudDevOps, "docker"),
            (Category::JavaScript, "npm"),
            (Category::Python, "poetry"),
            (Category::Rust, "cargo"),
            (Category::Go, "go"),
            (Category::Jvm, "mvn"),
            (Category::Cpp, "cmake"),
            (Category::Git, "git"),
            (Category::PackageManagers, "apt"),
            (Category::Filesystem, "tar"),
            (Category::Editors, "vim"),
            (Category::Text, "jq"),
            (Category::TaskRunners, "just"),
            (Category::System, "systemctl"),
        ];
        let all = entries().unwrap();
        for (category, name) in representatives {
            let entry = all
                .iter()
                .find(|entry| entry.category == category && entry.name == name)
                .unwrap_or_else(|| panic!("missing representative {name}"));
            let spec = command_spec(entry).unwrap();
            assert!(
                !spec.subcommands.is_empty() || !spec.options.is_empty(),
                "representative {name} has no structured metadata"
            );
            spec.validate().unwrap();
        }
    }

    #[test]
    fn representative_recursive_specs_are_available() {
        let index = spec_index().unwrap();
        let query = CompletionQuery::new("git remote ", 11, "/tmp", 1).unwrap();
        let displays = index
            .suggestions(&query)
            .into_iter()
            .map(|suggestion| suggestion.display().to_string())
            .collect::<BTreeSet<_>>();
        assert!(displays.contains("add"));
        assert!(displays.contains("set-url"));

        let query = CompletionQuery::new("egr", 3, "/tmp", 2).unwrap();
        let aliases = index
            .suggestions(&query)
            .into_iter()
            .map(|suggestion| suggestion.display().to_string())
            .collect::<BTreeSet<_>>();
        assert!(aliases.contains("egrep"));
    }

    #[test]
    fn common_system_commands_offer_flags_after_the_command() {
        let index = spec_index().unwrap();
        for (line, expected) in [("ls -", ["-a", "-l"]), ("ps -", ["-e", "-o"])] {
            let query = CompletionQuery::new(line, line.len(), "/tmp", 1).unwrap();
            let displays = index
                .suggestions(&query)
                .into_iter()
                .map(|suggestion| suggestion.display().to_string())
                .collect::<BTreeSet<_>>();
            for option in expected {
                assert!(
                    displays.contains(option),
                    "{line:?} did not suggest {option:?}"
                );
            }
        }
    }

    #[test]
    fn every_documented_iris_root_is_available_at_runtime() {
        let documented = entries()
            .unwrap()
            .iter()
            .map(|entry| entry.name.to_lowercase())
            .collect::<BTreeSet<_>>();
        let runtime = spec_index()
            .unwrap()
            .roots()
            .iter()
            .map(|root| root.name.to_lowercase())
            .collect::<BTreeSet<_>>();
        assert_eq!(documented.len(), 566);
        assert!(documented.is_subset(&runtime));
        assert_eq!(runtime.len(), 576);
    }

    #[test]
    fn imported_shell_and_filesystem_specs_are_contextual() {
        let index = spec_index().unwrap();
        for root in ["cd", "command", "export", "unset", "chmod", "cat"] {
            assert!(index.roots().iter().any(|candidate| candidate.name == root));
        }

        let parsed = tokenize("cd ", 3).unwrap();
        let active = index.resolve(&parsed).unwrap().active_generators();
        assert!(active.iter().any(|generator| matches!(
            &generator.kind,
            GeneratorKind::Filesystem(filesystem) if filesystem.directory_only
        )));

        let parsed = tokenize("cat first ", 10).unwrap();
        let active = index.resolve(&parsed).unwrap().active_generators();
        assert!(
            active
                .iter()
                .any(|generator| matches!(&generator.kind, GeneratorKind::Filesystem(_)))
        );

        let query = CompletionQuery::new("chmod ", 6, "/tmp", 1).unwrap();
        let displays = index
            .suggestions(&query)
            .into_iter()
            .map(|suggestion| suggestion.display().to_owned())
            .collect::<BTreeSet<_>>();
        assert!(displays.contains("755"));
        assert!(displays.contains("u+x"));

        let parsed = tokenize("chmod 755 ", 10).unwrap();
        let active = index.resolve(&parsed).unwrap().active_generators();
        assert!(
            active
                .iter()
                .any(|generator| matches!(&generator.kind, GeneratorKind::Filesystem(_)))
        );
    }

    #[test]
    fn every_required_dynamic_behavior_is_reachable_from_the_runtime_index() {
        for (line, kind) in [
            ("git tag ", GeneratorKind::GitTags),
            ("pnpm run ", GeneratorKind::PackageScripts),
            ("yarn run ", GeneratorKind::PackageScripts),
            ("bun run ", GeneratorKind::PackageScripts),
            ("make ", GeneratorKind::MakeTargets),
            ("ssh ", GeneratorKind::SshHosts),
            ("zoxide query ", GeneratorKind::ZoxideDirectories),
            ("kill ", GeneratorKind::Processes),
            ("pacman ", GeneratorKind::Packages),
            ("dnf remove ", GeneratorKind::Packages),
            ("brew uninstall ", GeneratorKind::Packages),
            (
                "ls ",
                GeneratorKind::Filesystem(FilesystemGenerator::default()),
            ),
            ("ps ", GeneratorKind::Processes),
            ("printenv ", GeneratorKind::EnvironmentVariables),
            ("fd --extension ", GeneratorKind::FileTypes),
        ] {
            let parsed = tokenize(line, line.len()).unwrap();
            let resolution = spec_index()
                .unwrap()
                .resolve(&parsed)
                .unwrap_or_else(|| panic!("runtime index did not resolve {line:?}"));
            assert!(
                resolution
                    .active_generators()
                    .iter()
                    .any(|generator| generator.kind == kind),
                "{kind:?} is not active for {line:?}"
            );
        }
    }

    #[test]
    fn malformed_root_is_isolated_from_valid_specs() {
        let runtime = index_roots([
            CommandSpec::new("valid", "usable root"),
            CommandSpec::new("-invalid", "bad root"),
        ])
        .unwrap();
        assert_eq!(runtime.rejected.len(), 1);
        assert_eq!(runtime.rejected[0].name, "-invalid");

        let query = CompletionQuery::new("val", 3, "/tmp", 1).unwrap();
        let suggestions = runtime.index.suggestions(&query);
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.display() == "valid")
        );
    }

    #[test]
    fn generated_documentation_is_current() {
        assert_eq!(markdown().unwrap(), include_str!("../../docs/commands.md"));
    }
}
