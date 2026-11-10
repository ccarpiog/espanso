//! Recovery data lives in the local runtime directory, never in synced match files.
use anyhow::Result;
use espanso_config::matches::Match;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FormAction {
    Copy,
    Insert,
    Cancel,
}

#[derive(Deserialize)]
pub struct FormResponse {
    pub action: FormAction,
    pub values: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastForm {
    pub fingerprint: String,
    pub context_key: String,
    pub trigger: Option<String>,
    pub args: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedResult {
    pub text: String,
    pub html: Option<String>,
}

pub struct CompletedForm {
    pub result: SavedResult,
    pub cursor_moves: usize,
    pub insert: bool,
}

#[derive(Default)]
struct Session {
    #[cfg(any(target_os = "macos", test))]
    capture_attempted: bool,
    used: bool,
    copy_requested: bool,
    cancelled: bool,
    form_index: usize,
    current: Option<LastForm>,
    reopen_context: Option<String>,
    completed: Option<CompletedForm>,
    persistence_failed: bool,
    #[cfg(target_os = "macos")]
    target: Option<espanso_mac_utils::FocusTarget>,
    #[cfg(target_os = "macos")]
    clipboard_snapshot: Option<espanso_mac_utils::ClipboardSnapshot>,
}

pub struct FormRecovery {
    directory: PathBuf,
    session: RefCell<Session>,
    last_result_memory: RefCell<Option<SavedResult>>,
}

impl FormRecovery {
    pub fn new(runtime: &Path) -> Self {
        Self {
            directory: runtime.join("form-recovery"),
            session: RefCell::default(),
            last_result_memory: RefCell::default(),
        }
    }

    pub fn reset(&self) {
        *self.session.borrow_mut() = Session::default();
    }
    pub fn used(&self) -> bool {
        self.session.borrow().used
    }

    #[cfg(test)]
    pub(crate) fn prevent_target_capture_for_test(&self) {
        // Transaction tests exercise persistence and routing without inspecting
        // or changing the developer's real foreground application.
        self.session.borrow_mut().capture_attempted = true;
    }

    /// Called before either the search UI or the first form can take focus.
    pub fn capture_target(&self) {
        #[cfg(target_os = "macos")]
        {
            let mut session = self.session.borrow_mut();
            if !session.capture_attempted {
                session.capture_attempted = true;
                session.target = espanso_mac_utils::FocusTarget::capture();
            }
        }
    }

    pub fn prepare(&self, last: LastForm) {
        self.session.borrow_mut().current = Some(last);
    }
    pub fn set_reopen_context(&self, key: String) {
        self.session.borrow_mut().reopen_context = Some(key);
    }
    pub fn reopen_context(&self) -> Option<String> {
        self.session.borrow().reopen_context.clone()
    }

    pub fn open_form(&self, schema: &str) -> Result<PathBuf> {
        self.capture_target();
        let mut session = self.session.borrow_mut();
        session.used = true;
        let last = session
            .current
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing form context"))?;
        self.save("last-form.json", last)?;
        let key = digest(&format!(
            "v1:{}:{}:{}:{schema}",
            last.fingerprint, last.context_key, session.form_index
        ));
        session.form_index += 1;
        Ok(self.directory.join(format!("{key}.json")))
    }

    pub fn select(&self, action: FormAction) {
        let mut session = self.session.borrow_mut();
        session.copy_requested |= action == FormAction::Copy;
        session.cancelled |= action == FormAction::Cancel;
    }

    pub fn finish(&self, result: SavedResult, cursor_moves: usize) -> Result<()> {
        *self.last_result_memory.borrow_mut() = Some(result.clone());
        let mut session = self.session.borrow_mut();
        let insert = !session.copy_requested && !session.cancelled;
        session.completed = Some(CompletedForm {
            result: result.clone(),
            cursor_moves,
            insert,
        });
        // A disk failure must not discard the in-memory result or authorize insertion.
        if let Err(err) = self.save("last-result.json", &result) {
            session.persistence_failed = true;
            session.completed.as_mut().unwrap().insert = false;
            return Err(err);
        }
        Ok(())
    }

    pub fn take_completed(&self) -> Option<CompletedForm> {
        self.session.borrow_mut().completed.take()
    }
    pub fn persistence_failed(&self) -> bool {
        self.session.borrow().persistence_failed
    }
    pub fn last_form(&self) -> Result<LastForm> {
        self.load("last-form.json")
    }
    pub fn last_result(&self) -> Result<SavedResult> {
        if let Some(result) = self.last_result_memory.borrow().as_ref() {
            return Ok(result.clone());
        }
        self.load("last-result.json")
    }

    pub fn restore_target(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.session
                .borrow()
                .target
                .as_ref()
                .is_some_and(|target| target.restore_and_verify())
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn capture_clipboard(&self, expected: &SavedResult) -> bool {
        #[cfg(target_os = "macos")]
        {
            let snapshot = espanso_mac_utils::ClipboardSnapshot::capture(&expected.text);
            let success = snapshot.is_some();
            self.session.borrow_mut().clipboard_snapshot = snapshot;
            success
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = expected;
            false
        }
    }

    pub fn paste_from_clipboard(
        &self,
        backspaces: usize,
        cursor_moves: usize,
        delay_ms: u32,
    ) -> bool {
        #[cfg(target_os = "macos")]
        {
            let session = self.session.borrow();
            let trigger = session
                .current
                .as_ref()
                .and_then(|last| last.trigger.as_deref())
                .unwrap_or("");
            if backspaces > trigger.chars().count() {
                return false;
            }
            let deletion: String = trigger
                .chars()
                .skip(trigger.chars().count() - backspaces)
                .collect();
            let Some(snapshot) = &session.clipboard_snapshot else {
                return false;
            };
            session.target.as_ref().is_some_and(|target| {
                target.paste_from_clipboard(&deletion, cursor_moves, delay_ms, snapshot)
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (backspaces, cursor_moves, delay_ms);
            false
        }
    }

    fn save(&self, name: &str, value: &impl Serialize) -> Result<()> {
        private_directory(&self.directory)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(name))?;
        Ok(())
    }

    fn load<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        Ok(serde_json::from_slice(&std::fs::read(
            self.directory.join(name),
        )?)?)
    }
}

fn private_directory(path: &Path) -> Result<()> {
    if path.exists() && std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        anyhow::bail!("form recovery directory must not be a symlink");
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub fn fingerprint(m: &Match) -> String {
    let mut normalized = m.clone();
    normalized.id = 0;
    if let espanso_config::matches::MatchEffect::Text(effect) = &mut normalized.effect {
        for var in &mut effect.vars {
            var.id = 0;
        }
    }
    digest(&format!("v1:{normalized:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_is_sticky_across_forms_and_results_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let recovery = FormRecovery::new(dir.path());
        recovery.select(FormAction::Copy);
        recovery.select(FormAction::Insert);
        recovery
            .finish(
                SavedResult {
                    text: "kept".into(),
                    html: None,
                },
                0,
            )
            .unwrap();
        assert!(!recovery.take_completed().unwrap().insert);
        assert_eq!(
            FormRecovery::new(dir.path()).last_result().unwrap().text,
            "kept"
        );
        recovery.reset();
        assert!(!recovery.used());
        assert!(recovery.take_completed().is_none());
    }

    #[test]
    fn fingerprints_ignore_runtime_ids_but_separate_snippets() {
        let mut m = Match::default();
        let before = fingerprint(&m);
        m.id = 123;
        assert_eq!(before, fingerprint(&m));
        m.label = Some("different".into());
        assert_ne!(before, fingerprint(&m));
    }

    #[cfg(unix)]
    #[test]
    fn saved_results_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let recovery = FormRecovery::new(dir.path());
        recovery
            .finish(
                SavedResult {
                    text: "private".into(),
                    html: None,
                },
                0,
            )
            .unwrap();
        assert_eq!(
            std::fs::metadata(&recovery.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(recovery.directory.join("last-result.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
