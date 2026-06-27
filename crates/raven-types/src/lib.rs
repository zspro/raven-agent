//! # agent-types
//!
//! 跨模块共享的核心类型定义。
//! 所有 crate 都依赖此模块，避免循环依赖。
//!
//! 类型按主题拆分到子模块，此处统一 `pub use` 重导出，
//! 外部仍可 `use raven_types::*` 或 `raven_types::Config` 无感访问。

mod config;
mod error;
mod message;
mod model;
mod provider;
mod util;

pub use config::*;
pub use error::*;
pub use message::*;
pub use model::*;
pub use provider::*;
pub use util::*;

#[cfg(test)]
mod tests;
