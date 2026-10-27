//! 「直接设为默认」：绕过系统设置，自己写带哈希的 `UserChoice`。
//!
//! 这是唯一会碰 `UserChoice` 的地方（见 `docs/adr/0008-*.md`）：只在用户明确点了
//! 「直接设为默认」时调用，失败也只是一个扩展名没设上，不影响已经登记的部分。

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;

use super::associate::{notify_shell, register};
use super::plan::{APP_KEY, CAPABILITIES, PROG_ID, extensions};
use super::registry;
use super::user_choice::{ForceReport, hash_input, timestamp_hex, user_choice_hash};

/// 当前用户的 SID 字符串（小写）。它参与哈希，取错了整条记录就会被系统判废。
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
            registry::delete_tree(&path);
            write_choice_values(&path, &hash)
        }
    }
}

fn write_choice_values(path: &str, hash: &str) -> anyhow::Result<()> {
    let key = registry::create(path)?;
    // 先 ProgId 后 Hash：顺序反了偶尔不生效（SetUserFTA 的经验）。
    let result = registry::set_string(key, "ProgId", PROG_ID)
        .and_then(|()| registry::set_string(key, "Hash", hash));
    registry::close(key);
    result
}

/// 跳过系统设置，把全部格式直接写成默认。
pub fn force_defaults() -> anyhow::Result<ForceReport> {
    if !registry::exists(CAPABILITIES) {
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
pub fn open_settings() -> anyhow::Result<()> {
    let uri = format!("ms-settings:defaultapps?registeredAppUser={APP_KEY}");
    shell_execute(&uri).with_context(|| format!("打开系统设置失败：{uri}"))
}

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
