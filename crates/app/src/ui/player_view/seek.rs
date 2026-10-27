//! 进度条与 seek 的换算：比例 → 秒数 → 真正能落到的位置。
//!
//! 全是纯函数，方便单测；视图只负责把结果交给引擎。

use std::time::Duration;

use gpui_kit::component::slider::SliderValue;
use ngy_audio_engine::{PlaybackState, Snapshot};

/// seek 到最右端时留的余量（秒）。解码器在末帧 seek 容易报 out-of-range，
/// ffmpeg 从结尾开流还可能一帧都读不出来。
pub(super) const SEEK_END_MARGIN: f64 = 0.05;
/// 刚发出的 seek 等多久还没落位就不再钉住进度条（秒）。
pub(super) const SEEK_SETTLE_TIMEOUT: Duration = Duration::from_millis(800);

/// 目标是否落在「有曲目、能 seek」的状态里。
pub(super) fn snapshot_can_seek(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot.duration > 0.0
            && matches!(
                snapshot.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Ended
            )
    })
}

/// 按 0..1 的比例算 seek 目标（秒）。
pub(super) fn seek_target(ratio: f32, duration: f64) -> f64 {
    seek_target_seconds(ratio as f64 * duration, duration)
}

/// 把秒数夹进文件真正能落到的范围：末尾 50ms 留给解码器的末帧保护。
pub(super) fn seek_target_seconds(target: f64, duration: f64) -> f64 {
    if !target.is_finite() {
        return 0.0;
    }
    let upper = (duration - SEEK_END_MARGIN).max(0.0);
    target.clamp(0.0, upper)
}

/// 从 SliderValue 里取单值。
pub(super) fn value_value(value: &SliderValue) -> Option<f32> {
    match value {
        SliderValue::Single(value) => Some(*value),
        SliderValue::Range(start, _) => Some(*start),
    }
}

pub(super) fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".into();
    }
    let total = seconds.round() as u64;
    let minutes = total / 60;
    let seconds = total % 60;
    if minutes >= 60 {
        format!("{}:{:02}:{:02}", minutes / 60, minutes % 60, seconds)
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
