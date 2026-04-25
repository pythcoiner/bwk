//! Scan state tracking for Silent Payment blockchain scanning.
//!
//! The `ScanState` tracks progress of blockchain scanning, including the last
//! scanned block height and hash (for reorg detection), and the wallet's
//! birthday height.

use std::sync::Arc;

use bwk::persist::{self as persist, NoopBackend, PersistenceBackend};

/// Row key under the `account` store for [`ScanState::last_scanned_height`].
const LAST_SCANNED_HEIGHT_ROW: &str = "last_scanned_height";
/// Row key under the `account` store for [`ScanState::last_block_hash`].
const LAST_BLOCK_HASH_ROW: &str = "last_block_hash";
/// Row key under the `account` store for [`ScanState::birthday_height`].
const BIRTHDAY_HEIGHT_ROW: &str = "birthday_height";

// ScanState

/// Tracks blockchain scanning progress for Silent Payment wallets.
///
/// This struct maintains the scan position (last scanned height and block hash)
/// and the wallet's birthday height. The block hash is stored for reorg detection.
#[derive(Clone)]
pub struct ScanState {
    /// The last successfully scanned block height (None if never scanned)
    last_scanned_height: Option<u32>,

    /// The hash of the last scanned block (for reorg detection)
    last_block_hash: Option<[u8; 32]>,

    /// The wallet's birthday height (where scanning should start)
    birthday_height: u32,

    backend: Arc<dyn PersistenceBackend>,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            last_scanned_height: None,
            last_block_hash: None,
            birthday_height: 0,
            backend: Arc::new(NoopBackend),
        }
    }
}

impl std::fmt::Debug for ScanState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanState")
            .field("last_scanned_height", &self.last_scanned_height)
            .field("last_block_hash", &self.last_block_hash)
            .field("birthday_height", &self.birthday_height)
            .finish()
    }
}

impl ScanState {
    // Constructors

    /// Create a new scan state with the given birthday height (no persistence).
    pub fn new(birthday_height: u32) -> Self {
        Self {
            last_scanned_height: None,
            last_block_hash: None,
            birthday_height,
            backend: Arc::new(NoopBackend),
        }
    }

    /// Create a scan state backed by an arbitrary backend.
    pub fn with_backend(birthday_height: u32, backend: Arc<dyn PersistenceBackend>) -> Self {
        Self {
            last_scanned_height: None,
            last_block_hash: None,
            birthday_height,
            backend,
        }
    }

    /// Load a scan state from the per-field rows in the `account` store.
    ///
    /// `birthday_height` is used as the fallback when no birthday row
    /// has been persisted yet (fresh wallet).
    pub fn load_from_backend(birthday_height: u32, backend: Arc<dyn PersistenceBackend>) -> Self {
        let read_bytes = |row: &str| -> Option<Vec<u8>> {
            match backend.get_row(persist::ACCOUNT_STORE_KEY, row) {
                Ok(Some(b)) => Some(b),
                Ok(None) => None,
                Err(e) => {
                    log::error!("ScanState::load_from_backend get_row {row}: {e}");
                    None
                }
            }
        };

        let last_scanned_height = read_bytes(LAST_SCANNED_HEIGHT_ROW)
            .and_then(|b| match serde_json::from_slice::<Option<u32>>(&b) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::error!("ScanState::load_from_backend decode last_scanned_height: {e}");
                    None
                }
            })
            .unwrap_or(None);

        let last_block_hash = read_bytes(LAST_BLOCK_HASH_ROW)
            .and_then(|b| match serde_json::from_slice::<Option<[u8; 32]>>(&b) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::error!("ScanState::load_from_backend decode last_block_hash: {e}");
                    None
                }
            })
            .unwrap_or(None);

        let birthday = read_bytes(BIRTHDAY_HEIGHT_ROW)
            .and_then(|b| match serde_json::from_slice::<u32>(&b) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::error!("ScanState::load_from_backend decode birthday_height: {e}");
                    None
                }
            })
            .unwrap_or(birthday_height);

        Self {
            last_scanned_height,
            last_block_hash,
            birthday_height: birthday,
            backend,
        }
    }

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

    /// Persist the state through the configured backend.
    ///
    /// Writes the three scalar fields as individual rows under the
    /// [`persist::ACCOUNT_STORE_KEY`] store.
    pub fn persist(&self) {
        let write = |row: &str, bytes: &[u8]| {
            if let Err(e) = self.backend.put_row(persist::ACCOUNT_STORE_KEY, row, bytes) {
                log::error!("ScanState::persist() put {row}: {e}");
            }
        };

        match serde_json::to_vec(&self.last_scanned_height) {
            Ok(b) => write(LAST_SCANNED_HEIGHT_ROW, &b),
            Err(e) => log::error!("ScanState::persist() encode last_scanned_height: {e}"),
        }
        match serde_json::to_vec(&self.last_block_hash) {
            Ok(b) => write(LAST_BLOCK_HASH_ROW, &b),
            Err(e) => log::error!("ScanState::persist() encode last_block_hash: {e}"),
        }
        match serde_json::to_vec(&self.birthday_height) {
            Ok(b) => write(BIRTHDAY_HEIGHT_ROW, &b),
            Err(e) => log::error!("ScanState::persist() encode birthday_height: {e}"),
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use bwk::persist::JsonBackend;
    use std::fs;

    #[test]
    fn test_scan_state_new() {
        let state = ScanState::new(100);

        assert_eq!(state.birthday_height(), 100);
        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);
    }

    #[test]
    fn test_scan_state_with_backend_json() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-scan-state-with-backend-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let backend = Arc::new(JsonBackend::open(temp_dir.clone()).unwrap());
        let state = ScanState::with_backend(200, backend);

        assert_eq!(state.birthday_height(), 200);
        let _ = fs::remove_dir_all(&temp_dir);
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
    fn test_scan_state_persistence() {
        use std::env;

        let temp_dir = env::temp_dir().join("bwk-sp-scan-state-test");
        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::create_dir_all(&temp_dir);

        let block_hash = [0x12; 32];

        // Scoped so `state` drops at the closing brace, releasing
        // the DirLock before the reopener tries to acquire it.
        {
            let backend = JsonBackend::open(temp_dir.clone()).unwrap();
            let account_path = backend.path_for(persist::ACCOUNT_STORE_KEY);
            let mut state = ScanState::with_backend(300, Arc::new(backend));
            state.update(350, block_hash);
            state.persist();

            // Singleton fields are persisted under the `account` store.
            assert!(account_path.exists());
        }

        // Load from dir
        let backend = Arc::new(JsonBackend::open(temp_dir.clone()).unwrap());
        let loaded = ScanState::load_from_backend(0, backend);

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

        // Create state with persist disabled (NoopBackend via ScanState::new)
        let backend = JsonBackend::open(temp_dir.clone()).unwrap();
        let account_path = backend.path_for(persist::ACCOUNT_STORE_KEY);
        drop(backend);
        let mut state = ScanState::new(100);
        state.update(150, [0xAA; 32]);
        state.persist();

        // Account file should NOT exist (persist is a noop)
        assert!(!account_path.exists());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_scan_state_load_from_backend_fresh_dir_returns_default() {
        use std::env;

        // Fresh directory — no persisted state yet. JsonBackend::open
        // stamps DB_VERSION; load_from_backend returns a state at birthday 0.
        let temp_dir = env::temp_dir().join("bwk-sp-scan-state-fresh-test");
        let _ = fs::remove_dir_all(&temp_dir);

        let backend = Arc::new(JsonBackend::open(temp_dir.clone()).unwrap());
        let state = ScanState::load_from_backend(0, backend);
        assert_eq!(state.birthday_height(), 0);
        assert_eq!(state.last_scanned_height(), None);
        assert_eq!(state.last_block_hash(), None);

        let _ = fs::remove_dir_all(&temp_dir);
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
    }
}
