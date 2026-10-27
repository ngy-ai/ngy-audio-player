//! Windows 文件关联登记：把本程序放进系统的「打开方式」与默认应用页。
//!
//! 只写 `HKCU`（当前用户），不需要管理员权限。`UserChoice` 一律不碰 —— Windows 8
//! 之后它带校验值，强写属于劫持，系统一升级就可能失效。所以这里只负责「登记」，
//! 真正的「设为默认」要用户去系统设置点一次，见
//! `docs/adr/0007-register-but-never-force-default.md`。
//!
//! 写注册表是不可回滚的系统副作用，所以「格式清单 → 注册表键值」全部由纯函数
//! [`plan`] 产出，测试只测它；真正落盘的函数都短到一眼能看完。
//!
//! 子模块按职责切：`plan` 只算内容，`user_choice` 只算哈希，`registry` 只做注册表
//! 读写原语；`associate` 与 `defaults` 才是有副作用的动作。

#[cfg(windows)]
mod associate;
#[cfg(windows)]
mod defaults;
mod plan;
#[cfg(windows)]
mod registry;
mod reveal;
#[cfg(not(windows))]
mod unsupported;
mod user_choice;

#[cfg(test)]
mod tests;

pub use plan::{APP_KEY, Entry, PROG_ID, Status, delete_roots, extensions, plan};
pub use reveal::reveal;
pub use user_choice::ForceReport;

#[cfg(windows)]
pub use associate::{maintain, register, status, unregister};
#[cfg(windows)]
pub use defaults::{force_defaults, open_settings};

#[cfg(not(windows))]
pub use unsupported::{force_defaults, maintain, open_settings, register, status, unregister};
