//! 视图的状态与生命周期：构造一次，之后靠 [`PlayerView::tick`] 跟着引擎走。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::*;

use ngy_audio_engine::engine::same_track;
use ngy_audio_engine::meta::TrackMeta;
use ngy_audio_engine::{PlaybackState, Player, Snapshot};

use crate::shell;
use crate::ui::spectrum::Spectrum;

use super::queue::{Playlist, RepeatMode, SortKey};
use super::seek::{SEEK_SETTLE_TIMEOUT, seek_target, snapshot_can_seek, value_value};
use super::settings::{initial_shell_status, library_path, read_setting};

/// 定时刷新间隔。频谱要跳得顺，但也没必要每帧算 FFT。
const TICK: Duration = Duration::from_millis(80);
/// 频谱柱数量。
pub(super) const SPECTRUM_BANDS: usize = 44;

pub struct PlayerView {
    pub(super) player: Option<Player>,
    pub(super) device_error: Option<String>,

    pub(super) playlists: Vec<Playlist>,
    /// 当前显示的列表下标，始终指向 `playlists` 里的有效项。
    pub(super) active: usize,
    /// 正在播放的曲目。用路径而不是下标记录：重排 / 删除曲目后依然指得准。
    pub(super) playing: Option<Arc<TrackMeta>>,
    pub(super) next_playlist_id: u64,
    pub(super) snapshot: Option<Snapshot>,

    pub(super) progress: Entity<SliderState>,
    pub(super) volume: Entity<SliderState>,
    /// 用户正在拖进度条：此时不要用播放位置覆盖滑块。
    pub(super) scrubbing: bool,
    /// 刚发出的 seek 目标与时间。引擎走位之前别让进度条先弹回旧位置。
    pub(super) seek_pending: Option<(f64, Instant)>,

    pub(super) spectrum: Spectrum,
    pub(super) levels: Vec<f32>,

    pub(super) cover: Option<Arc<RenderImage>>,
    pub(super) cover_for: Option<PathBuf>,

    pub(super) repeat: RepeatMode,
    /// 随机播放是独立开关：随机 × 循环才有完整语义。
    pub(super) shuffle: bool,
    /// 当前列表里可见的行：存的是 `tracks` 的下标，按显示顺序。
    pub(super) view: Vec<usize>,
    pub(super) sort: SortKey,
    /// 列表内搜索的关键字。空表示不过滤。
    pub(super) query: String,
    pub(super) has_ffmpeg: bool,
    pub(super) dark: bool,
    /// 一次性提示（例如「这个格式需要 ffmpeg」），过几秒自动消失。
    pub(super) notice: Option<(String, Instant)>,
    /// 已经打开的原生对话框标题：连点会叠出多个窗口，重复点击只把已有窗口提到前台。
    pub(super) dialog_open: Option<&'static str>,
    /// 系统里的默认播放器状态。非 Windows 恒为 `Unsupported`，界面上不显示入口。
    pub(super) shell_status: shell::Status,
    /// 启动时自动维护文件关联（登记过才会动手），存在 `settings.txt` 里。
    pub(super) auto_maintain: bool,
    /// 播放列表保存位置；`None` 表示这次运行不持久化（单元测试）。
    pub(super) library_path: Option<PathBuf>,
    pub(super) focus: FocusHandle,
}

impl PlayerView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 没有声卡也要能开界面看列表、读标签，否则用户连问题在哪都看不到。
        let (player, device_error) = match Player::new() {
            Ok(player) => (Some(player), None),
            Err(error) => {
                eprintln!("[audio] 打开输出设备失败: {error:#}");
                (
                    None,
                    Some(format!(
                        "没有可用的音频输出设备：{error:#}。检查系统输出设备后重启本程序。"
                    )),
                )
            }
        };

        // 进度条固定 0..1，用「已播比例」而不是绝对秒数：
        // 这样换歌时不用去改滑块的 max，少一处会不同步的状态。
        //
        // step 必须远小于 1：滑动组件会把值四舍五入到 step 的整数倍。默认的 1.0
        // 在 0..1 区间上只会得到 0 或 1 —— 拖到一半也会直接被吸到开头 / 结尾。
        let progress = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.0)
        });
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.8)
        });

        // 进度条：拖动过程中只更新数字，松手才真的 seek —— 边拖边 seek 会让
        // 解码线程反复重启，反而卡顿。
        cx.subscribe_in(
            &progress,
            window,
            |this, _, event: &SliderEvent, window, cx| {
                let (value, released) = match event {
                    SliderEvent::Change(value) => (value_value(value), false),
                    SliderEvent::Release(value) => (value_value(value), true),
                };
                let Some(ratio) = value else { return };
                // 没有在播的曲目、或者时长还没出来时，滑块本来就该是灰的；
                // 这里再守一道，免得叠在上面的键盘 / 无障碍操作把它拨出个假位置。
                if !this.can_seek() {
                    this.scrubbing = false;
                    return;
                }
                let duration = this
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.duration)
                    .unwrap_or(0.0);
                if released {
                    this.scrubbing = false;
                    let target = seek_target(ratio, duration);
                    if let Some(player) = &this.player {
                        player.seek(target);
                    }
                    // 解码线程处理命令要几十毫秒；这期间 tick 读到的还是旧位置，
                    // 不钉住的话进度条会先弹回去再跳过来。
                    this.seek_pending = Some((target, Instant::now()));
                    if let Some(snapshot) = &mut this.snapshot {
                        snapshot.position = target;
                    }
                } else {
                    this.scrubbing = true;
                    if let Some(snapshot) = &mut this.snapshot {
                        snapshot.position = ratio as f64 * duration;
                    }
                }
                let _ = window;
                cx.notify();
            },
        )
        .detach();

        cx.subscribe_in(&volume, window, |this, _, event: &SliderEvent, _, cx| {
            let value = match event {
                SliderEvent::Change(value) | SliderEvent::Release(value) => value_value(value),
            };
            if let (Some(value), Some(player)) = (value, &this.player) {
                player.set_volume(value);
                cx.notify();
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );

        let mut this = Self {
            player,
            device_error,
            playlists: vec![Playlist {
                id: 1,
                name: "列表 1".into(),
                tracks: Vec::new(),
                transient: false,
            }],
            active: 0,
            playing: None,
            // 编号只增不减：删掉「列表 2」再新建，不会冒出一个同名的列表。
            next_playlist_id: 2,
            snapshot: None,
            progress,
            volume,
            scrubbing: false,
            seek_pending: None,
            spectrum: Spectrum::new(SPECTRUM_BANDS),
            levels: vec![0.0; SPECTRUM_BANDS],
            cover: None,
            cover_for: None,
            repeat: RepeatMode::All,
            shuffle: false,
            view: Vec::new(),
            sort: SortKey::Manual,
            query: String::new(),
            has_ffmpeg: ngy_audio_engine::has_ffmpeg(),
            dark,
            notice: None,
            dialog_open: None,
            library_path: library_path(),
            shell_status: initial_shell_status(),
            auto_maintain: read_setting("auto-maintain").is_none_or(|value| value != "0"),
            focus,
        };

        // 程序被挪过位置（换了构建产物目录）之后，注册表里写的还是旧路径：
        // 那样双击音频文件什么都不会发生，比没登记更难查。
        if let Some(Err(error)) = this.auto_maintain.then(shell::maintain) {
            eprintln!("[shell] 维护文件关联失败: {error:#}");
        }

        // 上次退出时的播放列表先回来。
        this.restore_library(cx);

        // 双击文件 / 资源管理器交来的参数（`ngy-audio-player a.mp3 b.flac`）：
        // 进「外部打开」列表并立刻播放。不再追加到上次那份列表里 ——
        // 否则双击 A 之后响的可能是上次的 B。
        let args: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .collect();
        if !args.is_empty() {
            this.open_external(args, window, cx);
        }

        // 用户去系统设置里改完默认程序、再切回来时，按钮上的状态要跟着变。
        // Windows 不会通知我们「关联变了」，只能靠窗口激活回调补读一次。
        cx.observe_window_activation(window, |this, _window, cx| {
            if cfg!(test) {
                return;
            }
            let status = shell::status();
            if status != this.shell_status {
                this.shell_status = status;
                cx.notify();
            }
        })
        .detach();

        // 唯一一个常驻任务：定时读引擎状态、推动进度条、喂频谱。
        // 80ms 一次：比一帧慢，但足够让进度条与柱子看起来是连续的。
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        this
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(player) = &self.player else {
            return;
        };
        let mut snapshot = player.snapshot();

        // seek 命令发出后，引擎要不了一两百毫秒才会把新位置写回来。在这之前
        // 位置还停在原处：照实显示会让进度条先弹回去、再跳过来，看起来像「拖了没生效」。
        if let Some((target, at)) = self.seek_pending {
            if (snapshot.position - target).abs() < 0.25 || at.elapsed() > SEEK_SETTLE_TIMEOUT {
                self.seek_pending = None;
            } else {
                snapshot.position = target;
            }
        }

        // 标签探测在后台跑，可能比 Load 命令先完成而被引擎丢掉；时长一直为 0
        // 时补一次，别让进度条永远没有比例（拖到头会变成 seek 回 0）。
        if snapshot.duration <= 0.0
            && let Some(track) = self
                .current_index()
                .and_then(|index| self.active_tracks().get(index))
            && track.duration.is_some()
        {
            player.refresh_metadata(track.clone());
        }

        // 频谱：有声音才算，静音时让柱子自己落下去。
        let previous_levels = self.levels.clone();
        let samples = player.spectrum();
        if samples.is_empty() {
            self.levels = self.spectrum.silence().to_vec();
        } else {
            self.levels = self.spectrum.update(&samples).to_vec();
        }
        let levels_changed = previous_levels != self.levels;

        // 进度滑块：用户没在拖的时候才跟随播放位置。
        if !self.scrubbing {
            let ratio = if snapshot.duration > 0.0 {
                (snapshot.position / snapshot.duration).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.progress.update(cx, |state, cx| {
                state.set_value(ratio as f32, window, cx);
            });
        }

        // 引擎说在播哪一首就显示哪一首：自动切歌、上一首 / 下一首、点行播放
        // 最后都落到这里。UI 不再自己算下一首，只跟着引擎的权威结果走。
        let playing = snapshot.track.clone();
        let switched = match (&playing, &self.playing) {
            (Some(track), Some(current)) => !same_track(current, track),
            (Some(_), None) => true,
            _ => false,
        };
        if switched && let Some(track) = playing {
            self.playing = Some(track.clone());
            self.load_cover(&track, cx);
            self.progress.update(cx, |state, cx| {
                state.set_value(0.0, window, cx);
            });
        }

        // 暂停 / 播完 / 空闲时位置不再变化，没必要每 80ms 重建整棵元素树：
        // 列表越长，这次 notify 越贵。播放中照常每 tick 刷新（进度条 + 频谱）。
        let quiet = matches!(
            snapshot.state,
            PlaybackState::Idle | PlaybackState::Paused | PlaybackState::Ended
        );
        let moved = self.snapshot.as_ref().is_none_or(|old| {
            old.state != snapshot.state
                || old.queue_index != snapshot.queue_index
                || old.error != snapshot.error
                || (old.position - snapshot.position).abs() > 0.01
                || (old.duration - snapshot.duration).abs() > 0.01
                || (old.volume - snapshot.volume).abs() > 0.001
        });

        self.snapshot = Some(snapshot);
        if !quiet || moved || levels_changed {
            cx.notify();
        }
    }

    pub(super) fn active_tracks(&self) -> &[Arc<TrackMeta>] {
        self.playlists
            .get(self.active)
            .map(|playlist| playlist.tracks.as_slice())
            .unwrap_or(&[])
    }

    /// 正在播放的曲目在当前列表里的位置。不在当前列表（例如刚切到别的列表）就是 `None`。
    pub(super) fn current_index(&self) -> Option<usize> {
        let playing = self.playing.as_ref()?;
        self.active_tracks()
            .iter()
            .position(|track| same_track(track, playing))
    }

    /// 进度条能不能拖动：得真有一首曲子装进了解码器，而且知道总时长。
    ///
    /// 播完（`Ended`）仍算「可以拖」：引擎会把会话重新开起来，用户拖着回到中间
    /// 是合理操作。`Idle` / `Loading` / `Failed` 时没得可 seek，直接把滑块置灰。
    pub(super) fn can_seek(&self) -> bool {
        snapshot_can_seek(self.snapshot.as_ref())
    }
}
