//! 列表行：空状态、拖动换序、右键删除、搜索过滤。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::*;

// `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，会盖掉 std 的 `#[test]`。
// 这一行把 std 的那个拿回来。
use core::prelude::v1::test;

use ngy_audio_engine::meta::TrackMeta;

use super::super::queue::track_matches;
use super::super::view::PlayerView;
use super::harness::{Harness, samples, tagged};

#[gpui_kit::test]
fn empty_state_renders_hint_and_no_rows(cx: &mut TestAppContext) {
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
        // 没有任何曲目时，任何一行都不该存在。
        assert!(window.try_find("track:0").is_none());
    })
    .unwrap();

    let view = slot.borrow().clone().unwrap();
    assert_eq!(cx.update(|app| view.read(app).active_tracks().len()), 0);
}

#[gpui_kit::test]
fn tracks_can_be_reordered_and_removed(cx: &mut TestAppContext) {
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
    let order = |app: &mut gpui_kit::App| -> Vec<PathBuf> {
        view.read(app)
            .active_tracks()
            .iter()
            .map(|track| track.path.clone())
            .collect()
    };
    let before = cx.update(|app| order(app));

    cx.update(|app| view.update(app, |view, cx| view.move_track_to(0, 1, cx)));
    let after = cx.update(|app| order(app));
    assert_eq!(after[0], before[1], "下移之后第二首应当排到第一行");
    assert_eq!(after[1], before[0]);
    assert_eq!(after[2], before[2]);

    cx.update(|app| view.update(app, |view, cx| view.remove_track(1, cx)));
    let left = cx.update(|app| order(app));
    assert_eq!(left, vec![before[1].clone(), before[2].clone()]);
}

#[gpui_kit::test]
fn dropping_a_track_lands_at_the_drop_index(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let _window = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    let view = slot.borrow().clone().expect("视图没有建出来");
    // 用占位曲目就够：排序只看路径 / 标题，不读文件。
    let titles = cx.update(|app| {
        view.update(app, |view, cx| {
            view.playlists[0].tracks = ["a", "b", "c", "d"]
                .iter()
                .map(|name| Arc::new(TrackMeta::placeholder(Path::new(&format!("{name}.mp3")))))
                .collect();
            cx.notify();
            view.playlists[0]
                .tracks
                .iter()
                .map(|track| track.title.clone())
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(titles, vec!["a", "b", "c", "d"]);

    // 往下拖：c 落到第一行，其余保持相对顺序。
    cx.update(|app| view.update(app, |view, cx| view.move_track_to(2, 0, cx)));
    let titles = cx.update(|app| {
        view.read(app).playlists[0]
            .tracks
            .iter()
            .map(|track| track.title.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, vec!["c", "a", "b", "d"]);

    // 往上拖：再把 c 放到末尾。
    cx.update(|app| view.update(app, |view, cx| view.move_track_to(0, 3, cx)));
    let titles = cx.update(|app| {
        view.read(app).playlists[0]
            .tracks
            .iter()
            .map(|track| track.title.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, vec!["a", "b", "d", "c"]);

    // 越界的落点不动列表。
    cx.update(|app| view.update(app, |view, cx| view.move_track_to(0, 99, cx)));
    let titles = cx.update(|app| {
        view.read(app).playlists[0]
            .tracks
            .iter()
            .map(|track| track.title.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, vec!["a", "b", "d", "c"]);
}

#[gpui_kit::test]
fn row_context_menu_removes_the_clicked_track(cx: &mut TestAppContext) {
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
    let names = |app: &mut gpui_kit::App| -> Vec<String> {
        view.read(app)
            .active_tracks()
            .iter()
            .filter_map(|track| {
                track
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect()
    };
    let before = cx.update(|app| names(app));
    let current_before = cx.update(|app| view.read(app).current_index());

    // 右键第三行：菜单弹出来，但不会顺带切歌——菜单项要再点一次才生效。
    cx.update_window(handle.into(), |_, window, cx| {
        window.right_click("track:2", cx);
    })
    .unwrap();
    assert!(
        cx.update_window(handle.into(), |_, window, _| {
            window.try_find("popup-menu").is_some()
        })
        .unwrap(),
        "右键一行应当弹出上下文菜单"
    );
    assert_eq!(
        cx.update(|app| view.read(app).current_index()),
        current_before,
        "右键不该切歌"
    );

    // 菜单里第一项是「从列表移除」，键盘确认后只少这一行。
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    assert_eq!(
        cx.update(|app| names(app)),
        vec![before[0].clone(), before[1].clone()]
    );
}

#[test]
fn search_matches_title_artist_and_album_ignoring_case() {
    let track = tagged("a.mp3", "Lemon", "米津玄師", "STRAY SHEEP");
    assert!(track_matches(&track, ""), "空关键字应当放行");
    assert!(track_matches(&track, "lemon"), "标题不区分大小写");
    assert!(track_matches(&track, "stray"), "专辑也能命中");
    assert!(track_matches(&track, "米津"), "艺人也能命中");
    assert!(!track_matches(&track, "周杰伦"));
}

#[gpui_kit::test]
fn searching_filters_rows_and_clearing_restores_them(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let slot: Rc<RefCell<Option<Entity<PlayerView>>>> = Rc::new(RefCell::new(None));
    let handle = {
        let slot = slot.clone();
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            view.update(cx, |view, _cx| {
                view.playlists[0].tracks = vec![
                    tagged("a.mp3", "晴天", "周杰伦", "叶惠美"),
                    tagged("b.flac", "Lemon", "米津玄師", "STRAY SHEEP"),
                    tagged("c.wav", "夜曲", "周杰伦", "十一月的萧邦"),
                ];
                view.sync_queue();
            });
            *slot.borrow_mut() = Some(view.clone());
            Harness { view }
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("track:2").is_some(), "没过滤时三行都在");
    })
    .unwrap();

    let view = slot.borrow().clone().expect("视图没有建出来");

    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.query = "周杰伦".into();
            view.sync_queue();
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find("track:0").is_some());
        assert!(
            window.try_find("track:1").is_none(),
            "Lemon 不该留在过滤后的列表里"
        );
        assert!(window.try_find("track:2").is_some());
    })
    .unwrap();

    // 过滤后的可见行就是播放队列，队列里只该剩命中的两首。
    assert_eq!(
        cx.update(|app| view.read(app).visible_tracks().len()),
        2,
        "队列应当只剩命中关键字的曲目"
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.click("search-clear", cx);
        window.render_frame(cx);
    })
    .unwrap();

    let state = cx.update(|app| {
        let view = view.read(app);
        (view.query.clone(), view.visible_tracks().len())
    });
    assert!(state.0.is_empty(), "点标记应当清掉关键字");
    assert_eq!(state.1, 3, "清掉之后三首都要回来");
}
