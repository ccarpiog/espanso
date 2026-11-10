//! Local form recovery storage. Drafts never belong in the synced match directory.

use std::{collections::HashMap, fs, io::Write, path::Path};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    // Lists retain individual selections, rather than splitting a joined display value.
    pub fields: HashMap<String, Vec<String>>,
}

const MAX_DRAFT_BYTES: u64 = 4 * 1024 * 1024;

pub fn load(path: &Path) -> Result<Option<Draft>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_DRAFT_BYTES {
        bail!("Invalid form draft file");
    }
    let draft: Draft =
        serde_json::from_slice(&fs::read(path)?).context("Unable to read saved form entries")?;
    if draft
        .fields
        .iter()
        .any(|(key, values)| key.contains('\0') || values.iter().any(|value| value.contains('\0')))
    {
        bail!("Saved form entries contain invalid characters");
    }
    Ok(Some(draft))
}

pub fn save(path: &Path, draft: &Draft) -> Result<()> {
    let parent = path
        .parent()
        .context("Draft path has no parent directory")?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() {
        bail!("Form draft directory is not a directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Form draft directory must be private (permissions 0700)");
        }
    }
    let data = serde_json::to_vec(draft)?;
    if data.len() as u64 > MAX_DRAFT_BYTES {
        bail!("Form draft exceeds the 4 MiB limit");
    }
    // NamedTempFile uses private permissions and an exclusive random file name.
    // Persist atomically replaces the previous complete draft, including on Windows.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&data)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_lists_empty_selections_and_unicode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/entries.json");
        assert_eq!(load(&path).unwrap(), None);
        let draft = Draft {
            fields: HashMap::from([
                ("list".into(), vec!["one, two".into(), "日本語".into()]),
                ("empty-list".into(), vec![]),
                ("text".into(), vec!["line 1\nline 2".into()]),
            ]),
        };
        save(&path, &draft).unwrap();
        assert_eq!(load(&path).unwrap(), Some(draft));
        save(&path, &Draft::default()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(Draft::default()));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_draft_is_reported_and_not_changed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("entries.json");
        fs::write(&path, b"{broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn failed_oversized_write_keeps_last_good_draft() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/entries.json");
        save(&path, &Draft::default()).unwrap();
        let draft = Draft {
            fields: HashMap::from([("text".into(), vec!["x".repeat(MAX_DRAFT_BYTES as usize)])]),
        };
        assert!(save(&path, &draft).is_err());
        assert_eq!(load(&path).unwrap(), Some(Draft::default()));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_drafts_and_public_directories() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("draft.json");
        symlink("missing", &path).unwrap();
        assert!(load(&path).is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(save(&path, &Draft::default()).is_err());
    }
}
