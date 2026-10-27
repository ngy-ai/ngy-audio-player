//! 播放列表持久化：恢复、文本往返、分轨时间窗、越界下标。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::TestAppContext;
use gpui_kit::*;

use ngy_audio_engine::meta::{TimeWindow, TrackMeta};

// `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，它会盖掉 std 的 `#[test]`，
// 结果是 `#[gpui_kit::test]` 展开出来的 `#[test]` 又指回 gpui 的那个 —— 递归到底。
// 这一行把 std 的那个拿回来。
use core::prelude::v1::test;

use super::super::library::{
    SavedTrack, parse_library, read_library, serialize_library, write_library,
};
use super::super::queue::Playlist;
use super::super::view::PlayerView;
use super::harness::Harness;

#[gpui_kit::test]
fn playlists_are_restored_from_disk(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let dir = std::env::temp_dir().join(format!("ngy-audio-player-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("library.txt");

    // 造一份「上次退出时」的列表，然后从一个全新的视图里恢复。
    let playlists = [
        Playlist {
            id: 1,
            name: "通勤".into(),
            tracks: vec![Arc::new(TrackMeta::placeholder(Path::new("a.mp3")))],
            transient: false,
        },
        Playlist {
            id: 2,
            name: "夜里".into(),
            tracks: vec![
                Arc::new(TrackMeta::placeholder(Path::new("b.flac"))),
                Arc::new(TrackMeta::placeholder(Path::new("c.wav"))),
            ],
            transient: false,
        },
    ];
    let lists: Vec<&Playlist> = playlists.iter().collect();
    write_library(&path, &lists, 1).expect("应当能写进临时目录");

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

    cx.update(|app| {
        view.update(app, |view, cx| {
            // 生产路径由配置目录决定；测试里显式指到临时文件。
            view.library_path = Some(path.clone());
            view.restore_library(cx);
        });
    });

    let (active, lists, second_list) = cx.update(|app| {
        let view = view.read(app);
        (
            view.active,
            view.playlists
                .iter()
                .map(|list| (list.name.clone(), list.tracks.len()))
                .collect::<Vec<_>>(),
            view.playlists[1]
                .tracks
                .iter()
                .map(|track| track.path.clone())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(active, 1, "上次选中的列表要回来");
    assert_eq!(
        lists,
        vec![("通勤".to_string(), 1), ("夜里".to_string(), 2)]
    );
    assert_eq!(
        second_list,
        vec![PathBuf::from("b.flac"), PathBuf::from("c.wav")]
    );

    // 恢复之后继续改动，要写回同一个文件。
    cx.update(|app| {
        view.update(app, |view, cx| {
            view.rename_playlist(1, "上班路上".into(), cx)
        });
    });
    let saved = read_library(&path).expect("改完名字文件应当还在");
    assert_eq!(saved.playlists[0].0, "上班路上");
    assert_eq!(saved.active, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn library_text_round_trips_names_and_paths() {
    let playlist = |name: &str, paths: &[&str]| Playlist {
        id: 1,
        name: name.into(),
        tracks: paths
            .iter()
            .map(|path| Arc::new(TrackMeta::placeholder(Path::new(path))))
            .collect(),
        transient: false,
    };

    // 名字里带制表符 / 换行 / 反斜杠，路径里带 Windows 盘符：都是真实会遇到的。
    let first = playlist("夜里的\t歌\n单", &[r"C:\音乐\一 - 二.flac", "/tmp/三.mp3"]);
    let second = playlist("空列表", &[]);
    let text = serialize_library(&[&first, &second], 1);
    let saved = parse_library(&text).expect("应当能解回来");

    assert_eq!(saved.active, 1);
    assert_eq!(saved.playlists.len(), 2);
    assert_eq!(saved.playlists[0].0, "夜里的\t歌\n单");
    assert_eq!(
        saved.playlists[0].1,
        vec![
            SavedTrack {
                path: PathBuf::from(r"C:\音乐\一 - 二.flac"),
                window: None,
            },
            SavedTrack {
                path: PathBuf::from("/tmp/三.mp3"),
                window: None,
            }
        ]
    );
    assert_eq!(saved.playlists[1].0, "空列表");
    assert!(saved.playlists[1].1.is_empty());
}

#[test]
fn library_keeps_cue_time_windows() {
    let window = TimeWindow {
        start: Duration::from_millis(1_000),
        end: Duration::from_millis(2_500),
    };
    let mut meta = TrackMeta::placeholder(Path::new("/tmp/album.flac"));
    meta.window = Some(window);
    meta.duration = Some(window.len());
    let playlist = Playlist {
        id: 1,
        name: "整轨".into(),
        tracks: vec![Arc::new(meta)],
        transient: false,
    };

    let saved = parse_library(&serialize_library(&[&playlist], 0)).expect("应当能解回来");
    assert_eq!(saved.playlists[0].1[0].window, Some(window));
}

#[test]
fn library_active_index_is_clamped() {
    let playlist = |name: &str| Playlist {
        id: 1,
        name: name.into(),
        tracks: Vec::new(),
        transient: false,
    };
    let (a, b) = (playlist("A"), playlist("B"));
    let text = serialize_library(&[&a, &b], 9);
    assert_eq!(
        parse_library(&text).unwrap().active,
        1,
        "越界的 active 要收回来"
    );
    assert!(parse_library("").is_none(), "空文件不算一份列表");
    assert!(parse_library("version 1\nactive 0\n").is_none());
}
