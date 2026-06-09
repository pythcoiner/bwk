use super::Updater;

#[derive(Default)]
pub struct DummyUpdater;

impl DummyUpdater {
    pub fn new() -> Self {
        Self
    }
}

impl Updater for DummyUpdater {
    fn record_scan_progress(
        &mut self,
        _start: bitcoin::absolute::Height,
        _current: bitcoin::absolute::Height,
        _end: bitcoin::absolute::Height,
    ) -> crate::spdk_core::error::Result<()> {
        Ok(())
    }

    fn record_block_outputs(
        &mut self,
        _height: bitcoin::absolute::Height,
        _blkhash: bitcoin::BlockHash,
        _found_outputs: std::collections::HashMap<bitcoin::OutPoint, crate::spdk_core::OwnedOutput>,
    ) -> crate::spdk_core::error::Result<()> {
        Ok(())
    }

    fn record_block_inputs(
        &mut self,
        _blkheight: bitcoin::absolute::Height,
        _blkhash: bitcoin::BlockHash,
        _found_inputs: std::collections::HashSet<bitcoin::OutPoint>,
    ) -> crate::spdk_core::error::Result<()> {
        Ok(())
    }

    fn save_to_persistent_storage(&mut self) -> crate::spdk_core::error::Result<()> {
        Ok(())
    }

    fn restore_owned_outpoints(
        &self,
    ) -> crate::spdk_core::error::Result<std::collections::HashSet<bitcoin::OutPoint>> {
        Ok(std::collections::HashSet::new())
    }
}
