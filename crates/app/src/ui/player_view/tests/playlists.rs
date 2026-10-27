//! 播放列表：各自持曲、标签栏切换、新建与删除的对话框流程。

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::TestAppContext;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::{Cancel, Confirm};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::*;

// `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，会盖掉 std 的 `#[test]`。
// 这一行把 std 的那个拿回来。
use core::prelude::v1::test;

use super::super::view::PlayerView;
use super::harness::{Harness, root_window, samples};

#[gpui_kit::test]
fn playlists_hold_their_own_tracks(cx: &mut TestAppContext) {
    let paths = samples();
    if paths.len() < 3 {
        eprintln!("跳过：缺少 testdata 样本");
        return;
    }

    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        let first_two = paths[..2].to_vec();
        let third = paths[2].clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, cx| {
                view.add_paths(first_two, false, window, cx);
                view.create_playlist("列表 2".into(), cx);
                view.add_paths(vec![third], false, window, cx);
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
        (
            view.playlists.len(),
            view.active,
            view.playlists
                .iter()
                .map(|list| list.tracks.len())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(state.0, 2, "应当有两个播放列表");
    assert_eq!(state.1, 1, "新建之后应当切到新列表");
    assert_eq!(state.2, vec![2, 1], "两个列表各管各的曲目");

    cx.update(|app| view.update(app, |view, cx| view.switch_playlist(0, cx)));
    assert_eq!(cx.update(|app| view.read(app).active_tracks().len()), 2);

    cx.update(|app| view.update(app, |view, cx| view.remove_playlist(0, cx)));
    let left = cx.update(|app| {
        let view = view.read(app);
        (
            view.playlists.len(),
            view.active,
            view.playlists[0].name.clone(),
        )
    });
    assert_eq!(left.0, 1, "至少保留一个播放列表");
    assert_eq!(left.1, 0);
    assert_eq!(left.2, "列表 2", "删掉列表 1 之后列表 2 应当还在");
}

#[gpui_kit::test]
fn playlist_bar_switches_and_adds_lists(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");

    // 建列表本身要经过命名对话框（弹框流程见下面的 Root 测试），这里直接造数据。
    cx.update(|app| view.update(app, |view, cx| view.create_playlist("我的精选".into(), cx)));
    assert_eq!(cx.update(|app| view.read(app).playlists.len()), 2);
    assert_eq!(
        cx.update(|app| view.read(app).active),
        1,
        "新建之后应当切到新列表"
    );
    assert_eq!(
        cx.update(|app| view.read(app).playlists[1].name.clone()),
        "我的精选",
        "列表该用用户输入的名字"
    );

    // 点名字区域切换，而不是点整个标签（标签中间现在可能落在铅笔图标上）。
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("playlist-name:0", cx);
    })
    .unwrap();
    assert_eq!(cx.update(|app| view.read(app).active), 0);

    cx.update(|app| view.update(app, |view, cx| view.remove_playlist(0, cx)));
    let left = cx.update(|app| {
        let view = view.read(app);
        (view.playlists.len(), view.playlists[0].name.clone())
    });
    assert_eq!(left.0, 1);
    assert_eq!(left.1, "我的精选", "删掉列表 1 之后列表 2 应当还在");
}

#[gpui_kit::test]
fn creating_a_playlist_asks_for_a_name(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = root_window(cx, slot.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");
    assert_eq!(cx.update(|app| view.read(app).playlists.len()), 1);

    cx.update_window(handle.into(), |_, window, cx| {
        window.click("new-playlist", cx);
        window.render_frame(cx);
    })
    .unwrap();

    assert!(
        cx.update_window(handle.into(), |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "新建列表应当先弹命名对话框"
    );
    assert_eq!(
        cx.update(|app| view.read(app).playlists.len()),
        1,
        "没确认之前不该真的建列表"
    );

    // 默认名已经填进输入框；直接确认就应该得到「列表 2」。
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
        window.render_frame(cx);
    })
    .unwrap();

    let names = cx.update(|app| {
        view.read(app)
            .playlists
            .iter()
            .map(|playlist| playlist.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        names,
        vec!["列表 1".to_string(), "列表 2".to_string()],
        "确认后应当按输入框里的名字新建列表"
    );
}

#[gpui_kit::test]
fn deleting_a_playlist_asks_for_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = root_window(cx, slot.clone());

    let view = slot.borrow().clone().expect("视图没有建出来");
    let names = Rc::new(RefCell::new(Vec::<String>::new()));

    let view_for_setup = view.clone();
    cx.update_window(handle.into(), |_, window, cx| {
        view_for_setup.update(cx, |view, cx| {
            view.create_playlist("第二张".into(), cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    *names.borrow_mut() = cx.update(|app| {
        view.read(app)
            .playlists
            .iter()
            .map(|playlist| playlist.name.clone())
            .collect()
    });
    assert_eq!(names.borrow().len(), 2);

    // 点 ×：先出确认框，列表还在。
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("playlist-remove:0", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        cx.update_window(handle.into(), |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "删除播放列表应当先确认"
    );
    assert_eq!(
        cx.update(|app| view.read(app).playlists.len()),
        2,
        "确认之前列表不能被删掉"
    );

    // 取消：列表原样保留。
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(Cancel), cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(cx.update(|app| view.read(app).playlists.len()), 2);

    // 再来一次并确认：只删掉被点的那一张。
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("playlist-remove:0", cx);
        window.render_frame(cx);
        window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
        window.render_frame(cx);
    })
    .unwrap();

    let left = cx.update(|app| {
        view.read(app)
            .playlists
            .iter()
            .map(|playlist| playlist.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(left, vec!["第二张".to_string()], "只该删掉被点的那一张");
}
