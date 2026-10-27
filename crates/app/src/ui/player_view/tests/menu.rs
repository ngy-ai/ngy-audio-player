//! 菜单栏：每个菜单弹自己的项、贴着触发按钮。

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::*;

// `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，会盖掉 std 的 `#[test]`。
// 这一行把 std 的那个拿回来。
use core::prelude::v1::test;

use super::super::view::PlayerView;
use super::harness::Harness;

#[gpui_kit::test]
fn each_menu_opens_its_own_items_under_its_trigger(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let add_view = |cx: &mut TestAppContext| {
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            Harness { view }
        })
    };

    // 每个菜单新开一个窗口验证：同一窗口里先开一个再开另一个得先关掉，
    // 而「关掉」不是这里要测的东西。
    let file_window = add_view(cx);
    cx.update_window(file_window.into(), |_, window, cx| {
        window.render_frame(cx);
        let trigger = window
            .try_find("menu-file")
            .expect("菜单栏没有渲染出来")
            .bounds();
        window.click("menu-file", cx);
        let menu = window
            .try_find("popup-menu")
            .expect("点「文件」之后应当弹出菜单")
            .bounds();
        assert!(
            (menu.origin.x - trigger.origin.x).abs() < px(1.0),
            "菜单应当贴着「文件」弹出"
        );
        assert!(menu.size.height > px(60.0), "「文件」菜单应当有好几项");
    })
    .unwrap();

    let view_window = add_view(cx);
    cx.update_window(view_window.into(), |_, window, cx| {
        window.render_frame(cx);
        let trigger = window
            .try_find("menu-view")
            .expect("菜单栏没有渲染出来")
            .bounds();
        window.click("menu-view", cx);
        let menu = window
            .try_find("popup-menu")
            .expect("点「视图」之后应当弹出菜单")
            .bounds();
        assert!(
            (menu.origin.x - trigger.origin.x).abs() < px(1.0),
            "「视图」应当弹出自己的菜单，而不是复用「文件」的弹层"
        );
        assert!(menu.size.height < px(60.0), "「视图」菜单只有一项");
    })
    .unwrap();
}
