//! 配置目录与几项设置。格式是「一行 key=value」，不值得为它引 JSON 依赖。

use std::path::PathBuf;

use crate::shell;

/// 启动时的默认播放器状态。测试里直接当作不支持：`cargo test` 不该依赖
/// 开发机的文件关联状态，更不该去读真实注册表。
pub(super) fn initial_shell_status() -> shell::Status {
    if cfg!(test) {
        shell::Status::Unsupported
    } else {
        shell::status()
    }
}

/// 设置文件：和库文件放同一个目录，一行一个 `key=value`。
pub(super) fn settings_path() -> Option<PathBuf> {
    library_path().map(|path| path.with_file_name("settings.txt"))
}

/// 读一项设置。文件不存在、没这一项都返回 `None`，由调用方决定默认值。
pub(super) fn read_setting(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.trim().to_string())
}

/// 改一项设置，其余行原样保留。
pub(super) fn write_setting(key: &str, value: &str) {
    let Some(path) = settings_path() else {
        return;
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split_once('=').is_none_or(|(name, _)| name != key))
        .map(str::to_string)
        .collect();
    lines.push(format!("{key}={value}"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, lines.join("\n") + "\n") {
        eprintln!("[settings] 保存设置失败: {error}");
    }
}

/// 配置文件位置。单元测试里返回 `None`：测试不该读写用户真实的配置目录。
pub(super) fn library_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(config_dir()?.join("ngy-audio-player").join("library.txt"))
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
}
