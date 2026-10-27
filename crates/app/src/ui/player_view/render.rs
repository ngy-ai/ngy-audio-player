//! 主界面组装：顶栏、提示条、主体（列表 + 正在播放）、传输控制。
//!
//! 每一块在自己的文件里：`render/header.rs`、`render/playlist_bar.rs`、
//! `render/track_list.rs`、`render/now_playing.rs`、`render/footer.rs`。

mod footer;
mod header;
mod now_playing;
mod playlist_bar;
mod track_list;

use gpui_kit::component::*;
use gpui_kit::*;

use super::view::PlayerView;

/// 左侧列表宽度。
const SIDEBAR_WIDTH: f32 = 340.0;

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 颜色先抄成值：下面要建很多闭包，借 `cx` 会冲突。
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;

        let mut root = v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "space" => this.toggle(window, cx),
                    "left" => this.seek_by(-5.0, cx),
                    "right" => this.seek_by(5.0, cx),
                    "up" => {
                        if let Some(player) = &this.player {
                            let volume = (player.snapshot().volume + 0.05).min(1.0);
                            player.set_volume(volume);
                            this.volume
                                .update(cx, |state, cx| state.set_value(volume, window, cx));
                        }
                    }
                    "down" => {
                        if let Some(player) = &this.player {
                            let volume = (player.snapshot().volume - 0.05).max(0.0);
                            player.set_volume(volume);
                            this.volume
                                .update(cx, |state, cx| state.set_value(volume, window, cx));
                        }
                    }
                    "n" => this.step(1, window, cx),
                    "p" => this.step(-1, window, cx),
                    "delete" | "backspace" => this.remove_current(cx),
                    _ => {}
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.add_paths(paths.paths().to_vec(), true, window, cx);
            }));

        root = root.child(self.render_header(cx));
        if let Some(status) = self.render_status_bar(cx) {
            root = root.child(status);
        }
        root = root.child(self.render_body(cx));

        root.child(self.render_footer(cx))
    }
}

impl PlayerView {
    /// 主体：左边列表，右边正在播放。
    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;

        h_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(border)
                    .bg(muted.opacity(0.35))
                    .child(self.render_playlist_bar(cx))
                    .child(self.render_track_list(cx)),
            )
            .child(self.render_now_playing(cx))
            .into_any_element()
    }
}
