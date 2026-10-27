//! 底部传输控制：进度条、上一首 / 播放 / 下一首、随机、排序、循环、音量。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::*;
use gpui_kit::*;

use ngy_audio_engine::PlaybackState;

use super::super::seek::{format_time, snapshot_can_seek};
use super::super::view::PlayerView;

impl PlayerView {
    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted_foreground = theme.muted_foreground;

        let position = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.position)
            .unwrap_or(0.0);
        let duration = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.duration)
            .unwrap_or(0.0);
        let volume = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.volume)
            .unwrap_or(0.8);
        // 没有装进解码器的曲目时进度条置灰：拖它只会得到一个没有意义的位置。
        let can_seek = snapshot_can_seek(self.snapshot.as_ref());

        let tracks = self.active_tracks();
        let current_index = self.current_index();
        let count = if tracks.is_empty() {
            String::new()
        } else {
            format!(
                "{}/{}",
                current_index.map(|index| index + 1).unwrap_or(0),
                tracks.len()
            )
        };

        let progress_row = h_flex()
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(44.0))
                    .text_xs()
                    .text_color(muted_foreground)
                    .child(format_time(position)),
            )
            .child(
                div().flex_1().child(
                    Slider::new(&self.progress)
                        .horizontal()
                        // 没在播东西时滑块是灰的：点它不该产生任何位置变化。
                        .disabled(!can_seek),
                ),
            )
            .child(
                div()
                    .w(px(44.0))
                    .text_xs()
                    .text_color(muted_foreground)
                    .child(format_time(duration)),
            );

        let controls_row = h_flex()
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    .w(px(220.0))
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(muted_foreground).child(count)),
            )
            .child(self.render_transport(cx))
            .child(
                h_flex()
                    .w(px(220.0))
                    .gap_2()
                    .items_center()
                    .justify_end()
                    .child(
                        Icon::new(if volume <= 0.001 {
                            IconName::VolumeX
                        } else {
                            IconName::Volume2
                        })
                        .size(px(16.0))
                        .text_color(muted_foreground),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .child(Slider::new(&self.volume).horizontal()),
                    ),
            );

        v_flex()
            .flex_none()
            .px_6()
            .pt_3()
            .pb_4()
            .gap_2()
            .border_t_1()
            .border_color(border)
            .child(progress_row)
            .child(controls_row)
            .into_any_element()
    }

    fn render_transport(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted;
        let playing = matches!(
            self.snapshot.as_ref().map(|snapshot| snapshot.state),
            Some(PlaybackState::Playing)
        );
        let play_icon = if playing {
            IconName::Pause
        } else {
            IconName::Play
        };

        // 随机是独立开关：开着的时候用主色，否则关掉了也看不出来。
        let shuffle_button = {
            let button = Button::new("shuffle")
                .ghost()
                .small()
                .icon(IconName::Shuffle)
                .tooltip(if self.shuffle {
                    "随机播放：开"
                } else {
                    "随机播放：关"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.shuffle = !this.shuffle;
                    if let Some(player) = &this.player {
                        player.set_mode(this.mode());
                    }
                    cx.notify();
                }));
            if self.shuffle {
                button.primary()
            } else {
                button
            }
        };

        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_full()
            .bg(muted.opacity(0.55))
            .items_center()
            .child(
                Button::new("prev")
                    .ghost()
                    .icon(IconName::SkipBack)
                    .on_click(cx.listener(|this, _, window, cx| this.step(-1, window, cx))),
            )
            .child(
                // Button 是 RenderOnce，挂不上 test_support，包一层给它一个可观测的身份。
                div().id("play-slot").test_support().child(
                    Button::new("play")
                        .primary()
                        .rounded_full()
                        .w(px(44.0))
                        .h(px(44.0))
                        .icon(play_icon)
                        .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
                ),
            )
            .child(
                Button::new("next")
                    .ghost()
                    .icon(IconName::SkipForward)
                    .on_click(cx.listener(|this, _, window, cx| this.step(1, window, cx))),
            )
            .child(shuffle_button)
            .child(
                // 排序是循环切换：默认（拖出来的顺序）→ 标题 → 艺人 → 时长。
                div()
                    .id("sort")
                    .test_support()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .hover(|this| this.bg(muted))
                    .child(self.sort.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sort = this.sort.next();
                        this.sync_queue();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("repeat")
                    .ghost()
                    .small()
                    .icon(self.repeat.icon())
                    .tooltip(self.repeat.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repeat = this.repeat.next();
                        if let Some(player) = &this.player {
                            player.set_mode(this.mode());
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
