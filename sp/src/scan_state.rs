//! Scan state tracking for Silent Payment blockchain scanning.
//!
//! The `ScanState` tracks progress of blockchain scanning, including the last
//! scanned block height and hash (for reorg detection), and the wallet's
//! birthday height.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ScanStateError

/// Errors that can occur in the scan state.
#[derive(Debug, thiserror::Error)]
pub enum ScanStateError {
    /// IO error (file not found, permission denied, etc.)
    #[error("io error: {0}")]
    Io(String),
    /// JSON parsing error
    #[error("parse error: {0}")]
    Parse(String),
}

// ScanState

/// Tracks blockchain scanning progress for Silent Payment wallets.
///
/// This struct maintains the scan position (last scanned height and block hash)
/// and the wallet's birthday height. The block hash is stored for reorg detection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanState {
    /// The last successfully scanned block height (None if never scanned)
    last_scanned_height: Option<u32>,

    /// The hash of the last scanned block (for reorg detection)
    last_block_hash: Option<[u8; 32]>,

    /// The wallet's birthday height (where scanning should start)
    birthday_height: u32,

    /// Directory containing the JSON file, if persistence is enabled (not
    /// serialized).
    #[serde(skip)]
    dir: Option<PathBuf>,

    /// Whether persistence is enabled (not serialized, defaults to true)
    #[serde(skip)]
    persist: bool,
}

impl ScanState {
    /// Filename used under the account directory for this store's JSON.
    pub const FILENAME: &'static str = "state.json";

    // Constructors

    /// Create a new scan state with the given birthday height.
    pub fn new(birthday_height: u32) -> Self {
        Self {
            last_scanned_height: None,
            last_block_hash: None,
            birthday_height,
            dir: None,
            persist: true,
        }
    }

    /// Create a new scan state rooted at the given directory.
    ///
    /// The state persists to `{dir}/{FILENAME}`.
    pub fn with_path(birthday_height: u32, dir: PathBuf) -> Self {
        Self {
            last_scanned_height: None,
            last_block_hash: None,
            birthday_height,
            dir: Some(dir),
            persist: true,
        }
    }

    /// Load a scan state from `{dir}/{FILENAME}`.
    ///
    /// The loaded state will have its dir set but persist disabled.
    /// Call `enable_persist(true)` to enable persistence.
    pub fn from_file(dir: PathBuf) -> Result<Self, ScanStateError> {
        let path = dir.join(Self::FILENAME);
        let content = fs::read_to_string(&path).map_err(|e| {
            ScanStateError::Io(format!(
                "failed to read scan state from {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut state: ScanState = serde_json::from_str(&content)
            .map_err(|e| ScanStateError::Parse(format!("failed to parse scan state: {}", e)))?;
        state.dir = Some(dir);
        state.persist = false;
        Ok(state)
    }

    /// Enable or disable persistence (builder pattern).
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    } // Getters

    /// Returns the last scanned block height, if any.
    pub fn last_scanned_height(&self) -> Option<u32> {
        self.last_scanned_height
    }

    /// Returns the last scanned block hash, for reorg detection.
    pub fn last_block_hash(&self) -> Option<[u8; 32]> {
        self.last_block_hash
    }

    /// Returns the wallet's birthday height.
    pub fn birthday_height(&self) -> u32 {
        self.birthday_height
    }

    /// Returns the persistence directory, if set.
    pub fn path(&self) -> Option<&PathBuf> {
        self.dir.as_ref()
    }

    /// Returns whether persistence is enabled.
    pub fn is_persist_enabled(&self) -> bool {
        self.persist
    } // Mutators

    /// Update the scan state with a new height and block hash.
    pub fn update(&mut self, height: u32, block_hash: [u8; 32]) {
        self.last_scanned_height = Some(height);
        self.last_block_hash = Some(block_hash);
    }

    /// Sets only the last scanned height without updating the block hash.
    /// Used for progress tracking when the block hash is not available.
    pub fn set_last_scanned_height(&mut self, height: u32) {
        match self.last_scanned_height {
            Some(h) if height > h => self.last_scanned_height = Some(height),
            None => self.last_scanned_height = Some(height),
            _ => {}
        }
    } // Queries

    /// Returns the height where the next scan should start.
    ///
    /// If no blocks have been scanned yet, returns the birthday height.
    /// Otherwise, returns the last scanned height + 1.
    pub fn next_scan_start(&self) -> u32 {
        self.last_scanned_height
            .map(|h| h + 1)
            .unwrap_or(self.birthday_height)
    } // Persistence

    /// Persist the state to `{dir}/{FILENAME}`.
    ///
    /// Does nothing if persistence is disabled or no directory is set.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        let Some(dir) = &self.dir else {
            return;
        };
        let _ = fs::create_dir_all(dir);
        let path = dir.join(Self::FILENAME);

        match serde_json::to_string_pretty(self) {
            Ok(content) => {
                if let Err(e) = fs::write(path, content) {
                    log::error!("ScanState::persist() failed to write: {}", e);
                }
            }
            Err(e) => log::error!("ScanState::persist() failed to serialize: {}", e),
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_state_new() {
        let state = ScanState::new(100);

        assert_eq!(state.birthday_height(), 100);
        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);
        assert!(state.persist); // Default is true
        assert!(state.dir.is_none());
    }

    #[test]
    fn test_scan_state_with_path() {
        let dir = PathBuf::from("/tmp/test_scan_state_dir");
        let state = ScanState::with_path(200, dir.clone());

        assert_eq!(state.birthday_height(), 200);
        assert_eq!(state.dir, Some(dir));
        assert!(state.persist); // Default is true
    }

    #[test]
    fn test_scan_state_next_scan_start_initial() {
        let state = ScanState::new(100);

        // No scanned height yet, should return birthday
        assert_eq!(state.next_scan_start(), 100);
    }

    #[test]
    fn test_scan_state_next_scan_start_after_scan() {
        let mut state = ScanState::new(100);
        let block_hash = [0xAB; 32];

        state.update(150, block_hash);

        // After scanning to 150, next scan starts at 151
        assert_eq!(state.next_scan_start(), 151);
    }

    #[test]
    fn test_scan_state_update() {
        let mut state = ScanState::new(100);
        let block_hash = [0xCD; 32];

        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);

        state.update(500, block_hash);

        assert_eq!(state.last_scanned_height(), Some(500));
        assert_eq!(state.last_block_hash(), Some(block_hash));
    }

    #[test]
    fn test_scan_state_serde_roundtrip() {
        let mut state = ScanState::new(100);
        let block_hash = [0xEF; 32];
        state.update(250, block_hash);

        let json = serde_json::to_string(&state).expect("serialize");
        let loaded: ScanState = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(loaded.birthday_height(), 100);
        assert_eq!(loaded.last_scanned_height(), Some(250));
        assert_eq!(loaded.last_block_hash(), Some(block_hash));

        // Skipped fields should be defaults
        assert!(loaded.dir.is_none());
        assert!(!loaded.persist); // Default for bool is false
    }

    #[test]
    fn test_scan_state_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-scan-state-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let block_hash = [0x12; 32];

        // Create and populate state
        let mut state = ScanState::with_path(300, temp_dir.clone()).enable_persist(true);
        state.update(350, block_hash);
        state.persist();

        // File should exist under the account dir with the canonical name.
        assert!(temp_dir.join(ScanState::FILENAME).exists());

        // Load from dir
        let loaded = ScanState::from_file(temp_dir.clone()).expect("load");

        assert_eq!(loaded.birthday_height(), 300);
        assert_eq!(loaded.last_scanned_height(), Some(350));
        assert_eq!(loaded.last_block_hash(), Some(block_hash));

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_scan_state_persist_disabled() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-scan-state-no-persist-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        // Create state with persist disabled
        let mut state = ScanState::with_path(100, temp_dir.clone()).enable_persist(false);
        state.update(150, [0xAA; 32]);
        state.persist();

        // File should NOT exist
        assert!(!temp_dir.join(ScanState::FILENAME).exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_scan_state_from_file_not_found() {
        // Directory has no state.json under it, so load must fail with Io.
        let result = ScanState::from_file(PathBuf::from("/nonexistent/path"));
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, ScanStateError::Io(_)));
        }
    }

    #[test]
    fn test_scan_state_getters() {
        let mut state = ScanState::new(500);
        let block_hash = [0x55; 32];

        // Before update
        assert_eq!(state.birthday_height(), 500);
        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);

        // After update
        state.update(600, block_hash);

        assert_eq!(state.birthday_height(), 500); // Unchanged
        assert_eq!(state.last_scanned_height(), Some(600));
        assert_eq!(state.last_block_hash(), Some(block_hash));
    }

    #[test]
    fn test_scan_state_default() {
        let state = ScanState::default();

        assert_eq!(state.birthday_height(), 0);
        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);
        assert!(state.dir.is_none());
        assert!(!state.persist); // Default for bool is false
    }

    #[test]
    fn test_scan_state_error_display() {
        // Test Io error variant
        let err = ScanStateError::Io("state file locked".to_string());
        let msg = err.to_string();
        assert!(msg.contains("io error"));
        assert!(msg.contains("state file locked"));

        // Test Parse error variant
        let err = ScanStateError::Parse("corrupted state data".to_string());
        let msg = err.to_string();
        assert!(msg.contains("parse error"));
        assert!(msg.contains("corrupted state data"));
    }
}
