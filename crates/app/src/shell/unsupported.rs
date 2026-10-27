//! 非 Windows 平台的桩：报错文案与 `Status::Unsupported`。
//!
//! 保持函数签名与 Windows 实现一致，界面层就不用按平台写分支。

use super::plan::Status;
use super::user_choice::ForceReport;

pub fn status() -> Status {
    Status::Unsupported
}

pub fn register() -> anyhow::Result<()> {
    anyhow::bail!("登记默认播放器只在 Windows 上支持")
}

pub fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("取消登记只在 Windows 上支持")
}

pub fn maintain() -> anyhow::Result<()> {
    Ok(())
}

pub fn force_defaults() -> anyhow::Result<ForceReport> {
    anyhow::bail!("直接设为默认只在 Windows 上支持")
}

pub fn open_settings() -> anyhow::Result<()> {
    anyhow::bail!("打开系统默认应用页只在 Windows 上支持")
}
