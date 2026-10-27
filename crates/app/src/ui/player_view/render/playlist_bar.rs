//! 播放列表标签栏：切换、改名、删除，以及搜索入口和过滤标记。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::super::dialogs::NameTarget;
use super::super::view::PlayerView;

impl PlayerView {
    pub(super) fn render_playlist_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;
        let muted_foreground = theme.muted_foreground;
        let primary = theme.primary;
        let danger = theme.danger;

        let mut bar = h_flex()
            .flex_none()
            .px_2()
            .py_2()
            .gap_1()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(border);

        // 标签上的小图标：点击区比图标大一圈，顺手拦住冒泡，别触发切列表。
        let tab_action = |id: String, icon: IconName, warning: bool| {
            div()
                .id(id)
                .test_support()
                .flex()
                .items_center()
                .justify_center()
                // 图标本身只有 12px，把点击区撑到 16px 才好点。
                .size(px(16.0))
                .rounded_full()
                .cursor_pointer()
                .opacity(0.45)
                .hover(|this| this.opacity(1.0))
                .when(warning, |this| this.text_color(danger))
                .child(Icon::new(icon).size(px(12.0)))
        };

        // 「外部打开」不是播放列表：它只能被清空，不能被改名、也不参与「至少留一张」的计数。
        let persistent_lists = self.playlists.iter().filter(|list| !list.transient).count();
        for (index, playlist) in self.playlists.iter().enumerate() {
            let selected = index == self.active;
            let transient = playlist.transient;
            let removable = transient || persistent_lists > 1;
            let editable = playlist.id;
            bar = bar.child(
                h_flex()
                    .id(format!("playlist-tab:{index}"))
                    .test_support()
                    .h(px(26.0))
                    .px_2()
                    .gap_1()
                    .rounded_md()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .when(selected, |this| {
                        this.bg(primary.opacity(0.14)).text_color(primary)
                    })
                    .when(!selected, |this| this.text_color(muted_foreground))
                    .hover(|this| this.bg(muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_playlist(index, cx)))
                    .child(
                        div()
                            .id(format!("playlist-name:{index}"))
                            .test_support()
                            .max_w(px(110.0))
                            .truncate()
                            .child(playlist.name.clone()),
                    )
                    .when(!transient, |this| {
                        this.child(
                            tab_action(
                                format!("playlist-rename:{index}"),
                                IconName::PencilLine,
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_name_dialog(NameTarget::Rename(editable), window, cx);
                                },
                            )),
                        )
                    })
                    .when(removable, |this| {
                        this.child(
                            tab_action(format!("playlist-remove:{index}"), IconName::X, true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // 不拦下来的话，点 × 会先把这个列表切到前台再弹确认。
                                    cx.stop_propagation();
                                    if transient {
                                        // 文件还在磁盘上，清空不需要确认。
                                        this.clear_external_list(cx);
                                    } else {
                                        this.confirm_remove_playlist(index, window, cx);
                                    }
                                })),
                        )
                    }),
            );
        }

        bar = bar.child(
            div()
                .id("new-playlist")
                .test_support()
                .child(
                    Button::new("new-playlist-button")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .tooltip("新建播放列表"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_name_dialog(NameTarget::New, window, cx)
                })),
        );

        bar = bar.child(
            div()
                .id("search-tracks")
                .test_support()
                .child(
                    Button::new("search-tracks-button")
                        .ghost()
                        .small()
                        .icon(IconName::Search)
                        .tooltip("搜索当前列表"),
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_search_dialog(window, cx))),
        );

        // 过滤生效时留一个能一键清掉的标记：列表里少了几首，看起来会像丢了数据。
        if !self.query.is_empty() {
            bar = bar.child(
                h_flex()
                    .id("search-filter")
                    .test_support()
                    .h(px(26.0))
                    .px_2()
                    .gap_1()
                    .rounded_full()
                    .items_center()
                    .text_xs()
                    .bg(primary.opacity(0.14))
                    .text_color(primary)
                    .child(
                        div()
                            .max_w(px(160.0))
                            .truncate()
                            .child(format!("搜索：{}", self.query)),
                    )
                    .child(
                        div()
                            .id("search-clear")
                            .test_support()
                            .size(px(16.0))
                            .rounded_full()
                            .cursor_pointer()
                            .child(Icon::new(IconName::X).size(px(12.0)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.query.clear();
                                this.sync_queue();
                                cx.notify();
                            })),
                    ),
            );
        }

        bar.into_any_element()
    }
}
