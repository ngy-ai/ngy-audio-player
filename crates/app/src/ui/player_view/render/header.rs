//! 顶栏（图标、标题、菜单、窗口按钮）与下面的错误 / 提示条。

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::super::view::PlayerView;

impl PlayerView {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted_foreground = theme.muted_foreground;
        let danger = theme.danger;

        // 自绘标题栏：Windows / Linux 上系统标题栏是透明的，必须自己给窗口按钮，
        // 否则用户只能靠 Alt+F4 关窗。macOS 保留系统红绿灯。
        const SHOW_WINDOW_CONTROLS: bool = !cfg!(target_os = "macos");
        let control = |id: &'static str, icon: IconName, warning: bool| {
            let mut button = Button::new(id).ghost().small().icon(icon);
            if warning {
                button = button.text_color(danger);
            }
            button
        };

        let backend = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.backend)
            .map(|backend| backend.label().to_string());
        let backend_note = match (&backend, self.has_ffmpeg) {
            (Some(name), _) => format!("解码：{name}"),
            (None, true) => "解码：待命".into(),
            (None, false) => "解码：内置（未装 ffmpeg，部分格式不可用）".into(),
        };
        let track_count = self.active_tracks().len();

        h_flex()
            .h(px(56.0))
            .flex_none()
            .px_4()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(
                // 这一块是窗口拖动区：只包标题，不包菜单 / 按钮（否则点它们会变成拖窗）。
                h_flex()
                    .id("title-area")
                    .h_full()
                    .items_center()
                    .gap_3()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(img("app-icon.png").size(px(28.0)).flex_shrink_0())
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("声波"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted_foreground)
                                    .child(format!("{track_count} 首 · {backend_note}")),
                            ),
                    ),
            )
            // 菜单栏：原来的「文件」「文件夹」「主题」「设为默认」按钮都收进菜单。
            .child(self.render_menu_bar(cx))
            // 标题和窗口按钮之间的空白仍然可以拖窗。
            .child(
                div()
                    .id("title-drag")
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()),
            )
            .when(SHOW_WINDOW_CONTROLS, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().w(px(1.0)).h(px(20.0)).mx_1().bg(border))
                        .child(
                            control("win-min", IconName::Minus, false)
                                .on_click(|_, window, _| window.minimize_window()),
                        )
                        .child(
                            control("win-max", IconName::Maximize2, false)
                                .on_click(|_, window, _| window.zoom_window()),
                        )
                        .child(
                            control("win-close", IconName::X, true)
                                .on_click(|_, window, _| window.remove_window()),
                        ),
                )
            })
            .into_any_element()
    }

    /// 错误条优先于一次性提示：同一时间只说一件事。
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let primary = theme.primary;
        let danger = theme.danger;

        let error = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.error.clone())
            .or_else(|| self.device_error.clone());
        if let Some(error) = error {
            return Some(
                h_flex()
                    .px_6()
                    .py_2()
                    .gap_2()
                    .items_start()
                    .bg(danger.opacity(0.08))
                    .border_b_1()
                    .border_color(danger.opacity(0.25))
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(14.0))
                            .text_color(danger),
                    )
                    .child(div().text_xs().text_color(danger).child(error))
                    .into_any_element(),
            );
        }

        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(notice, _)| {
                h_flex()
                    .px_6()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .bg(primary.opacity(0.07))
                    .child(Icon::new(IconName::Info).size(px(14.0)).text_color(primary))
                    .child(div().text_xs().text_color(primary).child(notice.clone()))
                    .into_any_element()
            })
    }
}
