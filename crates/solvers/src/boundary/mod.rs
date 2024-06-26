//! This is a abstraction layer between the solver engine and the existing
//! "legacy" logic in `shared` and `model`.

pub mod baseline;
pub mod liquidity;
pub mod naive;

pub type Result<T> = anyhow::Result<T>;
