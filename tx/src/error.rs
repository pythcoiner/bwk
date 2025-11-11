#[derive(Debug)]
pub enum Error {
    Satisfaction,
    NoFundingTx,
    NoDescriptor,
    WrongVout,
    Update,
    Derivation,
    MultiDescriptor,
    KeyChain,
}
