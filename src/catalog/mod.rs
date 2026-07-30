//! Audited source of the built-in command catalog.
//!
//! Runtime specifications and generated documentation both consume these
//! entries. The baseline counts intentionally match the migration inventory in
//! the product requirements.

mod part_a;
mod part_b;
mod part_c;
mod part_d;
mod representative;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Write as _};
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
}

/// How one baseline command is represented by the Rust implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationStatus {
    /// A built-in top-level specification exists under this name.
    Migrated,
    /// The baseline name is represented by another canonical specification.
    Aliased { canonical: &'static str },
    /// Runtime inference deliberately replaces a static definition.
    Inferred,
    /// The baseline entry is deliberately unsupported.
    Retired { reason: &'static str },
}

impl MigrationStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Migrated => "migrated",
            Self::Aliased { .. } => "aliased",
            Self::Inferred => "inferred",
            Self::Retired { .. } => "retired",
        }
    }
}

/// One audited top-level command from the migration inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogEntry {
    /// Executable basename or baseline identity.
    pub name: &'static str,
    /// Concise user-facing purpose.
    pub description: &'static str,
    /// Baseline category.
    pub category: Category,
    /// Explicit migration disposition.
    pub status: MigrationStatus,
}

impl CatalogEntry {
    /// Defines a built-in command specification.
    #[must_use]
    pub const fn migrated(
        category: Category,
        name: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            description,
            category,
            status: MigrationStatus::Migrated,
        }
    }

    /// Defines a baseline command name represented by a canonical specification.
    #[must_use]
    pub const fn aliased(
        category: Category,
        name: &'static str,
        description: &'static str,
        canonical: &'static str,
    ) -> Self {
        Self {
            name,
            description,
            category,
            status: MigrationStatus::Aliased { canonical },
        }
    }
}

/// Successful catalog audit summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogAudit {
    /// Accounted top-level baseline entries.
    pub total: usize,
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

/// Returns every baseline entry in stable category order.
#[must_use]
pub fn entries() -> Vec<&'static CatalogEntry> {
    [
        part_a::ENTRIES,
        part_b::ENTRIES,
        part_c::ENTRIES,
        part_d::ENTRIES,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Verifies category totals, identity uniqueness, and explicit dispositions.
///
/// # Errors
///
/// Returns a descriptive error for the first incomplete or malformed catalog
/// invariant.
pub fn audit() -> Result<CatalogAudit, CatalogError> {
    let all_entries = entries();
    let mut names = BTreeSet::new();
    let mut category_counts = BTreeMap::new();
    let mut status_counts = BTreeMap::new();

    for entry in &all_entries {
        if !valid_executable_basename(entry.name) {
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
        if !names.insert(entry.name.to_lowercase()) {
            return Err(CatalogError(format!(
                "duplicate top-level command name {}",
                entry.name
            )));
        }
        *category_counts.entry(entry.category).or_insert(0) += 1;
        *status_counts.entry(entry.status.label()).or_insert(0) += 1;

        match entry.status {
            MigrationStatus::Aliased { canonical: "" } => {
                return Err(CatalogError(format!(
                    "{} has an empty alias target",
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

    for entry in &all_entries {
        let MigrationStatus::Aliased { canonical } = entry.status else {
            continue;
        };
        if entry.name.eq_ignore_ascii_case(canonical) {
            return Err(CatalogError(format!("{} aliases itself", entry.name)));
        }
        let Some(target) = all_entries
            .iter()
            .find(|target| target.name.eq_ignore_ascii_case(canonical))
        else {
            return Err(CatalogError(format!(
                "{} aliases missing canonical command {canonical}",
                entry.name
            )));
        };
        if !matches!(target.status, MigrationStatus::Migrated) {
            return Err(CatalogError(format!(
                "{} aliases non-migrated command {canonical}",
                entry.name
            )));
        }
    }

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

    let total = names.len();
    if total != 567 {
        return Err(CatalogError(format!(
            "catalog contains {total} unique entries; expected 567"
        )));
    }

    if baseline_manifest() != include_str!("baseline.tsv") {
        return Err(CatalogError(
            "catalog identities differ from baseline.tsv; review the migration and explicitly freeze the new baseline"
                .into(),
        ));
    }

    Ok(CatalogAudit {
        total,
        category_counts,
        status_counts,
    })
}

fn valid_executable_basename(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

/// Renders the frozen identity inventory used to detect accidental substitutions.
///
/// This deliberately excludes migration status: a reviewed entry may move from
/// a static spec to an alias or inference provider without changing the baseline
/// identity it accounts for.
#[must_use]
pub fn baseline_manifest() -> String {
    let mut catalog_entries = entries();
    catalog_entries.sort_unstable_by_key(|entry| (entry.category, entry.name));
    let mut output = String::from("# argmax equivalent catalog baseline v1\n");
    for entry in catalog_entries {
        writeln!(output, "{}\t{}", entry.category.key(), entry.name)
            .expect("writing to a String cannot fail");
    }
    output
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
    audit()?;
    let all_entries = entries();
    let generated = all_entries
        .iter()
        .filter_map(|entry| {
            if !matches!(entry.status, MigrationStatus::Migrated) {
                return None;
            }
            let mut root = command_spec(entry);
            for alias in all_entries.iter().filter(|alias| {
                matches!(
                    alias.status,
                    MigrationStatus::Aliased { canonical }
                        if canonical.eq_ignore_ascii_case(entry.name)
                )
            }) {
                root = root.with_alias(alias.name);
            }
            Some(root)
        })
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

fn command_spec(entry: &CatalogEntry) -> CommandSpec {
    if let Some(spec) = representative::spec(entry.name, entry.description) {
        return spec;
    }
    match entry.name {
        "kubectl" => kubectl_spec(entry.description),
        _ => CommandSpec::new(entry.name, entry.description),
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
         Generated from the same audited source used by argmax at runtime.\n\n\
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

    for category in Category::ALL {
        output.push_str("## ");
        output.push_str(category.label());
        output.push_str(
            "\n\n| Command | Description | Status | Subcommands | Options | Generators |\n\
             | --- | --- | --- | ---: | ---: | ---: |\n",
        );
        let mut category_entries = entries()
            .into_iter()
            .filter(|entry| entry.category == category)
            .collect::<Vec<_>>();
        category_entries.sort_unstable_by_key(|entry| entry.name);
        for entry in category_entries {
            let status = match entry.status {
                MigrationStatus::Migrated | MigrationStatus::Inferred => {
                    entry.status.label().to_string()
                }
                MigrationStatus::Aliased { canonical } => format!("aliased to `{canonical}`"),
                MigrationStatus::Retired { reason } => format!("retired: {reason}"),
            };
            let structure =
                matches!(entry.status, MigrationStatus::Migrated).then(|| command_spec(entry));
            let (subcommands, options, generators) =
                structure.as_ref().map_or((0, 0, 0), structure_counts);
            writeln!(
                output,
                "| `{}` | {} | {} | {subcommands} | {options} | {generators} |",
                entry.name,
                entry.description.replace('|', "\\|"),
                status
            )
            .expect("writing to a String cannot fail");
        }
        output.push('\n');
    }
    output.push_str(
        "## Supplemental runtime specifications\n\n\
         These installed tools extend the frozen migration baseline with required local generators.\n\n\
         | Command | Description | Subcommands | Options | Generators |\n\
         | --- | --- | ---: | ---: | ---: |\n",
    );
    for spec in representative::supplemental_specs() {
        let (subcommands, options, generators) = structure_counts(&spec);
        writeln!(
            output,
            "| `{}` | {} | {subcommands} | {options} | {generators} |",
            spec.name,
            spec.description.replace('|', "\\|")
        )
        .expect("writing to a String cannot fail");
    }
    output.push('\n');
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
    use crate::completion::{CompletionQuery, GeneratorKind, tokenize};

    #[test]
    fn audit_accounts_for_the_complete_baseline() {
        let audit = audit().unwrap();
        assert_eq!(audit.total, 567);
        assert_eq!(audit.category_counts.len(), Category::ALL.len());
        assert_eq!(audit.status_counts.values().sum::<usize>(), 567);
        assert_eq!(audit.status_counts.get("aliased"), Some(&4));
    }

    #[test]
    fn every_category_contributes_runtime_specs() {
        let all = entries();
        for category in Category::ALL {
            assert!(all.iter().any(|entry| {
                entry.category == category && matches!(entry.status, MigrationStatus::Migrated)
            }));
        }
        spec_index().unwrap();
        assert!(rejected_specs().unwrap().is_empty());
    }

    #[test]
    fn every_category_has_a_structured_representative() {
        let representatives = [
            (Category::CloudDevOps, "docker"),
            (Category::JavaScript, "npm"),
            (Category::Python, "pip"),
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
        let all = entries();
        for (category, name) in representatives {
            let entry = all
                .iter()
                .find(|entry| entry.category == category && entry.name == name)
                .unwrap_or_else(|| panic!("missing representative {name}"));
            let spec = command_spec(entry);
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
