//! 播放引擎与 UI 之间唯一的共享面：全是原子量 / 短锁，音频回调里不加阻塞锁。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::decode::Backend;
use crate::meta::TrackMeta;

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
    /// 「又播完一首」的序号：UI 比对它来触发自动下一首。
    pub finished_serial: u64,
    pub device: String,
}

pub struct Shared {
    pub active: AtomicBool,
    pub paused: AtomicBool,
    pub ended: AtomicBool,
    pub loading: AtomicBool,
    pub failed: Mutex<Option<String>>,
    pub track: Mutex<Option<std::sync::Arc<TrackMeta>>>,
    pub backend: Mutex<Option<Backend>>,

    /// seek 后的新原点（毫秒）。与 `seek_epoch` 配合，让音频回调知道该清零计数。
    pub baseline_ms: AtomicU64,
    pub seek_epoch: AtomicU64,
    pub position_ms: AtomicU64,
    pub duration_ms: AtomicU64,
    /// 音量，存的是 f32 的位模式。
    pub volume_bits: AtomicU32,
    /// 曲目播完的序号。
    pub finished_serial: AtomicU64,
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
            baseline_ms: AtomicU64::new(0),
            seek_epoch: AtomicU64::new(0),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            volume_bits: AtomicU32::new(0.8f32.to_bits()),
            finished_serial: AtomicU64::new(0),
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
        self.device.lock().map(|name| name.clone()).unwrap_or_default()
    }

    /// 音频回调是否应该往外送样本。
    pub fn should_render(&self) -> bool {
        self.active.load(Ordering::Relaxed)
            && !self.paused.load(Ordering::Relaxed)
            && !self.loading.load(Ordering::Relaxed)
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
        }
    }

    // —— 解码线程用的写入 ——

    pub(crate) fn begin_load(&self, track: std::sync::Arc<TrackMeta>) {
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
        let matches_current = self
            .track
            .lock()
            .map(|slot| {
                slot.as_ref()
                    .map(|current| current.path == track.path)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !matches_current {
            return;
        }

        if let Some(duration) = track.duration {
            self.duration_ms
                .store(duration.as_millis() as u64, Ordering::Relaxed);
        }
        if let Ok(mut slot) = self.track.lock() {
            *slot = Some(track);
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
