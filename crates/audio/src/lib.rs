//! 音频播放内核。
//!
//! 分层：
//!
//! * [`decode`] — 两条解码路径（Symphonia / ffmpeg），统一成 2 声道交错 f32；
//! * [`resample`] — 设备采样率与素材不一致时做 FFT 重采样；
//! * [`output`] — cpal 输出与无锁环形缓冲；
//! * [`state`] — 音频线程与 UI 之间唯一的共享面；
//! * [`engine`] — 把上面几块拼成 `Player`。
//!
//! 这个 crate **不依赖 GPUI**：它可以在没有任何界面的情况下被测试和复用
//! （命令行播放器、集成测试都直接用它）。

pub mod cue;
pub mod decode;
pub mod engine;
pub mod format;
pub mod meta;
pub mod output;
pub mod resample;
pub mod state;

pub use decode::{Backend, Decoder, StreamSpec};
pub use engine::{Player, Track, placeholder_track};
pub use meta::{CoverArt, TimeWindow, TrackMeta};
pub use state::{PlaybackState, QueueMode, Repeat, Snapshot};

/// 这个构建能播的格式（扩展名列表）。
pub fn supported_extensions() -> &'static [&'static str] {
    format::dialog_extensions()
}

/// 系统里有没有 ffmpeg 兜底。
pub fn has_ffmpeg() -> bool {
    decode::ffmpeg::available()
}
