//! 播放引擎与 UI 之间唯一的共享面：全是原子量 / 短锁，音频回调里不加阻塞锁。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::decode::Backend;
use crate::meta::TrackMeta;

/// 队列播到末尾之后怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Repeat {
    /// 播完最后一首就停。
    #[default]
    Off,
    /// 回到队列开头继续。
    All,
    /// 同一首反复播。
    One,
}

impl Repeat {
    fn code(self) -> u8 {
        match self {
            Repeat::Off => 0,
            Repeat::All => 1,
            Repeat::One => 2,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Repeat::All,
            2 => Repeat::One,
            _ => Repeat::Off,
        }
    }
}

/// 队列的播放方式。随机是一个开关而不是第四种循环：随机 × 循环才有完整语义。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueMode {
    pub repeat: Repeat,
    pub shuffle: bool,
}

/// UI 侧看到的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// 没有加载任何曲子。
    Idle,
    /// 正在打开解码器 / 读文件头。
    Loading,
    Playing,
    Paused,
    /// 播到了末尾（或解码流结束）。
    Ended,
    Failed,
}

impl PlaybackState {
    pub fn is_active(self) -> bool {
        matches!(self, PlaybackState::Playing | PlaybackState::Paused)
    }
}

/// 一次快照：UI 每帧读这个，不直接碰引擎内部。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: PlaybackState,
    /// 已播放秒数。
    pub position: f64,
    /// 总时长；未知时为 0。
    pub duration: f64,
    /// 0..1。
    pub volume: f32,
    pub backend: Option<Backend>,
    pub error: Option<String>,
    pub track: Option<std::sync::Arc<TrackMeta>>,
    /// 整条队列播完的序号。队列里还有下一首时引擎会自己接上，不会加这个数。
    pub finished_serial: u64,
    pub device: String,
    /// 当前曲目在队列里的下标（队列的原始顺序，不是播放顺序）。
    pub queue_index: Option<usize>,
    pub mode: QueueMode,
}

pub struct Shared {
    pub active: AtomicBool,
    pub paused: AtomicBool,
    pub ended: AtomicBool,
    pub loading: AtomicBool,
    pub failed: Mutex<Option<String>>,
    pub track: Mutex<Option<std::sync::Arc<TrackMeta>>>,
    pub backend: Mutex<Option<Backend>>,
    /// 最近一次标签探测结果。探测在后台线程跑，可能比 `Load` 命令先完成；
    /// 先记下来，等真正装载同一路径时再补，避免时长永远停在 0。
    probed: Mutex<Option<std::sync::Arc<TrackMeta>>>,

    /// seek 后的新原点（毫秒）。与 `seek_epoch` 配合，让音频回调知道该清零计数。
    pub baseline_ms: AtomicU64,
    pub seek_epoch: AtomicU64,
    pub position_ms: AtomicU64,
    pub duration_ms: AtomicU64,
    /// 音量，存的是 f32 的位模式。
    pub volume_bits: AtomicU32,
    /// 曲目播完的序号。
    pub finished_serial: AtomicU64,
    /// 当前曲目在队列里的下标；`u64::MAX` 表示没有。
    queue_index: AtomicU64,
    /// 循环方式，见 `Repeat::code`。
    repeat: AtomicU8,
    shuffle: AtomicBool,
    /// 要求音频回调把队列里剩下的旧数据丢掉（seek / 切歌时用）。
    pub flush: AtomicBool,

    /// 设备采样率 / 声道数。开流之后才知道，所以是原子量。
    pub rate: AtomicU32,
    pub out_channels: AtomicU16,

    /// 最近一段单声道样本，给频谱图用。音频线程只 `try_lock`，绝不在这里等。
    pub viz: Mutex<Vec<f32>>,
    /// 设备名，仅用于展示。
    device: Mutex<String>,
    /// 当前状态码，见 `state()`。
    state: AtomicU8,
}

impl Shared {
    pub fn new(device: String) -> Self {
        Self {
            active: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            loading: AtomicBool::new(false),
            failed: Mutex::new(None),
            track: Mutex::new(None),
            backend: Mutex::new(None),
            probed: Mutex::new(None),
            baseline_ms: AtomicU64::new(0),
            seek_epoch: AtomicU64::new(0),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            volume_bits: AtomicU32::new(0.8f32.to_bits()),
            finished_serial: AtomicU64::new(0),
            queue_index: AtomicU64::new(u64::MAX),
            repeat: AtomicU8::new(0),
            shuffle: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            rate: AtomicU32::new(48_000),
            out_channels: AtomicU16::new(2),
            viz: Mutex::new(Vec::new()),
            device: Mutex::new(device),
            state: AtomicU8::new(0),
        }
    }

    pub fn set_device(&self, rate: u32, channels: u16, name: String) {
        self.rate.store(rate, Ordering::Relaxed);
        self.out_channels.store(channels, Ordering::Relaxed);
        if let Ok(mut device) = self.device.lock() {
            *device = name;
        }
    }

    pub fn device_name(&self) -> String {
        self.device
            .lock()
            .map(|name| name.clone())
            .unwrap_or_default()
    }

    /// 音频回调是否应该往外送样本。
    pub fn should_render(&self) -> bool {
        self.active.load(Ordering::Relaxed)
            && !self.paused.load(Ordering::Relaxed)
            && !self.loading.load(Ordering::Relaxed)
    }

    pub fn set_mode(&self, mode: QueueMode) {
        self.repeat.store(mode.repeat.code(), Ordering::Relaxed);
        self.shuffle.store(mode.shuffle, Ordering::Relaxed);
    }

    pub fn mode(&self) -> QueueMode {
        QueueMode {
            repeat: Repeat::from_code(self.repeat.load(Ordering::Relaxed)),
            shuffle: self.shuffle.load(Ordering::Relaxed),
        }
    }

    pub fn set_queue_index(&self, index: Option<usize>) {
        let value = index.map(|index| index as u64).unwrap_or(u64::MAX);
        self.queue_index.store(value, Ordering::Relaxed);
    }

    fn queue_index(&self) -> Option<usize> {
        match self.queue_index.load(Ordering::Relaxed) {
            u64::MAX => None,
            value => Some(value as usize),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    /// 把 0..1 的推子位置换成增益。用平方曲线：人耳对响度的感知更接近能量，
    /// 线性推子在低端会显得「一过半就很大」。
    pub fn gain(&self) -> f32 {
        let volume = self.volume();
        if volume <= 0.001 {
            // 推到底就是真静音，而不是「很小声」。
            return 0.0;
        }
        (volume * volume).clamp(0.0, 1.0)
    }

    pub fn seek_to(&self, seconds: f64) {
        let ms = (seconds.max(0.0) * 1000.0) as u64;
        self.baseline_ms.store(ms, Ordering::Relaxed);
        self.position_ms.store(ms, Ordering::Relaxed);
        self.seek_epoch.fetch_add(1, Ordering::Relaxed);
        self.ended.store(false, Ordering::Relaxed);
    }

    pub fn set_error(&self, message: impl Into<String>) {
        if let Ok(mut slot) = self.failed.lock() {
            *slot = Some(message.into());
        }
        self.loading.store(false, Ordering::Relaxed);
        self.active.store(false, Ordering::Relaxed);
        self.ended.store(false, Ordering::Relaxed);
    }

    pub fn clear_error(&self) {
        if let Ok(mut slot) = self.failed.lock() {
            *slot = None;
        }
    }

    fn state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }

    fn set_state(&self, state: u8) {
        self.state.store(state, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let failed = self.failed.lock().ok().and_then(|slot| slot.clone());
        let track = self.track.lock().ok().and_then(|slot| slot.clone());
        let backend = self.backend.lock().ok().and_then(|slot| *slot);

        let state = if failed.is_some() {
            PlaybackState::Failed
        } else if track.is_none() {
            PlaybackState::Idle
        } else if self.loading.load(Ordering::Relaxed) {
            PlaybackState::Loading
        } else if self.ended.load(Ordering::Relaxed) {
            PlaybackState::Ended
        } else if self.paused.load(Ordering::Relaxed) {
            PlaybackState::Paused
        } else if self.state() == 0 {
            PlaybackState::Idle
        } else {
            PlaybackState::Playing
        };

        let duration = self.duration_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let position = self.position_ms.load(Ordering::Relaxed) as f64 / 1000.0;

        Snapshot {
            state,
            position: position.min(if duration > 0.0 { duration } else { position }),
            duration,
            volume: self.volume(),
            backend,
            error: failed,
            track,
            finished_serial: self.finished_serial.load(Ordering::Relaxed),
            device: self.device_name(),
            queue_index: self.queue_index(),
            mode: self.mode(),
        }
    }

    // —— 解码线程用的写入 ——

    pub(crate) fn begin_load(&self, track: std::sync::Arc<TrackMeta>) {
        // 标签探测是异步的，可能比这条 Load 命令先到（小文件、刚启动时很常见）。
        // 那次 refresh_track 会因为还没有「当前曲目」而被丢掉，这里把它捡回来。
        let track = self
            .probed
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .filter(|probed| probed.path == track.path)
            .unwrap_or(track);
        self.clear_error();
        self.active.store(false, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        self.ended.store(false, Ordering::Relaxed);
        self.loading.store(true, Ordering::Relaxed);
        self.set_state(1);
        if let Ok(mut slot) = self.track.lock() {
            *slot = Some(track.clone());
        }
        if let Ok(mut slot) = self.backend.lock() {
            *slot = None;
        }
        self.duration_ms.store(
            track.duration.map(|d| d.as_millis() as u64).unwrap_or(0),
            Ordering::Relaxed,
        );
        self.baseline_ms.store(0, Ordering::Relaxed);
        self.position_ms.store(0, Ordering::Relaxed);
        self.seek_epoch.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut viz) = self.viz.lock() {
            viz.clear();
        }
    }

    /// 标签探测完成：把真实元数据补进「当前正在播的这首」。
    ///
    /// 列表为了能立刻开播，先塞的是占位曲目（没有标题、没有时长）。探测回来
    /// 只更新列表是不够的：引擎里存的还是占位曲目，快照的时长就是 0，进度条
    /// 没有比例、总时长显示 0:00。这里顺手把时长也补上。
    pub(crate) fn refresh_track(&self, track: std::sync::Arc<TrackMeta>) {
        // 先记下来：即使当前还没装载这首，后面的 begin_load 也能补上时长。
        if let Ok(mut slot) = self.probed.lock() {
            *slot = Some(track.clone());
        }

        let matches_current = self
            .track
            .lock()
            .map(|mut slot| {
                let matches = slot
                    .as_ref()
                    .map(|current| current.path == track.path)
                    .unwrap_or(false);
                if matches {
                    *slot = Some(track.clone());
                }
                matches
            })
            .unwrap_or(false);
        if !matches_current {
            return;
        }

        if let Some(duration) = track.duration {
            self.duration_ms
                .store(duration.as_millis() as u64, Ordering::Relaxed);
        }
    }

    pub(crate) fn mark_ready(&self, backend: Backend, duration_ms: u64) {
        self.loading.store(false, Ordering::Relaxed);
        self.active.store(true, Ordering::Relaxed);
        self.set_state(1);
        if let Ok(mut slot) = self.backend.lock() {
            *slot = Some(backend);
        }
        if duration_ms > 0 {
            self.duration_ms.store(duration_ms, Ordering::Relaxed);
        }
    }

    pub(crate) fn mark_finished(&self) {
        if self.ended.swap(true, Ordering::Relaxed) {
            return;
        }
        self.finished_serial.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn track(path: &str, duration: Option<Duration>) -> std::sync::Arc<TrackMeta> {
        let mut meta = TrackMeta::placeholder(Path::new(path));
        meta.duration = duration;
        std::sync::Arc::new(meta)
    }

    #[test]
    fn metadata_probed_before_load_is_not_lost() {
        let shared = Shared::new("测试设备".into());
        let path = "C:/music/sample.wav";

        // 后台探测先回来：此时引擎里还没有「当前曲目」，旧逻辑会直接丢掉。
        shared.refresh_track(track(path, Some(Duration::from_secs(41))));
        assert_eq!(shared.snapshot().duration, 0.0);

        // Load 命令随后才到，拿的还是占位曲目；应该把探测结果补回来。
        shared.begin_load(track(path, None));
        assert_eq!(shared.snapshot().duration, 41.0);
        assert_eq!(
            shared.snapshot().track.and_then(|track| track.duration),
            Some(Duration::from_secs(41))
        );
    }

    #[test]
    fn probe_result_for_another_track_is_not_applied() {
        let shared = Shared::new("测试设备".into());
        let path = PathBuf::from("C:/music/a.wav");
        shared.begin_load(track(path.to_str().unwrap(), None));

        shared.refresh_track(track("C:/music/b.wav", Some(Duration::from_secs(3))));
        assert_eq!(shared.snapshot().duration, 0.0);

        shared.refresh_track(track(path.to_str().unwrap(), Some(Duration::from_secs(7))));
        assert_eq!(shared.snapshot().duration, 7.0);
    }
}
