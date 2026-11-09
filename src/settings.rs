//! App-scoped Codex settings. They live beside the boards instead of in
//! `~/.codex/config.toml`, so what this app asks Codex for never changes what
//! the CLI does everywhere else.

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_LINEAGE_REFS: usize = 6;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingsData {
    /// Passed as `codex exec -m`. Empty keeps whatever the CLI is configured with.
    pub model: String,
    /// Passed as `-c model_reasoning_effort=…`. Empty keeps the CLI's value.
    pub reasoning_effort: String,
    /// How many earlier images from a card's own chain are offered to Codex as
    /// extra references, newest first. Zero passes only the image being edited.
    pub lineage_refs: usize,
}

impl Default for SettingsData {
    fn default() -> Self {
        Self {
            model: String::new(),
            reasoning_effort: String::new(),
            lineage_refs: 3,
        }
    }
}

impl SettingsData {
    /// Drops anything a hand-edited file could carry that the CLI would reject.
    fn sanitize(&mut self) {
        self.model = self.model.trim().to_owned();
        self.reasoning_effort = self.reasoning_effort.trim().to_lowercase();
        // The CLI owns the list of efforts; only keep the value safe to splice
        // into its `-c model_reasoning_effort="…"` override.
        if !self
            .reasoning_effort
            .bytes()
            .all(|byte| byte.is_ascii_lowercase())
        {
            self.reasoning_effort.clear();
        }
        self.lineage_refs = self.lineage_refs.min(MAX_LINEAGE_REFS);
    }

    pub fn model_label(&self) -> &str {
        if self.model.is_empty() {
            "CLI default"
        } else {
            &self.model
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    data: Arc<RwLock<SettingsData>>,
    path: Arc<PathBuf>,
}

impl Settings {
    /// Reads `path`, falling back to defaults for a missing or unreadable file
    /// so a bad settings file can never keep the app from opening.
    pub fn load(path: PathBuf) -> Self {
        let mut data = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SettingsData>(&bytes).ok())
            .unwrap_or_default();
        data.sanitize();
        Self {
            data: Arc::new(RwLock::new(data)),
            path: Arc::new(path),
        }
    }

    pub fn get(&self) -> SettingsData {
        self.data.read().clone()
    }

    /// Applies `change` and writes the file. The file is small and changes are
    /// user-driven, so this saves inline rather than through a writer thread.
    pub fn update(&self, change: impl FnOnce(&mut SettingsData)) -> Result<()> {
        let snapshot = {
            let mut data = self.data.write();
            change(&mut data);
            data.sanitize();
            data.clone()
        };
        crate::storage::atomic_write(&self.path, &serde_json::to_vec_pretty(&snapshot)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_LINEAGE_REFS, Settings, SettingsData};

    #[test]
    fn settings_round_trip_through_disk_and_reject_unsafe_effort() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let settings = Settings::load(path.clone());
        assert_eq!(settings.get(), SettingsData::default());

        settings
            .update(|data| {
                data.model = "  gpt-5.1-codex  ".into();
                data.reasoning_effort = "HIGH".into();
                data.lineage_refs = 99;
            })
            .unwrap();

        let reloaded = Settings::load(path).get();
        assert_eq!(reloaded.model, "gpt-5.1-codex");
        assert_eq!(reloaded.reasoning_effort, "high");
        assert_eq!(reloaded.lineage_refs, MAX_LINEAGE_REFS);

        let settings = Settings::load(directory.path().join("other.json"));
        settings
            .update(|data| data.reasoning_effort = "high\" -c x=\"y".into())
            .unwrap();
        assert!(settings.get().reasoning_effort.is_empty());
    }
}
