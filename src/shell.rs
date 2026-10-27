//! Windows 文件关联登记：把本程序放进系统的「打开方式」与默认应用页。
//!
//! 只写 `HKCU`（当前用户），不需要管理员权限。`UserChoice` 一律不碰 —— Windows 8
//! 之后它带校验值，强写属于劫持，系统一升级就可能失效。所以这里只负责「登记」，
//! 真正的「设为默认」要用户去系统设置点一次，见
//! `docs/adr/0007-register-but-never-force-default.md`。
//!
//! 写注册表是不可回滚的系统副作用，所以「格式清单 → 注册表键值」全部由纯函数
//! [`plan`] 产出，测试只测它；真正落盘的函数都短到一眼能看完。

use std::path::Path;

use anyhow::Context as _;
use ngy_audio_engine::format;

/// ProgID：所有格式共用一个，图标和打开命令也就只有一份。
pub const PROG_ID: &str = "Soundwave.AudioFile";
/// 在 `RegisteredApplications` 里的键名，同时也是系统设置深链接的参数。
/// 用 ASCII 是为了能原样拼进 `ms-settings:` 的查询串。
pub const APP_KEY: &str = "Soundwave";
/// 系统默认应用页里显示的名字。
const DISPLAY_NAME: &str = "声波";
const DESCRIPTION: &str = "格式通用的桌面音频播放器";
const CAPABILITIES: &str = "Software\\Soundwave\\Capabilities";

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

// —— Windows 实现 ——

#[cfg(windows)]
mod win {
    use std::ptr::{null, null_mut};

    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegCreateKeyW,
        RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use super::Entry;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(code: u32, what: &str) -> Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            bail!("{what}失败（错误码 {code}）")
        }
    }

    pub fn write(entry: &Entry) -> Result<()> {
        let path = wide(&entry.key);
        let mut key: HKEY = null_mut();
        // SAFETY: 路径是以 NUL 结尾的 UTF-16，句柄只在本次调用内使用，用完就关。
        let code = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
        check(code, &format!("创建注册表键 {} ", entry.key))?;

        let name = entry.name.as_deref().map(wide);
        let data: Vec<u16> = entry
            .data
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // REG_SZ 的长度按字节算，含结尾的 NUL。
        let bytes = (data.len() * 2) as u32;
        let code = unsafe {
            RegSetValueExW(
                key,
                name.as_ref().map_or(null(), |name| name.as_ptr()),
                0,
                REG_SZ,
                data.as_ptr().cast::<u8>(),
                bytes,
            )
        };
        unsafe { RegCloseKey(key) };
        let which = entry.name.as_deref().unwrap_or("(默认)");
        check(code, &format!("写入 {}\\{which} ", entry.key))
    }

    fn open(path: &str) -> Option<HKEY> {
        let path = wide(path);
        let mut key: HKEY = null_mut();
        let code =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };
        (code == ERROR_SUCCESS).then_some(key)
    }

    pub fn exists(path: &str) -> bool {
        let Some(key) = open(path) else {
            return false;
        };
        unsafe { RegCloseKey(key) };
        true
    }

    pub fn read(path: &str, name: Option<&str>) -> Option<String> {
        let key = open(path)?;
        let name = name.map(wide);
        let name_ptr = name.as_ref().map_or(null(), |name| name.as_ptr());

        // 先问长度：值可能比预想的长，硬编码一个缓冲区迟早截断。
        let mut size = 0u32;
        let code =
            unsafe { RegQueryValueExW(key, name_ptr, null(), null_mut(), null_mut(), &mut size) };
        if code != ERROR_SUCCESS || size == 0 {
            unsafe { RegCloseKey(key) };
            return None;
        }

        let mut buffer = vec![0u16; size as usize / 2 + 1];
        let mut read = size;
        let code = unsafe {
            RegQueryValueExW(
                key,
                name_ptr,
                null(),
                null_mut(),
                buffer.as_mut_ptr().cast::<u8>(),
                &mut read,
            )
        };
        unsafe { RegCloseKey(key) };
        if code != ERROR_SUCCESS {
            return None;
        }
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        Some(String::from_utf16_lossy(&buffer))
    }

    pub fn delete_tree(path: &str) {
        let path = wide(path);
        // SAFETY: 只是删当前用户下的一个子树；不存在的键会直接返回错误，忽略即可。
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
    }

    pub fn delete_value(path: &str, name: &str) {
        let path_wide = wide(path);
        let mut key: HKEY = null_mut();
        let code = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path_wide.as_ptr(),
                0,
                KEY_READ | KEY_WRITE,
                &mut key,
            )
        };
        if code != ERROR_SUCCESS {
            return;
        }
        let name = wide(name);
        unsafe { RegDeleteValueW(key, name.as_ptr()) };
        unsafe { RegCloseKey(key) };
    }
}

#[cfg(windows)]
fn notify_shell() {
    use std::ptr::null;

    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    // 不通知的话，资源管理器会继续拿旧图标和旧关联，直到下次登录。
    // SAFETY: 只带一个事件号，不带任何数据指针。
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
}

#[cfg(windows)]
fn shell_execute(target: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 各指针都指向 NUL 结尾的 UTF-16 字符串；返回值按约定 > 32 才算成功。
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if (result as isize) <= 32 {
        anyhow::bail!("ShellExecuteW 返回 {}", result as isize);
    }
    Ok(())
}

#[cfg(windows)]
fn current_exe() -> anyhow::Result<std::path::PathBuf> {
    std::env::current_exe().context("取不到本程序的路径")
}

#[cfg(windows)]
pub fn status() -> Status {
    if !win::exists(CAPABILITIES) {
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
#[cfg(windows)]
fn user_choice_is_ours(ext: &str) -> bool {
    let path = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.{ext}\\UserChoice"
    );
    win::read(&path, Some("ProgId")).as_deref() == Some(PROG_ID)
}

#[cfg(windows)]
pub fn register() -> anyhow::Result<()> {
    let exe = current_exe()?;
    for entry in plan(&exe) {
        win::write(&entry)?;
    }
    notify_shell();
    Ok(())
}

#[cfg(windows)]
pub fn unregister() -> anyhow::Result<()> {
    for root in delete_roots() {
        win::delete_tree(&root);
    }
    win::delete_value("Software\\RegisteredApplications", APP_KEY);
    // 打开方式列表里的登记逐个撤掉；键本身留着，别的程序还在用它。
    for ext in extensions() {
        win::delete_value(
            &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
            PROG_ID,
        );
    }
    notify_shell();
    Ok(())
}

/// 程序被挪过位置之后，把注册表里写死的 exe 路径刷新过来。
///
/// 只在「登记过」时动手：那条键存在就等于用户表达过意愿。路径悬空的后果是
/// 双击音频文件什么都不发生，比没登记更难查。
#[cfg(windows)]
pub fn refresh() -> anyhow::Result<()> {
    if !win::exists(CAPABILITIES) {
        return Ok(());
    }
    let exe = current_exe()?;
    let stored = win::read(
        &format!("Software\\Classes\\{PROG_ID}\\shell\\open\\command"),
        None,
    );
    let wanted = plan(&exe)
        .into_iter()
        .find(|entry| entry.name.is_none() && entry.key.ends_with("shell\\open\\command"))
        .map(|entry| entry.data);
    if stored.is_some() && stored == wanted {
        return Ok(());
    }
    register()
}

/// 打开系统的「默认应用」页，并尽量直接落在声波那一页。
///
/// 参数取 `RegisteredApplications` 的键名：Windows 11 上会直接进到本应用页，
/// 那里有一个按钮覆盖全部已登记类型。落空时也只是停在默认应用首页，不影响使用。
#[cfg(windows)]
pub fn open_settings() -> anyhow::Result<()> {
    let uri = format!("ms-settings:defaultapps?registeredAppUser={APP_KEY}");
    shell_execute(&uri).with_context(|| format!("打开系统设置失败：{uri}"))
}

// —— 其它平台 ——

#[cfg(not(windows))]
pub fn status() -> Status {
    Status::Unsupported
}

#[cfg(not(windows))]
pub fn register() -> anyhow::Result<()> {
    anyhow::bail!("登记默认播放器只在 Windows 上支持")
}

#[cfg(not(windows))]
pub fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("取消登记只在 Windows 上支持")
}

#[cfg(not(windows))]
pub fn refresh() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn open_settings() -> anyhow::Result<()> {
    anyhow::bail!("打开系统默认应用页只在 Windows 上支持")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(exe: &str) -> Vec<Entry> {
        plan(Path::new(exe))
    }

    fn find<'a>(entries: &'a [Entry], key_suffix: &str, name: Option<&str>) -> Option<&'a Entry> {
        entries
            .iter()
            .find(|entry| entry.key.ends_with(key_suffix) && entry.name.as_deref() == name)
    }

    #[test]
    fn command_quotes_the_exe_and_takes_one_argument() {
        let entries = plan_for("C:\\Program Files\\声波\\ngy-audio-player.exe");
        let command = find(&entries, "shell\\open\\command", None).expect("缺少打开命令");
        assert_eq!(
            command.data,
            "\"C:\\Program Files\\声波\\ngy-audio-player.exe\" \"%1\""
        );
        let icon = find(&entries, "DefaultIcon", None).expect("缺少图标");
        assert!(
            icon.data.ends_with("\",0"),
            "图标同样要带引号：{}",
            icon.data
        );
    }

    #[test]
    fn every_registered_extension_gets_both_entries() {
        let entries = plan_for("C:\\app.exe");
        let exts = extensions();
        for ext in &exts {
            assert!(
                find(
                    &entries,
                    &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                    Some(PROG_ID)
                )
                .is_some(),
                "`{ext}` 没进打开方式"
            );
            assert!(
                find(
                    &entries,
                    "\\Capabilities\\FileAssociations",
                    Some(&format!(".{ext}"))
                )
                .is_some(),
                "`{ext}` 没进默认应用页的能力表"
            );
        }
        // 能力表里不该混进视频容器。
        assert!(
            find(&entries, "\\Capabilities\\FileAssociations", Some(".mkv")).is_none(),
            "视频容器不该出现在登记清单里"
        );
    }

    #[test]
    fn app_is_advertised_to_the_settings_page() {
        let entries = plan_for("C:\\app.exe");
        let registered = find(&entries, "RegisteredApplications", Some(APP_KEY))
            .expect("没在 RegisteredApplications 里登记，系统设置里就不会出现");
        assert_eq!(registered.data, CAPABILITIES);
        assert_eq!(
            find(&entries, "Capabilities", Some("ApplicationName"))
                .expect("缺少应用名")
                .data,
            "声波"
        );
    }

    #[test]
    fn unregister_removes_both_trees() {
        let roots = delete_roots();
        assert!(roots.iter().any(|root| root.ends_with(PROG_ID)));
        assert!(
            roots
                .iter()
                .any(|root| root.ends_with("Software\\Soundwave"))
        );
    }
}
