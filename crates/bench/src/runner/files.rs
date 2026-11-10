//! Files an app needs in place for a run, such as its configuration, with the originals backed up and restored.
//!
//! The backup directory holds a manifest that is written before anything is overwritten, so a series that was
//! interrupted is restored by the next one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MANIFEST: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    to: PathBuf,
    /// The original file, or `None` when there was none and the placed file is removed on restore.
    backup: Option<PathBuf>,
}

fn write_manifest(dir: &Path, entries: &[Entry]) -> io::Result<()> {
    let text = serde_json::to_string_pretty(entries).map_err(io::Error::other)?;
    fs::write(dir.join(MANIFEST), text)
}

/// Copies each `(from, to)` pair, backing up existing destinations in `backup_dir`. Fails if a previous placement
/// was not restored.
pub fn place(backup_dir: &Path, files: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    if backup_dir.join(MANIFEST).exists() {
        return Err(io::Error::other(format!(
            "{} holds files that were not restored",
            backup_dir.display()
        )));
    }
    if let Some((from, _)) = files.iter().find(|(from, _)| !from.is_file()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", from.display()),
        ));
    }
    fs::create_dir_all(backup_dir)?;
    let mut entries = Vec::with_capacity(files.len());
    for (i, (from, to)) in files.iter().enumerate() {
        let backup = if to.exists() {
            let backup = backup_dir.join(format!("{i}.bak"));
            fs::copy(to, &backup)?;
            Some(backup)
        } else {
            None
        };
        entries.push(Entry {
            to: to.clone(),
            backup,
        });
        // Recorded before the destination changes, so an interruption from here on is restorable.
        write_manifest(backup_dir, &entries)?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to)?;
    }
    Ok(())
}

/// Puts back what [`place`] replaced and removes what it added. Returns the restored destinations; nothing to do
/// when there is no manifest.
pub fn restore(backup_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let manifest = backup_dir.join(MANIFEST);
    let text = match fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let entries: Vec<Entry> = serde_json::from_str(&text).map_err(io::Error::other)?;
    // Later entries first, in case two of them name the same destination.
    for entry in entries.iter().rev() {
        match &entry.backup {
            Some(backup) => {
                fs::copy(backup, &entry.to)?;
            }
            None => match fs::remove_file(&entry.to) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            },
        }
    }
    for backup in entries.iter().filter_map(|e| e.backup.as_ref()) {
        let _ = fs::remove_file(backup);
    }
    fs::remove_file(&manifest)?;
    Ok(entries.into_iter().map(|e| e.to).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pitot-bench-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn places_and_restores() {
        let dir = temp("restore");
        let backups = dir.join("backup");
        let (source_a, source_b) = (dir.join("a.toml"), dir.join("b.toml"));
        fs::write(&source_a, "bench a").unwrap();
        fs::write(&source_b, "bench b").unwrap();
        let existing = dir.join("app/config.toml");
        fs::create_dir_all(existing.parent().unwrap()).unwrap();
        fs::write(&existing, "user settings").unwrap();
        let added = dir.join("app/new/extra.toml");

        let files = [(source_a, existing.clone()), (source_b, added.clone())];
        place(&backups, &files).expect("place");
        assert_eq!(fs::read_to_string(&existing).unwrap(), "bench a");
        assert_eq!(fs::read_to_string(&added).unwrap(), "bench b");
        assert!(place(&backups, &files).is_err(), "not restored yet");

        let restored = restore(&backups).expect("restore");
        assert_eq!(restored, [existing.clone(), added.clone()]);
        assert_eq!(fs::read_to_string(&existing).unwrap(), "user settings");
        assert!(!added.exists());
        assert_eq!(
            restore(&backups).expect("nothing left"),
            Vec::<PathBuf>::new()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_source_changes_nothing() {
        let dir = temp("missing");
        let target = dir.join("config.toml");
        fs::write(&target, "user").unwrap();
        let result = place(
            &dir.join("backup"),
            &[(dir.join("nope.toml"), target.clone())],
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "user");
        let _ = fs::remove_dir_all(&dir);
    }
}
