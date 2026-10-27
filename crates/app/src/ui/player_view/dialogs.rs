//! 命名对话框与列表内搜索框。
//!
//! 两者都走 `open_alert_dialog`：输入框的事件在 gpui-component 里没有稳定的公开
//! 订阅面，用对话框拿「确认后的值」最稳。

use std::cell::Cell;
use std::time::Instant;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::*;

use super::view::PlayerView;

/// 命名对话框要做的事：新建一个列表，还是给已有列表改名。
#[derive(Clone, Copy)]
pub(super) enum NameTarget {
    New,
    Rename(u64),
}

impl PlayerView {
    /// 搜索当前列表。
    pub(super) fn open_search_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 带着当前关键字打开：再点一次搜索能看见、也能改掉现在的过滤条件。
        let current = self.query.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current)
                .placeholder("标题、艺人或专辑，留空恢复全部")
        });
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title("搜索当前列表")
                .description("只过滤你看到的这份列表，不改动文件。")
                .ok_text("应用")
                .cancel_text("取消")
                .child(Input::new(&input))
                .on_ok({
                    let input = input.clone();
                    let view = view.clone();
                    move |_, _, cx| {
                        let query = input.read(cx).value().to_string();
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| {
                                this.query = query.trim().to_string();
                                this.sync_queue();
                                cx.notify();
                            });
                        }
                        true
                    }
                })
        });
    }

    /// 打开新建 / 重命名的输入对话框。
    pub(super) fn open_name_dialog(
        &mut self,
        target: NameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, description, initial) = match target {
            NameTarget::New => (
                "新建播放列表",
                "给新列表起个名字，之后还能改。",
                self.default_playlist_name(),
            ),
            NameTarget::Rename(id) => {
                let Some(list) = self.playlists.iter().find(|list| list.id == id) else {
                    return;
                };
                ("重命名播放列表", "列表里的曲目不会变。", list.name.clone())
            }
        };

        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .placeholder("列表名称")
        });
        // 这个 builder 可能被重复调用；聚焦只做一次，否则会把焦点从输入框抢走。
        let focused = Cell::new(false);
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, window, cx| {
            if !focused.replace(true) {
                input.update(cx, |state, cx| {
                    // 预填的是「列表 3」这种默认名，全选后直接输入就能替换。
                    state.select_all(window, cx);
                    state.focus_handle(cx).focus(window, cx);
                });
            }
            alert
                .confirm()
                .title(title)
                .description(description)
                .ok_text("保存")
                .cancel_text("取消")
                .child(Input::new(&input))
                .on_ok({
                    let input = input.clone();
                    let view = view.clone();
                    move |_, _, cx| {
                        let name = input.read(cx).value().trim().to_string();
                        let Some(view) = view.upgrade() else {
                            return true;
                        };
                        if name.is_empty() {
                            view.update(cx, |this, cx| {
                                this.notice = Some(("列表名称不能为空".into(), Instant::now()));
                                cx.notify();
                            });
                            // 返回 false：对话框不关，用户可以接着改。
                            return false;
                        }
                        view.update(cx, |this, cx| this.apply_name(target, name, cx));
                        true
                    }
                })
        });
    }

    fn apply_name(&mut self, target: NameTarget, name: String, cx: &mut Context<Self>) {
        match target {
            NameTarget::New => self.create_playlist(name, cx),
            NameTarget::Rename(id) => self.rename_playlist(id, name, cx),
        }
    }
}
