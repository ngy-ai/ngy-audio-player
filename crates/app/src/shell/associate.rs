//! 登记 / 取消登记 / 启动时维护，以及「现在登记到哪一步」的只读查询。
//!
//! 全部是幂等操作：重复执行只是把同一批键值再写一遍。

use anyhow::Context as _;

use super::plan::{APP_KEY, CAPABILITIES, PROG_ID, Status, delete_roots, extensions, plan};
use super::registry;

#[cfg(windows)]
pub fn notify_shell() {
    use std::ptr::null;

    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    // 不通知的话，资源管理器会继续拿旧图标和旧关联，直到下次登录。
    // SAFETY: 只带一个事件号，不带任何数据指针。
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
}

fn current_exe() -> anyhow::Result<std::path::PathBuf> {
    std::env::current_exe().context("取不到本程序的路径")
}

pub fn status() -> Status {
    if !registry::exists(CAPABILITIES) {
        return Status::Absent;
    }
    let exts = extensions();
    let defaults = exts.iter().filter(|ext| user_choice_is_ours(ext)).count();
    Status::Registered {
        defaults,
        total: exts.len(),
    }
}

/// 某个扩展名现在是不是由本程序打开。只读 `UserChoice`，从不写。
fn user_choice_is_ours(ext: &str) -> bool {
    let path = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.{ext}\\UserChoice"
    );
    registry::read(&path, Some("ProgId")).as_deref() == Some(PROG_ID)
}

pub fn register() -> anyhow::Result<()> {
    let exe = current_exe()?;
    for entry in plan(&exe) {
        registry::write(&entry)?;
    }
    notify_shell();
    Ok(())
}

pub fn unregister() -> anyhow::Result<()> {
    for root in delete_roots() {
        registry::delete_tree(&root);
    }
    registry::delete_value("Software\\RegisteredApplications", APP_KEY);
    // 打开方式列表里的登记逐个撤掉；键本身留着，别的程序还在用它。
    for ext in extensions() {
        registry::delete_value(
            &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
            PROG_ID,
        );
    }
    notify_shell();
    Ok(())
}

/// 启动时的「自动维护」：登记过就把整份登记幂等重写一遍。
///
/// 只在「登记过」时动手——那条键存在就等于用户表达过意愿。要维护的东西有两个：
/// 程序被挪过位置之后注册表里写死的 exe 路径（悬空的话双击文件什么都不会发生，
/// 比没登记更难查），以及被别的程序顺手删掉的 OpenWithProgids 条目。
pub fn maintain() -> anyhow::Result<()> {
    if !registry::exists(CAPABILITIES) {
        return Ok(());
    }
    register()
}
