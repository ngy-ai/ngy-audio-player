//! 进度条与 seek 的纯函数。

use ngy_audio_engine::{PlaybackState, QueueMode, Snapshot};

use super::super::seek::{seek_target, seek_target_seconds, snapshot_can_seek};

#[test]
fn progress_is_only_seekable_while_something_is_loaded() {
    let snapshot = |state, duration| Snapshot {
        state,
        position: 0.0,
        duration,
        volume: 1.0,
        backend: None,
        error: None,
        track: None,
        finished_serial: 0,
        device: String::new(),
        queue_index: None,
        mode: QueueMode::default(),
    };

    assert!(!snapshot_can_seek(None), "还没装载曲目时不能拖进度");
    assert!(!snapshot_can_seek(Some(&snapshot(
        PlaybackState::Idle,
        10.0
    ))));
    assert!(!snapshot_can_seek(Some(&snapshot(
        PlaybackState::Loading,
        10.0
    ))));
    assert!(!snapshot_can_seek(Some(&snapshot(
        PlaybackState::Failed,
        10.0
    ))));
    assert!(
        !snapshot_can_seek(Some(&snapshot(PlaybackState::Playing, 0.0))),
        "时长未知时换算不出秒数，也不该让拖"
    );

    assert!(snapshot_can_seek(Some(&snapshot(
        PlaybackState::Playing,
        10.0
    ))));
    assert!(snapshot_can_seek(Some(&snapshot(
        PlaybackState::Paused,
        10.0
    ))));
    assert!(
        snapshot_can_seek(Some(&snapshot(PlaybackState::Ended, 10.0))),
        "播完之后还能拖回去重听"
    );
}

#[test]
fn seek_target_stays_off_the_last_frames() {
    assert!((seek_target(1.0, 10.0) - 9.95).abs() < 1e-9);
    assert_eq!(seek_target(0.0, 10.0), 0.0);
    assert_eq!(seek_target(0.5, 10.0), 5.0);
    assert_eq!(seek_target(1.0, 0.0), 0.0);
    assert_eq!(seek_target_seconds(-3.0, 10.0), 0.0);
}
