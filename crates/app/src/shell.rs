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
use std::time::{SystemTime, UNIX_EPOCH};

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
/// 「体验字符串」：Windows 在 shell32.dll 里硬编码的一段话，参与 UserChoice 哈希。
/// 自 Win10 起没变过；万一哪天改了，哈希会算错 —— 表现为系统忽略这次设置，不会崩溃。
const USER_EXPERIENCE: &str =
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

/// 「直接设为默认」的结果：成功几个、失败几个（失败带原因，界面上只显示第一条）。
#[derive(Debug)]
pub struct ForceReport {
    pub applied: usize,
    pub failures: Vec<String>,
}

/// 当前时间压成参与哈希的 16 位十六进制：FILETIME 的高 32 位 + 低 32 位。
///
/// **必须截到分钟**：Windows 用 `UserChoice` 键自身的写入时间（分钟级）重算哈希来校验，
/// 所以「算哈希」和「写键」必须落在同一分钟里，否则整条记录被判无效。
fn timestamp_hex(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    // FILETIME 是自 1601-01-01 起的 100ns 计数；Unix 纪元与它的差是 11644473600 秒。
    let filetime = (seconds / 60 + 11_644_473_600) * 10_000_000;
    format!("{:08x}{:08x}", filetime >> 32, filetime & 0xFFFF_FFFF)
}

/// 哈希的输入串：扩展名 + 用户 SID + ProgId + 时间戳 + 体验字符串，全部小写、无分隔符。
fn hash_input(ext: &str, sid: &str, timestamp: &str) -> String {
    format!(
        ".{ext}{sid}{}{timestamp}{USER_EXPERIENCE}",
        PROG_ID.to_lowercase()
    )
    .to_lowercase()
}

/// 复刻 Windows 对 `UserChoice` 的哈希校验：MD5 之后还要走两轮带魔数的 32 位混算，
/// 再把两组结果交叉异或、取 8 字节做 Base64（12 个字符，末尾带 `=`）。
///
/// 移植自社区逆向实现（PS-SFTA，MIT）。所有中间量都是 32 位、按补码回绕；
/// 右移必须按无符号数做（见下面的 `shr`）。测试里拿参考实现算出的向量逐位比对。
fn user_choice_hash(base_info: &str) -> String {
    // UTF-16LE，再补两个 0 字节（等价于结尾的 NUL）。
    let mut buffer: Vec<u8> = Vec::with_capacity(base_info.len() * 2 + 2);
    for unit in base_info.encode_utf16() {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    buffer.push(0);
    buffer.push(0);

    use md5::Digest as _;
    let digest = md5::Md5::digest(&buffer);

    // 轮数：长度（字节，含结尾）先折成 PS-SFTA 里的 `length`，再折成循环次数。
    let length_base = buffer.len() as i32;
    let length = i32::from(length_base & 4 == 0) + (length_base >> 2) - 1;
    if length <= 1 {
        return String::new();
    }
    let rounds = ((length - 2) >> 1) + 1;

    let word = |at: usize| -> i32 {
        let mut bytes = [0u8; 4];
        for (offset, slot) in bytes.iter_mut().enumerate() {
            if let Some(byte) = buffer.get(at + offset) {
                *slot = *byte;
            }
        }
        i32::from_le_bytes(bytes)
    };
    let digest_word = |at: usize| -> i32 {
        i32::from_le_bytes([digest[at], digest[at + 1], digest[at + 2], digest[at + 3]])
    };

    // 右移是**无符号**的：Windows 内部按 32 位无符号数移位，负数不做符号扩展。
    // 这一点搞错（写成 `i32 >> n`）会算出一个格式正确、但系统永远不认的哈希。
    let shr = |value: i32, bits: u32| ((value as u32) >> bits) as i32;

    // 第一轮：两个魔数各带一个加法常量。
    let md51 = (digest_word(0) | 1).wrapping_add(0x69FB_0000u32 as i32);
    let md52 = (digest_word(4) | 1).wrapping_add(0x13DB_0000u32 as i32);
    let (mut cache, mut out1, mut out2) = (0i32, 0i32, 0i32);
    let mut at = 0usize;
    for _ in 0..rounds {
        let r0 = word(at).wrapping_add(out1);
        let r1 = word(at + 4);
        at += 8;
        let r2a = r0
            .wrapping_mul(md51)
            .wrapping_sub((0x10FA_9605u32 as i32).wrapping_mul(shr(r0, 16)));
        let r2b = (0x79F8_A395u32 as i32)
            .wrapping_mul(r2a)
            .wrapping_add((0x689B_6B9Fu32 as i32).wrapping_mul(shr(r2a, 16)));
        let r3 = (0xEA97_0001u32 as i32)
            .wrapping_mul(r2b)
            .wrapping_sub((0x3C10_1569u32 as i32).wrapping_mul(shr(r2b, 16)));
        let r4 = r3.wrapping_add(r1);
        let r5 = cache.wrapping_add(r3);
        let r6a = r4
            .wrapping_mul(md52)
            .wrapping_sub((0x3CE8_EC25u32 as i32).wrapping_mul(shr(r4, 16)));
        let r6b = (0x59C3_AF2Du32 as i32)
            .wrapping_mul(r6a)
            .wrapping_sub((0x2232_E0F1u32 as i32).wrapping_mul(shr(r6a, 16)));
        out1 = (0x1EC9_0001u32 as i32)
            .wrapping_mul(r6b)
            .wrapping_add((0x35BD_1EC9u32 as i32).wrapping_mul(shr(r6b, 16)));
        out2 = r5.wrapping_add(out1);
        cache = out2;
    }
    let (first1, first2) = (out1, out2);

    // 第二轮：同样的结构，但不带加法常量。
    let md51 = digest_word(0) | 1;
    let md52 = digest_word(4) | 1;
    let (mut cache, mut out1, mut out2) = (0i32, 0i32, 0i32);
    let mut at = 0usize;
    for _ in 0..rounds {
        let r0 = word(at).wrapping_add(out1);
        at += 8;
        let r1a = r0.wrapping_mul(md51);
        let r1b = (0xB111_0000u32 as i32)
            .wrapping_mul(r1a)
            .wrapping_sub((0x3067_4EEFu32 as i32).wrapping_mul(shr(r1a, 16)));
        let r2a = (0x5B9F_0000u32 as i32)
            .wrapping_mul(r1b)
            .wrapping_sub((0x78F7_A461u32 as i32).wrapping_mul(shr(r1b, 16)));
        let r2b = (0x12CE_B96Du32 as i32)
            .wrapping_mul(shr(r2a, 16))
            .wrapping_sub((0x4693_0000u32 as i32).wrapping_mul(r2a));
        let r3 = (0x1D83_0000u32 as i32)
            .wrapping_mul(r2b)
            .wrapping_add((0x257E_1D83u32 as i32).wrapping_mul(shr(r2b, 16)));
        let r4a = md52.wrapping_mul(r3.wrapping_add(word(at - 4)));
        let r4b = (0x16F5_0000u32 as i32)
            .wrapping_mul(r4a)
            .wrapping_sub((0x5D8B_E90Bu32 as i32).wrapping_mul(shr(r4a, 16)));
        let r5a = (0x96FF_0000u32 as i32)
            .wrapping_mul(r4b)
            .wrapping_sub((0x2C7C_6901u32 as i32).wrapping_mul(shr(r4b, 16)));
        let r5b = (0x2B89_0000u32 as i32)
            .wrapping_mul(r5a)
            .wrapping_add((0x7C93_2B89u32 as i32).wrapping_mul(shr(r5a, 16)));
        out1 = (0x9F69_0000u32 as i32)
            .wrapping_mul(r5b)
            .wrapping_sub((0x405B_6097u32 as i32).wrapping_mul(shr(r5b, 16)));
        out2 = out1.wrapping_add(cache).wrapping_add(r3);
        cache = out2;
    }

    let mut raw = [0u8; 8];
    raw[..4].copy_from_slice(&(first1 ^ out1).to_le_bytes());
    raw[4..].copy_from_slice(&(first2 ^ out2).to_le_bytes());
    base64(&raw)
}

/// 标准 Base64（带 `=` 填充）。只为 8 字节的输出写一份，不引依赖。
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b1 = u32::from(chunk[0]);
        let b2 = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let b3 = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let triple = (b1 << 16) | (b2 << 8) | b3;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
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

    /// 打开（不存在则创建）一个键，调用方负责 [`close`]。
    pub fn create(key_path: &str) -> Result<HKEY> {
        let path = wide(key_path);
        let mut key: HKEY = null_mut();
        // SAFETY: 路径是以 NUL 结尾的 UTF-16；句柄交给调用方关闭。
        let code = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
        check(code, &format!("创建注册表键 {key_path} "))?;
        Ok(key)
    }

    pub fn set_string(key: HKEY, name: &str, data: &str) -> Result<()> {
        let name_wide = wide(name);
        let data: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = (data.len() * 2) as u32;
        // SAFETY: 键由 create 得到且仍然有效；两个指针都是 NUL 结尾的 UTF-16。
        let code = unsafe {
            RegSetValueExW(
                key,
                name_wide.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast::<u8>(),
                bytes,
            )
        };
        check(code, &format!("写入注册表值 {name} "))
    }

    pub fn close(key: HKEY) {
        unsafe { RegCloseKey(key) };
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

/// 启动时的「自动维护」：登记过就把整份登记幂等重写一遍。
///
/// 只在「登记过」时动手——那条键存在就等于用户表达过意愿。要维护的东西有两个：
/// 程序被挪过位置之后注册表里写死的 exe 路径（悬空的话双击文件什么都不会发生，
/// 比没登记更难查），以及被别的程序顺手删掉的 OpenWithProgids 条目。
#[cfg(windows)]
pub fn maintain() -> anyhow::Result<()> {
    if !win::exists(CAPABILITIES) {
        return Ok(());
    }
    register()
}

/// 当前用户的 SID 字符串（小写）。它参与哈希，取错了整条记录就会被系统判废。
#[cfg(windows)]
fn current_sid() -> anyhow::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: 只读当前进程的令牌；句柄用完就关。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        anyhow::bail!("拿不到进程令牌");
    }
    let mut size = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size) };
    let mut buffer = vec![0u8; size as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        anyhow::bail!("读不到令牌里的用户信息");
    }

    // SAFETY: 调用成功时 buffer 就是一个 TOKEN_USER，布局由系统保证。
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let mut raw: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut raw) } == 0 {
        anyhow::bail!("SID 转字符串失败");
    }
    let mut len = 0usize;
    // SAFETY: 系统返回的是 NUL 结尾的宽字符串，先量长度再拷贝。
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let sid = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(raw, len)) };
    unsafe { LocalFree(raw.cast()) };
    Ok(sid.to_lowercase())
}

/// 写一条「用户已经选过」的记录。系统会在下一次解析关联时校验 `Hash`，
/// 对不上就忽略并删掉它 —— 所以这两个值必须在同一分钟内一起写下去。
#[cfg(windows)]
fn write_user_choice(ext: &str, sid: &str, timestamp: &str) -> anyhow::Result<()> {
    let path = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.{ext}\\UserChoice"
    );
    let hash = user_choice_hash(&hash_input(ext, sid, timestamp));
    match write_choice_values(&path, &hash) {
        Ok(()) => Ok(()),
        Err(_) => {
            // 有些扩展名的 UserChoice 键带一条「拒绝写入」的 ACE —— 那是系统给默认程序
            // 上的锁。整条删掉再建（用户对自己名下这个键有删除权），等价于 SetUserFTA 的做法。
            win::delete_tree(&path);
            write_choice_values(&path, &hash)
        }
    }
}

#[cfg(windows)]
fn write_choice_values(path: &str, hash: &str) -> anyhow::Result<()> {
    let key = win::create(path)?;
    // 先 ProgId 后 Hash：顺序反了偶尔不生效（SetUserFTA 的经验）。
    let result =
        win::set_string(key, "ProgId", PROG_ID).and_then(|()| win::set_string(key, "Hash", hash));
    win::close(key);
    result
}

/// 跳过系统设置，把全部格式直接写成默认。
///
/// 这是绕开 Windows 保护的路径（见 ADR 0008）：只在用户明确点了「直接设为默认」时调用，
/// 失败也只是一个扩展名没设上，不影响已经登记的部分。
#[cfg(windows)]
pub fn force_defaults() -> anyhow::Result<ForceReport> {
    if !win::exists(CAPABILITIES) {
        register()?;
    }
    let sid = current_sid()?;

    // 哈希只精确到分钟：离整分太近就等一下，免得「算的时候是这一分钟、写完是下一分钟」。
    // 最多等 1 秒，界面期间会卡一下 —— 比写出一批系统永远不认的记录好。
    let seconds_into_minute = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() % 60)
        .unwrap_or(0);
    if seconds_into_minute >= 59 {
        std::thread::sleep(std::time::Duration::from_secs(60 - seconds_into_minute));
    }
    let timestamp = timestamp_hex(SystemTime::now());

    let mut applied = 0usize;
    let mut failures = Vec::new();
    for ext in extensions() {
        match write_user_choice(ext, &sid, &timestamp) {
            Ok(()) => applied += 1,
            Err(error) => failures.push(format!(".{ext}：{error}")),
        }
    }
    notify_shell();
    Ok(ForceReport { applied, failures })
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

/// 在系统的文件管理器里定位一个文件：Windows 打开所在文件夹并选中它，
/// macOS 在 Finder 里显示，Linux 交给 `xdg-open` 打开父目录。
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
pub fn maintain() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn force_defaults() -> anyhow::Result<ForceReport> {
    anyhow::bail!("直接设为默认只在 Windows 上支持")
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

    /// 三个向量由参考实现（PS-SFTA 的原版 PowerShell 代码）在固定输入下算出。
    /// 移植只要有一处移位方向、常量或字节序写错，这里就会对不上。
    #[test]
    fn hash_matches_the_reference_implementation() {
        let sid = "s-1-5-21-1111111111-2222222222-3333333333-1001";
        let stamp = "01d9d0a1b2c3d4e5";
        assert_eq!(
            user_choice_hash(&hash_input("mp3", sid, stamp)),
            "FlKzJsb+8YI="
        );
        assert_eq!(
            user_choice_hash(&hash_input("flac", sid, stamp)),
            "SvHWGghlvOM="
        );
        assert_eq!(
            user_choice_hash(&hash_input("cue", sid, stamp)),
            "sRvW+NvUcl0="
        );
    }

    /// 手动验收用：会**真的**改本机的默认程序，所以默认忽略。
    /// `cargo test -p ngy-audio-player --lib force_defaults_touches_real_system -- --ignored --nocapture`
    #[test]
    #[ignore = "会改写本机文件关联"]
    fn force_defaults_touches_real_system() {
        let report = force_defaults().expect("写 UserChoice 失败");
        println!("applied={} failures={:?}", report.applied, report.failures);
        assert!(report.applied > 0);
    }

    #[test]
    fn timestamp_is_truncated_to_the_minute() {
        // 2026-10-27T00:00:00Z 正好是整分。
        let start = UNIX_EPOCH + std::time::Duration::from_secs(1_792_059_180);
        assert_eq!(
            timestamp_hex(start),
            timestamp_hex(start + std::time::Duration::from_secs(59)),
            "同一分钟内的哈希必须可以互换，否则算哈希和写键会错开"
        );
        assert_ne!(
            timestamp_hex(start),
            timestamp_hex(start + std::time::Duration::from_secs(60))
        );
        assert_eq!(timestamp_hex(start).len(), 16);
    }

    #[test]
    fn base64_pads_like_the_real_thing() {
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(&[0xff]), "/w==");
    }
}
