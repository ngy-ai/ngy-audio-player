//! 菜单栏：菜单项、状态文案与统一分发。
//!
//! 用 `dropdown_menu` 自绘，而不是 `AppMenuBar`：后者只从全局注册的 action 菜单
//! 里取内容，而这里的每一项都要拿到 `Context<PlayerView>`（弹对话框、切主题），
//! 直接回调比注册一堆 action 少一层间接。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::shell;

use super::view::PlayerView;

/// 菜单栏里能触发的事情。
///
/// 菜单项的回调只能是 `Fn`，闭包被放进弹层后就无法再按值取用；
/// 用一个 `Copy` 的枚举回到 `PlayerView::run_menu_action` 统一分发。
#[derive(Clone, Copy)]
pub(super) enum MenuAction {
    OpenFiles,
    OpenFolder,
    DefaultPlayer,
    OpenDefaultApps,
    Unregister,
    ToggleAutoMaintain,
    ToggleTheme,
    Quit,
    /// 列表行的右键菜单：删掉第 N 首（N 是列表里的真实下标）。
    RemoveTrack(usize),
    /// 列表行的右键菜单：在文件管理器里定位第 N 首。
    RevealTrack(usize),
}

/// 造一个菜单项。视图已经关掉时点击什么也不做。
pub(super) fn menu_item(
    view: &WeakEntity<PlayerView>,
    action: MenuAction,
    label: impl Into<SharedString>,
    icon: Option<IconName>,
    checked: bool,
) -> PopupMenuItem {
    let view = view.clone();
    let mut item = PopupMenuItem::new(label).checked(checked);
    if let Some(icon) = icon {
        item = item.icon(icon);
    }
    item.on_click(move |_, window, cx| {
        if let Some(view) = view.upgrade() {
            view.update(cx, |this, cx| this.run_menu_action(action, window, cx));
        }
    })
}

impl PlayerView {
    /// 菜单项统一入口：每一项最终都落到已有的按钮处理函数上。
    pub(super) fn run_menu_action(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::OpenFiles => self.open_files(window, cx),
            MenuAction::OpenFolder => self.open_folder(window, cx),
            MenuAction::DefaultPlayer => self.open_default_dialog(window, cx),
            MenuAction::OpenDefaultApps => self.reopen_settings(cx),
            MenuAction::Unregister => self.confirm_unregister(window, cx),
            MenuAction::ToggleAutoMaintain => self.toggle_auto_maintain(cx),
            MenuAction::ToggleTheme => self.toggle_theme(window, cx),
            MenuAction::RemoveTrack(index) => self.remove_track(index, cx),
            MenuAction::RevealTrack(index) => self.reveal_track(index, cx),
            MenuAction::Quit => window.remove_window(),
        }
    }

    /// 「设为默认播放器」菜单项的状态：文案、图标、勾选都要说出当前进度，
    /// 否则用户点完不知道有没有生效。
    fn default_player_entry(&self) -> (String, IconName, bool) {
        match self.shell_status {
            // 带省略号：点它是弹确认层，不是立刻动手。
            shell::Status::Unsupported | shell::Status::Absent => {
                ("设为默认音频播放器…".to_string(), IconName::Settings, false)
            }
            shell::Status::Registered { defaults, total } if total > 0 && defaults == total => {
                ("已是默认音频播放器".to_string(), IconName::Settings, true)
            }
            shell::Status::Registered { defaults, total } if defaults > 0 => (
                format!("已默认 {defaults}/{total} 种格式"),
                IconName::Settings,
                false,
            ),
            shell::Status::Registered { .. } => {
                ("已登记为打开方式".to_string(), IconName::Settings, false)
            }
        }
    }

    pub(super) fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        Theme::change(
            if self.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );
        cx.notify();
    }

    /// 顶部菜单栏。
    pub(super) fn render_menu_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let top = |id: &'static str, label: &'static str| {
            Button::new(id).ghost().small().compact().label(label)
        };

        let (shell_label, shell_icon, shell_checked) = self.default_player_entry();
        // 非 Windows 没有「默认播放器」这回事，整个「设置」菜单都不出现。
        let supports_shell = !matches!(self.shell_status, shell::Status::Unsupported);
        let total = shell::extensions().len();
        // 状态行：登记是「能不能出现」，默认是「真的抢到没有」，两者分开说。
        let status_text = match self.shell_status {
            shell::Status::Unsupported => String::new(),
            shell::Status::Absent => format!("已登记关联：0/{total} 个格式"),
            shell::Status::Registered { defaults, .. } if defaults > 0 => {
                format!("已登记关联：{total}/{total} 个格式 · 已默认 {defaults}")
            }
            shell::Status::Registered { .. } => format!("已登记关联：{total}/{total} 个格式"),
        };
        let auto_maintain = self.auto_maintain;

        let file_view = view.clone();
        let file_menu = top("menu-file-button", "文件").dropdown_menu(move |menu, _, _| {
            menu.item(menu_item(
                &file_view,
                MenuAction::OpenFiles,
                "打开文件…",
                Some(IconName::FilePlus),
                false,
            ))
            .item(menu_item(
                &file_view,
                MenuAction::OpenFolder,
                "打开文件夹…",
                Some(IconName::FolderOpen),
                false,
            ))
            .separator()
            .item(menu_item(&file_view, MenuAction::Quit, "退出", None, false))
        });

        let theme_view = view.clone();
        let dark = self.dark;
        let view_menu = top("menu-view-button", "视图").dropdown_menu(move |menu, _, _| {
            menu.item(menu_item(
                &theme_view,
                MenuAction::ToggleTheme,
                "深色模式",
                None,
                dark,
            ))
        });

        let settings_view = view;
        let settings_menu = top("menu-settings-button", "设置").dropdown_menu(move |menu, _, _| {
            menu.item(menu_item(
                &settings_view,
                MenuAction::DefaultPlayer,
                shell_label.clone(),
                Some(shell_icon),
                shell_checked,
            ))
            .item(menu_item(
                &settings_view,
                MenuAction::OpenDefaultApps,
                "打开系统默认应用设置",
                None,
                false,
            ))
            .separator()
            .item(menu_item(
                &settings_view,
                MenuAction::Unregister,
                "移除本程序的文件关联",
                None,
                false,
            ))
            .separator()
            .item(menu_item(
                &settings_view,
                MenuAction::ToggleAutoMaintain,
                "启动时自动维护关联",
                None,
                auto_maintain,
            ))
            .separator()
            .item(PopupMenuItem::new(status_text.clone()).disabled(true))
        });

        // `Button` 是 RenderOnce，挂不上 test_support；包一层给它一个可点中的身份。
        let slot = |id: &'static str, menu: AnyElement| div().id(id).test_support().child(menu);

        h_flex()
            .gap_1()
            .items_center()
            .child(slot("menu-file", file_menu.into_any_element()))
            .child(slot("menu-view", view_menu.into_any_element()))
            .when(supports_shell, |this| {
                this.child(slot("menu-settings", settings_menu.into_any_element()))
            })
    }
}
