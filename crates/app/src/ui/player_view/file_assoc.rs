//! 界面上的「默认播放器」入口：三态共用同一个确认层，只是文案与动作不同。
//!
//! 真正的注册表操作在 `crate::shell`，这里只管问用户、显示结果、更新状态。

use std::time::Instant;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::*;
use gpui_kit::*;

use crate::shell;

use super::settings::write_setting;
use super::view::PlayerView;

impl PlayerView {
    /// 确认层里那条「不走系统设置」的快捷方式：文案要如实说清代价。
    fn force_button(view: gpui::WeakEntity<Self>) -> impl IntoElement {
        Button::new("force-default-player")
            .outline()
            .small()
            .icon(IconName::CircleCheck)
            .label("直接设为默认")
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| this.apply_force_defaults(cx));
                }
            })
    }

    /// 顶栏的默认播放器入口。
    pub(super) fn open_default_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let total = shell::extensions().len();
        match self.shell_status {
            shell::Status::Registered { defaults, .. } => {
                window.open_alert_dialog(cx, move |alert, _, cx| {
                    alert
                        .confirm()
                        .title("默认播放器")
                        .description(format!(
                            "声波已在系统里登记 {total} 种音频格式，其中 {defaults} 种已经交给声波打开。\n\n\
                             Windows 不允许程序自己改默认程序：点「去系统设置」会打开声波那一页，\
                             在那里把这些格式设成用声波打开（新版 Windows 顶部有「设置默认值」，一次覆盖全部）。\n\
                             「取消登记」会把声波从「打开方式」里移除，已设为默认的类型会退回询问。"
                        ))
                        .ok_text("去系统设置")
                        .cancel_text("取消登记")
                        .cancel_variant(ButtonVariant::Danger)
                        .on_ok({
                            let view = view.clone();
                            move |_, _, cx| {
                                if let Some(view) = view.upgrade() {
                                    view.update(cx, |this, cx| this.reopen_settings(cx));
                                }
                                true
                            }
                        })
                        .on_cancel({
                            let view = view.clone();
                            move |_, _, cx| {
                                if let Some(view) = view.upgrade() {
                                    view.update(cx, |this, cx| this.apply_unregister(cx));
                                }
                                true
                            }
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "高级：跳过系统设置，直接把全部格式写成默认。\
                                             这会绕过 Windows 的默认程序保护，系统大版本更新后可能失效。",
                                        ),
                                )
                                .child(Self::force_button(view.clone())),
                        )
                });
            }
            _ => {
                window.open_alert_dialog(cx, move |alert, _, cx| {
                    alert
                        .confirm()
                        .title("设为默认播放器")
                        .description(format!(
                            "接下来会把 {total} 种音频格式登记到系统（只写当前用户，不需要管理员权限）。\n\n\
                             然后会打开 Windows 的「默认应用」页 —— 还要在那里把它们设成用声波打开，\
                             系统不允许程序代劳（新版 Windows 顶部有「设置默认值」，一次覆盖全部）。"
                        ))
                        .ok_text("登记并前往设置")
                        .cancel_text("取消")
                        .on_ok({
                            let view = view.clone();
                            move |_, _, cx| {
                                if let Some(view) = view.upgrade() {
                                    view.update(cx, |this, cx| this.apply_register(cx));
                                }
                                true
                            }
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "高级：跳过系统设置，登记的同时直接把全部格式写成默认。\
                                             这会绕过 Windows 的默认程序保护，系统大版本更新后可能失效。",
                                        ),
                                )
                                .child(Self::force_button(view.clone())),
                        )
                });
            }
        }
    }

    /// 不管系统设置，直接写 `UserChoice` 把全部格式设为默认（见 ADR 0008）。
    fn apply_force_defaults(&mut self, cx: &mut Context<Self>) {
        let total = shell::extensions().len();
        match shell::force_defaults() {
            Ok(report) => {
                self.shell_status = shell::status();
                let text = if report.failures.is_empty() {
                    format!("已把 {total} 种格式直接设为默认")
                } else {
                    format!(
                        "已设 {} / {total} 种，{} 种失败（{}）",
                        report.applied,
                        report.failures.len(),
                        report.failures.first().map(String::as_str).unwrap_or("-")
                    )
                };
                self.notice = Some((text, Instant::now()));
            }
            Err(error) => {
                self.notice = Some((format!("直接设为默认失败：{error:#}"), Instant::now()));
            }
        }
        cx.notify();
    }

    fn apply_register(&mut self, cx: &mut Context<Self>) {
        let count = shell::extensions().len();
        match shell::register() {
            Ok(()) => {
                self.shell_status = shell::status();
                let text = match shell::open_settings() {
                    Ok(()) => format!("已登记 {count} 种格式，请在系统设置里把它们设成用声波打开"),
                    Err(error) => format!("已登记 {count} 种格式，但打开系统设置失败：{error:#}"),
                };
                self.notice = Some((text, Instant::now()));
            }
            Err(error) => {
                self.notice = Some((format!("登记失败：{error:#}"), Instant::now()));
            }
        }
        cx.notify();
    }

    fn apply_unregister(&mut self, cx: &mut Context<Self>) {
        let was_registered = matches!(self.shell_status, shell::Status::Registered { .. });
        match shell::unregister() {
            Ok(()) => {
                self.shell_status = shell::status();
                let text = if was_registered {
                    "已移除文件关联，声波不再出现在「打开方式」里"
                } else {
                    "本来就没有登记过文件关联"
                };
                self.notice = Some((text.into(), Instant::now()));
            }
            Err(error) => {
                self.notice = Some((format!("取消登记失败：{error:#}"), Instant::now()));
            }
        }
        cx.notify();
    }

    /// 「移除本程序的文件关联」：系统级副作用，先确认再动手。
    pub(super) fn confirm_unregister(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title("移除文件关联")
                .description(
                    "会把声波从系统的「打开方式」里移除，已设为默认的类型会退回「询问用什么程序打开」。\
                     下次还能重新登记。",
                )
                .ok_text("移除")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("取消")
                .on_ok({
                    let view = view.clone();
                    move |_, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| this.apply_unregister(cx));
                        }
                        true
                    }
                })
        });
    }

    pub(super) fn toggle_auto_maintain(&mut self, cx: &mut Context<Self>) {
        self.auto_maintain = !self.auto_maintain;
        write_setting("auto-maintain", if self.auto_maintain { "1" } else { "0" });
        cx.notify();
    }

    pub(super) fn reopen_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = shell::open_settings() {
            self.notice = Some((format!("打开系统设置失败：{error:#}"), Instant::now()));
        }
        self.shell_status = shell::status();
        cx.notify();
    }
}
