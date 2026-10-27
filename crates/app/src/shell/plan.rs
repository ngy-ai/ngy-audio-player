//! 登记内容的纯函数部分：格式清单 → 注册表键值。
//!
//! 这里不碰注册表，测试才能在没有副作用的机器上验证「写了什么」。

use std::path::Path;

use ngy_audio_engine::format;

/// ProgID：所有格式共用一个，图标和打开命令也就只有一份。
pub const PROG_ID: &str = "Soundwave.AudioFile";
/// 在 `RegisteredApplications` 里的键名，同时也是系统设置深链接的参数。
/// 用 ASCII 是为了能原样拼进 `ms-settings:` 的查询串。
pub const APP_KEY: &str = "Soundwave";
/// 系统默认应用页里显示的名字。
const DISPLAY_NAME: &str = "声波";
const DESCRIPTION: &str = "格式通用的桌面音频播放器";
pub(super) const CAPABILITIES: &str = "Software\\Soundwave\\Capabilities";
/// 「体验字符串」：Windows 在 shell32.dll 里硬编码的一段话，参与 UserChoice 哈希。
/// 自 Win10 起没变过；万一哪天改了，哈希会算错 —— 表现为系统忽略这次设置，不会崩溃。
pub(super) const USER_EXPERIENCE: &str =
    "user choice set via windows user experience {d18b6dd5-6124-4341-9318-804003bafa0b}";

/// 「设为默认」入口的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// 非 Windows：界面上不显示入口。
    Unsupported,
    /// 还没登记过。
    Absent,
    /// 登记过；`defaults` 个类型已经交给本程序打开。
    Registered { defaults: usize, total: usize },
}

/// 一条要写进注册表的值：键路径 + 值名（`None` 表示该键的默认值）+ 数据。
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub name: Option<String>,
    pub data: String,
}

/// 登记时要写的全部内容。
pub fn plan(exe: &Path) -> Vec<Entry> {
    let exe = exe.display().to_string();
    // 路径几乎一定带空格（`C:\Program Files\…`），命令与图标都必须整体加引号。
    let icon = format!("\"{exe}\",0");
    let command = format!("\"{exe}\" \"%1\"");
    let mut entries = vec![
        entry(
            &format!("Software\\Classes\\{PROG_ID}"),
            None,
            "声波音频文件",
        ),
        entry(
            &format!("Software\\Classes\\{PROG_ID}\\DefaultIcon"),
            None,
            &icon,
        ),
        entry(
            &format!("Software\\Classes\\{PROG_ID}\\shell\\open\\command"),
            None,
            &command,
        ),
        entry(CAPABILITIES, Some("ApplicationName"), DISPLAY_NAME),
        entry(CAPABILITIES, Some("ApplicationDescription"), DESCRIPTION),
        entry(CAPABILITIES, Some("ApplicationIcon"), &icon),
        entry(
            "Software\\RegisteredApplications",
            Some(APP_KEY),
            CAPABILITIES,
        ),
    ];
    for ext in format::shell_extensions() {
        // 让这些扩展名的「打开方式」列表里出现声波。
        entries.push(entry(
            &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
            Some(PROG_ID),
            "",
        ));
        // 默认应用页按这张表列出「声波能打开的格式」。
        entries.push(entry(
            &format!("{CAPABILITIES}\\FileAssociations"),
            Some(&format!(".{ext}")),
            PROG_ID,
        ));
    }
    entries
}

/// 取消登记时要整棵删掉的键。
pub fn delete_roots() -> Vec<String> {
    vec![
        format!("Software\\Classes\\{PROG_ID}"),
        "Software\\Soundwave".to_string(),
    ]
}

/// 要登记哪些扩展名。界面上的「N 种格式」也用它，免得和注册内容对不上。
pub fn extensions() -> Vec<&'static str> {
    format::shell_extensions()
}

fn entry(key: &str, name: Option<&str>, data: &str) -> Entry {
    Entry {
        key: key.to_string(),
        name: name.map(str::to_string),
        data: data.to_string(),
    }
}
