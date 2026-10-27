//! 注册表读写原语：只负责把 UTF-16 指针和错误码包成 `Result`。
//!
//! 这里不知道「ProgId」「关联」这些概念，上层要写什么由调用方决定。

use std::ptr::{null, null_mut};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegCreateKeyW,
    RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use super::plan::Entry;

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
    let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };
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
