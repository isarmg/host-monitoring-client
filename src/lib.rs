//! Host Monitoring 的跨平台只读遥测 Client。
//!
//! 遥测进程不监听业务端口、不执行 Server 下发的命令，也不包含自更新器。
//! 配置与配对通过命令行完成，后台由操作系统服务管理器运行。
//! 平台差异通过 capability 表达；缺失数据使用 `None`，不会用 0 冒充。

#[cfg(all(feature = "desktop", not(unix)))]
mod atomic_file;
#[cfg(feature = "desktop")]
pub mod client_identity;
#[cfg(feature = "desktop")]
pub mod collectors;
#[cfg(feature = "desktop")]
pub mod config;
pub mod mobile;
pub mod model;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "desktop")]
pub mod pairing;
#[cfg(feature = "desktop")]
pub mod pairing_input;
#[cfg(all(feature = "desktop", any(not(unix), test)))]
mod private_fs;
mod report_contract;
#[cfg(feature = "desktop")]
mod secret_io;
#[cfg(feature = "desktop")]
pub mod service;
#[cfg(feature = "desktop")]
pub mod spool;
#[cfg(all(feature = "desktop", not(unix)))]
mod state_lock;
#[cfg(feature = "desktop")]
mod state_store;
#[cfg(feature = "desktop")]
mod tls_input;
#[cfg(feature = "desktop")]
pub mod transport;

#[cfg(feature = "desktop")]
pub use collectors::SystemSampler;
#[cfg(feature = "desktop")]
pub use config::{ClientCommand, ClientConfig, OutputMode};
pub use model::*;

#[cfg(feature = "desktop")]
pub mod maintenance;

#[cfg(feature = "desktop")]
pub mod runtime_status;
