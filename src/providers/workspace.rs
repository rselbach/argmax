//! Workspace signature detection.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Ecosystem inferred from a workspace signature file or directory.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WorkspaceKind {
    /// Git repository metadata.
    Git,
    /// Node.js or JavaScript package metadata.
    Node,
    /// Go module or workspace metadata.
    Go,
    /// Rust package metadata.
    Rust,
    /// Python project metadata.
    Python,
    /// Docker build or Compose metadata.
    Docker,
    /// Make build metadata.
    Make,
    /// Just task-runner metadata.
    Just,
    /// Kubernetes, Kustomize, or Helm metadata.
    Kubernetes,
}

/// One detected workspace marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSignature {
    /// Ecosystem represented by the marker.
    pub kind: WorkspaceKind,
    /// Directory containing the marker.
    pub root: PathBuf,
    /// Marker path used for detection.
    pub marker: PathBuf,
}

/// Detected workspace context, nearest markers first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceContext {
    /// Directory from which detection began.
    pub cwd: PathBuf,
    /// At most one nearest marker for each ecosystem.
    pub signatures: Vec<WorkspaceSignature>,
}

impl WorkspaceContext {
    /// Returns whether an ecosystem was detected.
    #[must_use]
    pub fn contains(&self, kind: WorkspaceKind) -> bool {
        self.signatures
            .iter()
            .any(|signature| signature.kind == kind)
    }

    /// Returns detected ecosystem kinds in nearest-marker order.
    pub fn kinds(&self) -> impl Iterator<Item = WorkspaceKind> + '_ {
        self.signatures.iter().map(|signature| signature.kind)
    }
}

/// Detects supported workspace signatures in `cwd` and its ancestors.
///
/// Missing, unreadable, and concurrently disappearing paths are treated as
/// absent. For each ecosystem, the nearest marker wins.
#[must_use]
pub fn detect_workspace(cwd: &Path) -> WorkspaceContext {
    let mut detected = BTreeSet::new();
    let mut signatures = Vec::new();

    for directory in cwd.ancestors() {
        for (kind, marker_names) in signature_groups() {
            if detected.contains(&kind) {
                continue;
            }
            let Some(marker) = marker_names
                .iter()
                .map(|name| directory.join(name))
                .find(|path| marker_exists(path, kind))
            else {
                continue;
            };
            detected.insert(kind);
            signatures.push(WorkspaceSignature {
                kind,
                root: directory.to_path_buf(),
                marker,
            });
        }
    }

    WorkspaceContext {
        cwd: cwd.to_path_buf(),
        signatures,
    }
}

fn signature_groups() -> [(WorkspaceKind, &'static [&'static str]); 9] {
    [
        (WorkspaceKind::Git, &[".git"]),
        (
            WorkspaceKind::Node,
            &[
                "package.json",
                "pnpm-workspace.yaml",
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "bun.lock",
                "bun.lockb",
            ],
        ),
        (WorkspaceKind::Go, &["go.mod", "go.work"]),
        (WorkspaceKind::Rust, &["Cargo.toml"]),
        (
            WorkspaceKind::Python,
            &[
                "pyproject.toml",
                "requirements.txt",
                "setup.py",
                "Pipfile",
                "poetry.lock",
            ],
        ),
        (
            WorkspaceKind::Docker,
            &[
                "Dockerfile",
                "docker-compose.yml",
                "docker-compose.yaml",
                "compose.yml",
                "compose.yaml",
            ],
        ),
        (
            WorkspaceKind::Make,
            &["Makefile", "makefile", "GNUmakefile"],
        ),
        (WorkspaceKind::Just, &["justfile", "Justfile"]),
        (
            WorkspaceKind::Kubernetes,
            &["kustomization.yaml", "kustomization.yml", "Chart.yaml"],
        ),
    ]
}

fn marker_exists(path: &Path, kind: WorkspaceKind) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() || (kind == WorkspaceKind::Git && metadata.is_dir())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-workspace-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn detects_signatures_in_current_directory_and_ancestors() {
        let temp = TempDirectory::new();
        fs::create_dir(temp.0.join(".git")).unwrap();
        fs::write(temp.0.join("package.json"), "{}").unwrap();
        fs::write(temp.0.join("go.mod"), "module greendale").unwrap();
        fs::write(temp.0.join("Cargo.toml"), "").unwrap();
        fs::write(temp.0.join("pyproject.toml"), "").unwrap();
        fs::write(temp.0.join("Dockerfile"), "").unwrap();
        fs::write(temp.0.join("Makefile"), "").unwrap();
        fs::write(temp.0.join("justfile"), "").unwrap();
        fs::write(temp.0.join("kustomization.yaml"), "").unwrap();
        let nested = temp.0.join("school/study-room");
        fs::create_dir_all(&nested).unwrap();

        let context = detect_workspace(&nested);
        assert_eq!(context.signatures.len(), 9);
        for kind in [
            WorkspaceKind::Git,
            WorkspaceKind::Node,
            WorkspaceKind::Go,
            WorkspaceKind::Rust,
            WorkspaceKind::Python,
            WorkspaceKind::Docker,
            WorkspaceKind::Make,
            WorkspaceKind::Just,
            WorkspaceKind::Kubernetes,
        ] {
            assert!(context.contains(kind));
        }
    }

    #[test]
    fn nearest_marker_wins_for_each_ecosystem() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("Cargo.toml"), "").unwrap();
        let nested = temp.0.join("annex");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("Cargo.toml"), "").unwrap();

        let context = detect_workspace(&nested);
        assert_eq!(context.signatures.len(), 1);
        assert_eq!(context.signatures[0].root, nested);
    }

    #[test]
    fn missing_directory_produces_empty_context() {
        let temp = TempDirectory::new();
        let missing = temp.0.join("missing/child");
        let context = detect_workspace(&missing);

        assert!(context.signatures.is_empty());
        assert_eq!(context.cwd, missing);
    }
}
