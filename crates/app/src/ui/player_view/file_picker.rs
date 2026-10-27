//! 原生「打开文件 / 打开文件夹」对话框，以及扫歌用的目录遍历。
//!
//! rfd 是**阻塞**的，必须在后台线程上开；这里只负责把结果送回视图。

use std::path::PathBuf;

use gpui_kit::*;

use ngy_audio_engine::format;

use super::view::PlayerView;

/// 文件对话框标题。重复点击时靠它找回已经打开的窗口。
const FILE_DIALOG_TITLE: &str = "选择音频文件";
/// 文件夹对话框标题。
const FOLDER_DIALOG_TITLE: &str = "选择音乐文件夹";

impl PlayerView {
    pub(super) fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(title) = self.dialog_open {
            focus_dialog(title);
            return;
        }
        self.dialog_open = Some(FILE_DIALOG_TITLE);
        let extensions = format::dialog_extensions();
        let dialog = rfd::FileDialog::new()
            .add_filter("音频文件", extensions)
            .add_filter("所有文件", &["*"])
            .set_title(FILE_DIALOG_TITLE)
            // 指定父窗口后才是真模态：打开期间主窗口不可操作，选择框也始终跟着主窗口。
            .set_parent(window);
        let task = cx
            .background_executor()
            .spawn(async move { dialog.pick_files().unwrap_or_default() });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dialog_open = None;
                if files.is_empty() {
                    return;
                }
                this.add_paths(files, true, window, cx);
            });
        })
        .detach();
    }

    pub(super) fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(title) = self.dialog_open {
            focus_dialog(title);
            return;
        }
        self.dialog_open = Some(FOLDER_DIALOG_TITLE);
        let dialog = rfd::FileDialog::new()
            .set_title(FOLDER_DIALOG_TITLE)
            .set_parent(window);
        let task = cx.background_executor().spawn(async move {
            let Some(folder) = dialog.pick_folder() else {
                return Vec::new();
            };
            scan_folder(&folder)
        });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dialog_open = None;
                if files.is_empty() {
                    return;
                }
                this.add_paths(files, false, window, cx);
            });
        })
        .detach();
    }
}

/// 把已经打开的原生选择框提到前台。
#[cfg(windows)]
fn focus_dialog(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 标题是以 NUL 结尾的 UTF-16 字符串；句柄只传给 user32 做窗口激活。
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
fn focus_dialog(_title: &str) {}

fn scan_folder(folder: &std::path::Path) -> Vec<PathBuf> {
    // 只扫两层：音乐库通常就是「专辑/曲目」两层，再深多半是在扫游戏资源目录。
    const MAX_FILES: usize = 2000;
    let mut found = Vec::new();
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 2 {
                    stack.push((path, depth + 1));
                }
            } else if format::is_audio_path(&path) {
                found.push(path);
                if found.len() >= MAX_FILES {
                    return found;
                }
            }
        }
    }
    found
}
