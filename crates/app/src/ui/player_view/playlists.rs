//! 播放列表的增删改与切换，以及「外部打开」这张临时列表。

use std::time::Instant;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::*;
use gpui_kit::*;

use super::queue::Playlist;
use super::view::PlayerView;

/// 「外部打开」列表的名字：系统 / 命令行交进来的文件先落在这里。
/// 它不写盘、不许改名 —— 见 `CONTEXT.md` 里「外部打开」词条。
pub(super) const EXTERNAL_LIST: &str = "外部打开";

impl PlayerView {
    /// 新建一个播放列表。名字来自命名对话框，留空时退回「列表 N」。
    pub(super) fn create_playlist(&mut self, name: String, cx: &mut Context<Self>) {
        let id = self.next_playlist_id;
        self.next_playlist_id += 1;
        let name = name.trim();
        let name = if name.is_empty() {
            format!("列表 {id}")
        } else {
            name.to_string()
        };
        self.playlists.push(Playlist {
            id,
            name: name.clone(),
            tracks: Vec::new(),
            transient: false,
        });
        self.active = self.playlists.len() - 1;
        self.notice = Some((format!("已新建「{name}」"), Instant::now()));
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 下一个新列表的默认名字。用户不填也不会撞出一个同名列表。
    pub(super) fn default_playlist_name(&self) -> String {
        format!("列表 {}", self.next_playlist_id)
    }

    pub(super) fn rename_playlist(&mut self, id: u64, name: String, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(list) = self.playlists.iter_mut().find(|list| list.id == id) else {
            return;
        };
        list.name = name.to_string();
        self.notice = Some((format!("已重命名为「{name}」"), Instant::now()));
        self.save_library();
        cx.notify();
    }

    pub(super) fn switch_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.playlists.len() || index == self.active {
            return;
        }
        self.active = index;
        self.notice = None;
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 删除一个播放列表：至少保留一个；删掉正在看的那个就落到相邻的列表。
    /// 播放不因此中断——正在响的曲目哪怕来自被删的列表，也会一直播完。
    pub(super) fn remove_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        // 「外部打开」不算播放列表：判断「能不能删」时要把它排除。
        let persistent = self.playlists.iter().filter(|list| !list.transient).count();
        if persistent <= 1 || index >= self.playlists.len() || self.playlists[index].transient {
            return;
        }
        self.playlists.remove(index);
        if index == self.active {
            self.active = index.min(self.playlists.len() - 1);
        } else if index < self.active {
            self.active -= 1;
        }
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 删列表不可逆，先问一句。真正删除在确认按钮的回调里。
    pub(super) fn confirm_remove_playlist(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.playlists.len() <= 1 {
            return;
        }
        let Some(playlist) = self.playlists.get(index) else {
            return;
        };
        let id = playlist.id;
        let name = playlist.name.clone();
        let count = playlist.tracks.len();
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title("删除播放列表")
                .description(format!(
                    "确定删除「{name}」吗？列表里的 {count} 首曲目只会从列表移除，磁盘上的文件不会被删除。"
                ))
                .ok_text("删除")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("取消")
                .on_ok({
                    let view = view.clone();
                    // 确认之前列表可能已经变了，按 id 找回真正要删的那一张。
                    move |_, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| {
                                if let Some(index) =
                                    this.playlists.iter().position(|list| list.id == id)
                                {
                                    this.remove_playlist(index, cx);
                                }
                            });
                        }
                        true
                    }
                })
        });
    }

    /// 准备好「外部打开」列表：有就清空复用，没有就插到标签栏最前面。
    pub(super) fn ensure_external_list(&mut self) {
        if let Some(index) = self.playlists.iter().position(|list| list.transient) {
            self.playlists[index].tracks.clear();
            self.active = index;
        } else {
            let id = self.next_playlist_id;
            self.next_playlist_id += 1;
            self.playlists.insert(
                0,
                Playlist {
                    id,
                    name: EXTERNAL_LIST.into(),
                    tracks: Vec::new(),
                    transient: true,
                },
            );
            self.active = 0;
        }
        // 切了列表就得重算可见行，否则渲染还会拿上一张表的行号取曲目。
        self.sync_queue();
    }

    /// 系统 / 命令行交进来的文件：替换「外部打开」的内容，并立刻播第一首。
    pub(super) fn open_external(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_external_list();
        self.add_paths(paths, true, window, cx);
    }

    /// 关掉「外部打开」：只清内容，磁盘上的文件一个都不动。
    pub(super) fn clear_external_list(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.playlists.iter().position(|list| list.transient) else {
            return;
        };
        self.playlists.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        if self.active >= self.playlists.len() {
            self.active = self.playlists.len().saturating_sub(1);
        }
        self.sync_queue();
        cx.notify();
    }
}
