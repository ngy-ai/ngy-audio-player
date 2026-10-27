//! 播放动作：点行、点播放、打开文件、去重。

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::*;

// `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，会盖掉 std 的 `#[test]`。
// 这一行把 std 的那个拿回来。
use core::prelude::v1::test;

use super::super::view::PlayerView;
use super::harness::{Harness, samples};

#[gpui_kit::test]
fn clicking_a_row_switches_the_current_track(cx: &mut TestAppContext) {
    let paths = samples();
    if paths.len() < 3 {
        eprintln!("跳过：缺少 testdata 样本");
        return;
    }

    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, cx| {
                view.add_paths(paths.clone(), false, window, cx)
            });
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");
    let state = cx.update(|app| {
        let view = view.read(app);
        (view.active_tracks().len(), view.current_index())
    });
    assert_eq!(state.0, 3, "三首样本都该进列表");
    assert_eq!(state.1, Some(0), "加载完成后应当从第一首开始");

    cx.update_window(handle.into(), |_, window, cx| {
        // 点第二行。命中测试要真的落在那一行上，否则这里会直接报错。
        window.click("track:1", cx);
        window.render_frame(cx);
    })
    .unwrap();

    let current = cx.update(|app| view.read(app).current_index());
    assert_eq!(current, Some(1), "点第二行之后当前曲目应当是它");
}

/// 刚打开程序时列表是从磁盘恢复的，没有任何当前曲目：点播放不能没反应。
#[gpui_kit::test]
fn play_button_starts_the_first_track_when_nothing_is_current(cx: &mut TestAppContext) {
    let paths = samples();
    if paths.len() < 2 {
        eprintln!("跳过：缺少 testdata 样本");
        return;
    }

    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, cx| {
                view.add_paths(paths.clone(), false, window, cx)
            });
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    let view = slot.borrow().clone().expect("视图没有建出来");
    // 删掉正在播的那首：列表还在、引擎也还有队列，但没有当前曲目。
    cx.update(|app| view.update(app, |view, cx| view.remove_track(0, cx)));
    assert_eq!(
        cx.update(|app| view.read(app).current_index()),
        None,
        "删掉正在播的那首之后不该还有当前曲目"
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("play-slot", cx);
    })
    .unwrap();

    assert_eq!(
        cx.update(|app| view.read(app).current_index()),
        Some(0),
        "没有当前曲目时，点播放应当从列表第一首开始"
    );
}

#[gpui_kit::test]
fn opening_a_file_switches_to_it_immediately(cx: &mut TestAppContext) {
    let paths = samples();
    if paths.len() < 3 {
        eprintln!("跳过：缺少 testdata 样本");
        return;
    }

    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let first_two = paths[..2].to_vec();
    let handle = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, cx| {
                view.add_paths(first_two.clone(), false, window, cx)
            });
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");
    assert_eq!(cx.update(|app| view.read(app).current_index()), Some(0));

    let third = paths[2].clone();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.add_paths(vec![third], true, window, cx));
    })
    .unwrap();

    assert_eq!(
        cx.update(|app| view.read(app).current_index()),
        Some(2),
        "打开文件后应当直接播它，而不是只加进列表"
    );
}

#[gpui_kit::test]
fn the_same_file_cannot_enter_a_playlist_twice(cx: &mut TestAppContext) {
    let paths = samples();
    if paths.len() < 3 {
        eprintln!("跳过：缺少 testdata 样本");
        return;
    }

    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        let first = paths[0].clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, cx| {
                view.add_paths(vec![first.clone()], false, window, cx);
                // 再从「文件」按钮加一次同一个文件：不该多出一行。
                view.add_paths(vec![first], false, window, cx);
            });
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");
    assert_eq!(
        cx.update(|app| view.read(app).active_tracks().len()),
        1,
        "同一个文件只能进列表一次"
    );
}
