//! Regenerates the checked-in command catalog documentation.

use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let documentation = root.join("docs/commands.md");
    let baseline = root.join("src/catalog/baseline.tsv");
    let mut arguments = env::args().skip(1);
    let operation = arguments.next();
    if arguments.next().is_some() {
        return Err("expected at most one argument".into());
    }

    match operation.as_deref() {
        None => write_atomic(&documentation, &argmax::catalog::markdown()?)?,
        Some("--check") => check_current(&documentation, &argmax::catalog::markdown()?)?,
        Some("--freeze-baseline") => {
            write_atomic(&baseline, &argmax::catalog::baseline_manifest()?)?;
        }
        Some(argument) => return Err(format!("unknown argument {argument:?}").into()),
    }
    Ok(())
}

fn check_current(path: &Path, wanted: &str) -> io::Result<()> {
    let actual = fs::read_to_string(path)?;
    if actual == wanted {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} is stale; regenerate it with `cargo run --example generate_catalog`",
        path.display()
    )))
}

fn write_atomic(destination: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(destination);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(destination: &Path) -> PathBuf {
    let filename = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog");
    destination.with_file_name(format!(".{filename}.tmp-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_writer_replaces_complete_files_and_check_detects_staleness() {
        let destination = env::temp_dir().join(format!(
            "argmax-catalog-generator-test-{}.md",
            std::process::id()
        ));
        let temporary = temporary_path(&destination);
        let _ = fs::remove_file(&destination);
        let _ = fs::remove_file(&temporary);

        write_atomic(&destination, "first\n").unwrap();
        write_atomic(&destination, "second\n").unwrap();
        check_current(&destination, "second\n").unwrap();
        assert!(check_current(&destination, "stale\n").is_err());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "second\n");
        assert!(!temporary.exists());

        fs::remove_file(destination).unwrap();
    }
}
