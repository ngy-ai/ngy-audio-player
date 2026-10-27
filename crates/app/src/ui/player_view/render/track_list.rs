//! 曲目列表：一行一首，可拖动换序，右键菜单管删除与定位。

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use ngy_audio_engine::PlaybackState;

use super::super::menu::{MenuAction, menu_item};
use super::super::seek::format_time;
use super::super::view::PlayerView;

/// 拖动列表行的载荷。`from` 是按下时的行号，落点由接收行决定。
#[derive(Clone)]
struct TrackDrag {
    from: usize,
    title: String,
}

impl Render for TrackDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.title.clone())
    }
}

impl PlayerView {
    pub(super) fn render_track_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted;
        let muted_foreground = theme.muted_foreground;
        let foreground = theme.foreground;
        let primary = theme.primary;

        let playing = matches!(
            self.snapshot.as_ref().map(|snapshot| snapshot.state),
            Some(PlaybackState::Playing)
        );
        let tracks = self.active_tracks();
        let current_index = self.current_index();

        let mut list = v_flex()
            .id("playlist")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_1();

        if tracks.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("列表是空的。\n\n拖文件进来，或者用「文件」菜单打开文件 / 文件夹。"),
            );
        } else if self.view.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("没有匹配的曲目。换个关键字，或者点上面的搜索标记清掉过滤。"),
            );
        }

        // 行号跟着可见顺序走，`index` 仍是 `tracks` 里的真实下标——
        // 增删、重排、拖拽都用它，点击播放用行号。
        for (position, index) in self.view.clone().into_iter().enumerate() {
            let Some(track) = tracks.get(index) else {
                continue;
            };
            let selected = current_index == Some(index);
            let is_current = selected && playing;
            let duration_text = track
                .duration
                .map(|duration| format_time(duration.as_secs_f64()))
                .unwrap_or_else(|| "—".into());
            let subtitle = if track.artist.is_empty() {
                track.format.clone()
            } else {
                format!("{} · {}", track.artist, track.format)
            };
            // group 名带行号：同名的话，鼠标划过一行会让每一行的按钮都亮起来。
            let row_group = format!("track-row:{index}");
            list = list.child(
                h_flex()
                    .id(format!("track:{index}"))
                    .test_support()
                    .group(row_group.clone())
                    .flex_none()
                    .h(px(52.0))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(primary.opacity(0.10))
                            .border_l_2()
                            .border_color(primary)
                    })
                    .hover(|this| this.bg(muted))
                    // 整行可拖：按住任何位置拖动都能换顺序，松手时由落点行接手。
                    .on_drag(
                        TrackDrag {
                            from: index,
                            title: track.title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .drag_over::<TrackDrag>(move |style, _, _, _| {
                        style
                            .bg(primary.opacity(0.10))
                            .border_1()
                            .border_color(primary)
                    })
                    .on_drop(cx.listener(move |this, drag: &TrackDrag, _, cx| {
                        this.move_track_to(drag.from, index, cx);
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_position(position, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(18.0))
                            .flex()
                            .justify_center()
                            .text_xs()
                            .text_color(if selected { primary } else { muted_foreground })
                            .child(if is_current {
                                Icon::new(IconName::AudioLines)
                                    .size(px(14.0))
                                    .text_color(primary)
                                    .into_any_element()
                            } else if selected {
                                Icon::new(IconName::Pause)
                                    .size(px(14.0))
                                    .text_color(primary)
                                    .into_any_element()
                            } else {
                                div().child(format!("{}", position + 1)).into_any_element()
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .text_color(if selected { primary } else { foreground })
                                    .child(track.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .child(duration_text),
                    )
                    .child(
                        // 拖拽手柄只是个提示：真正可拖的是整行。和操作按钮一样，
                        // 平时不显示，鼠标落在这一行才淡入。
                        div()
                            .flex_none()
                            .opacity(0.0)
                            .group_hover(row_group.clone(), |this| this.opacity(1.0))
                            .child(
                                Icon::new(IconName::GripVertical)
                                    .size(px(14.0))
                                    .text_color(muted_foreground),
                            ),
                    )
                    // 行的操作都收进右键菜单：排序靠拖动，删除和定位文件不需要常驻按钮。
                    .context_menu({
                        let view = cx.entity().downgrade();
                        move |menu, _, _| {
                            menu.item(menu_item(
                                &view,
                                MenuAction::RemoveTrack(index),
                                "从列表移除",
                                Some(IconName::Trash),
                                false,
                            ))
                            .item(menu_item(
                                &view,
                                MenuAction::RevealTrack(index),
                                "定位文件",
                                Some(IconName::FolderOpen),
                                false,
                            ))
                        }
                    }),
            );
        }

        list.into_any_element()
    }
}
