// use bwk_descriptor::SpkDerivator;
//
// use crate::TxTemplate;
//
// pub trait ChangeTip {
//     fn next_index(&mut self) -> u32;
// }
//
// pub enum ChangeTipHandle {
//     Internal(u32),
//     External(Box<dyn ChangeTip>),
//     None,
// }
//
// pub struct TxBuilder {
//     derivator: SpkDerivator,
//     change_tip: ChangeTipHandle,
//     tx_template: TxTemplate,
// }
