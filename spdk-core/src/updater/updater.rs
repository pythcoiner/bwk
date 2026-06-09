use std::collections::{HashMap, HashSet};

use bitcoin::{absolute::Height, BlockHash, OutPoint};

use crate::error::Result;

use crate::OwnedOutput;

/// Trait for persisting scan results and progress
///
/// This trait provides synchronous methods for recording scanning progress,
/// found outputs, and spent inputs. Implementations should handle persistence
/// to storage (database, file system, etc.).
///
/// For async operations, see `AsyncUpdater`.
pub trait Updater {
    /// Ask the updater to record the scanning progress.
    fn record_scan_progress(&mut self, start: Height, current: Height, end: Height) -> Result<()>;

    /// Ask the updater to record the outputs found in a block.
    fn record_block_outputs(
        &mut self,
        height: Height,
        blkhash: BlockHash,
        found_outputs: HashMap<OutPoint, OwnedOutput>,
    ) -> Result<()>;

    /// Ask the updater to record the inputs found in a block.
    fn record_block_inputs(
        &mut self,
        blkheight: Height,
        blkhash: BlockHash,
        found_inputs: HashSet<OutPoint>,
    ) -> Result<()>;

    /// Advance the contiguous scan frontier to a fully scanned block.
    ///
    /// Used by the two-phase scan at a sub-range boundary, where receives and
    /// spends are recorded order-free and must not move the frontier per block.
    /// The default is a no-op for updaters that do not track a frontier.
    fn record_scan_frontier(&mut self, _height: Height, _block_hash: BlockHash) -> Result<()> {
        Ok(())
    }

    /// Advance the spend frontier to a fully swept height.
    ///
    /// The spend (input) sweep trails the receive frontier; this persists how far
    /// it has run so a resume skips swept heights. Default is a no-op for updaters
    /// that do not track it.
    fn record_spend_frontier(&mut self, _height: Height) -> Result<()> {
        Ok(())
    }

    /// Return the highest spend-swept height, if any.
    ///
    /// Default is `None` (sweep starts from the scan range start).
    fn spend_frontier(&self) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Ask the updater to save all recorded changes to persistent storage.
    fn save_to_persistent_storage(&mut self) -> Result<()>;

    /// Restore the set of owned outpoints from persistent storage.
    fn restore_owned_outpoints(&self) -> Result<HashSet<OutPoint>>;
}

/// Async version of Updater for non-blocking I/O operations
/// Available by default, excluded when "sync" feature is enabled
#[cfg(feature = "async")]
#[async_trait::async_trait]
pub trait AsyncUpdater: Send + Sync {
    /// Ask the updater to record the scanning progress.
    ///
    /// # Arguments
    /// * `start` - Starting block height
    /// * `current` - Current block height being processed
    /// * `end` - Ending block height
    async fn record_scan_progress(
        &mut self,
        start: Height,
        current: Height,
        end: Height,
    ) -> Result<()>;

    /// Ask the updater to record the outputs found in a block.
    ///
    /// # Arguments
    /// * `height` - Block height
    /// * `blkhash` - Block hash
    /// * `found_outputs` - Map of found outputs
    async fn record_block_outputs(
        &mut self,
        height: Height,
        blkhash: BlockHash,
        found_outputs: HashMap<OutPoint, OwnedOutput>,
    ) -> Result<()>;

    /// Ask the updater to record the inputs found in a block.
    ///
    /// # Arguments
    /// * `blkheight` - Block height
    /// * `blkhash` - Block hash
    /// * `found_inputs` - Set of spent inputs
    async fn record_block_inputs(
        &mut self,
        blkheight: Height,
        blkhash: BlockHash,
        found_inputs: HashSet<OutPoint>,
    ) -> Result<()>;

    /// Ask the updater to save all recorded changes to persistent storage.
    async fn save_to_persistent_storage(&mut self) -> Result<()>;

    /// Restore the set of owned outpoints from persistent storage.
    async fn restore_owned_outpoints(&self) -> Result<HashSet<OutPoint>>;
}
