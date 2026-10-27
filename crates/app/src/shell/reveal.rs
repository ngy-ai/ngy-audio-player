//! 在系统的文件管理器里定位文件。三个平台各用各的入口，没有注册表参与。

use std::path::Path;

use anyhow::Context as _;

/// Windows 打开所在文件夹并选中它，macOS 在 Finder 里显示，
/// Linux 交给 `xdg-open` 打开父目录。
pub fn reveal(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        // explorer 的 /select 只认一个参数；路径带空格时由 Command 自己补引号。
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    };
    command
        .spawn()
        .with_context(|| format!("打开文件管理器失败：{}", path.display()))?;
    Ok(())
}
