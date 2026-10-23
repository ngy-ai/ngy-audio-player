//! 应用启动。窗口装配只做三件事：初始化 gpui-kit、跟随系统外观、把主视图塞进 Root。

use std::borrow::Cow;

use gpui_kit::component::*;
use gpui_kit::{AssetSource, Result as AssetResult, SharedString};
use gpui_kit::*;

use crate::ui::PlayerView;

const APP_NAME: &str = "声波";
const DEFAULT_SIZE: (f32, f32) = (1120.0, 760.0);

/// 资源源：先用完整 Lucide 图标目录，再退回 gpui-kit 的默认资源。
///
/// 为什么要包一层：`gpui_kit::assets::Assets` 只打包了约 100 个「默认图标」，
/// 像 `music` / `skip-forward` / `volume-2` 这些都不在里头 —— 用了它们会**静默**渲染成空白
/// （不报错，只是那个位置什么都没有）。`AllAssets` 有全部 1900 个图标但没有默认资源，
/// 所以两个都要。
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> AssetResult<Option<Cow<'static, [u8]>>> {
        match gpui_kit::assets::AllAssets.load(path) {
            Ok(Some(data)) => Ok(Some(data)),
            _ => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> AssetResult<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path).unwrap_or_default();
        names.extend(gpui_kit::assets::AllAssets.list(path).unwrap_or_default());
        names.sort();
        names.dedup();
        Ok(names)
    }
}

pub fn run() -> anyhow::Result<()> {
    let app = gpui_kit::application().with_assets(AppAssets);

    app.run(|cx| {
        // gpui-kit 约定：必须在最早期初始化主题与全局配置。
        gpui_kit::init(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(initial_bounds(cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_NAME.into()),
                appears_transparent: true,
                // macOS 红绿灯按钮位置；自绘标题栏 56pt，系统默认位置会顶到边上。
                traffic_light_position: Some(point(px(12.0), px(18.0))),
            }),
            ..WindowOptions::default()
        };

        let opened = cx.open_window(options, |window, cx| {
            // 跟随系统外观：Windows / macOS / Linux 的深色模式都通过这里读。
            let dark = matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            );
            Theme::change(
                if dark { ThemeMode::Dark } else { ThemeMode::Light },
                Some(window),
                cx,
            );

            let view = cx.new(|cx| PlayerView::new(window, cx));
            // 约定：每个窗口的第一层必须是 Root（对话框 / 通知 / 文字选中都挂在它上面）。
            cx.new(|cx| Root::new(view, window, cx))
        });

        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => {
                eprintln!("[fatal] 创建窗口失败: {error:#}");
            }
        }
    });

    Ok(())
}

/// 首次打开的窗口位置：屏幕中央，且不超过屏幕的 90%。
fn initial_bounds(cx: &App) -> Bounds<Pixels> {
    let preferred = size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1));
    let Some(display) = cx.primary_display() else {
        return Bounds::new(point(px(80.0), px(80.0)), preferred);
    };
    let screen = display.default_bounds();
    // 不超过屏幕的 92%，小屏笔记本上不会把窗口顶出可视区。
    let width = preferred.width.min(screen.size.width * 0.92);
    let height = preferred.height.min(screen.size.height * 0.92);
    let origin = point(
        screen.origin.x + (screen.size.width - width) / 2.0,
        screen.origin.y + (screen.size.height - height) / 2.0,
    );
    Bounds::new(origin, size(width, height))
}
