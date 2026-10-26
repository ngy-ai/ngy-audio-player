//! 主视图：播放列表、正在播放、传输控制。

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use ngy_audio_engine::cue;
use ngy_audio_engine::engine::same_track;
use ngy_audio_engine::format;
use ngy_audio_engine::meta::{CoverArt, TimeWindow, TrackMeta};
use ngy_audio_engine::{PlaybackState, Player, QueueMode, Repeat, Snapshot};

use super::cover;
use super::spectrum::Spectrum;

const SIDEBAR_WIDTH: f32 = 340.0;
const SPECTRUM_BANDS: usize = 44;
const SPECTRUM_HEIGHT: f32 = 96.0;
/// 定时刷新间隔。频谱要跳得顺，但也没必要每帧算 FFT。
const TICK: Duration = Duration::from_millis(80);
/// 文件对话框标题。重复点击时靠它找回已经打开的窗口。
const FILE_DIALOG_TITLE: &str = "选择音频文件";
/// 文件夹对话框标题。
const FOLDER_DIALOG_TITLE: &str = "选择音乐文件夹";
/// seek 到最右端时留的余量（秒）。解码器在末帧 seek 容易报 out-of-range，
/// ffmpeg 从结尾开流还可能一帧都读不出来。
const SEEK_END_MARGIN: f64 = 0.05;
/// 刚发出的 seek 等多久还没落位就不再钉住进度条（秒）。
const SEEK_SETTLE_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "顺序播放",
            RepeatMode::All => "列表循环",
            RepeatMode::One => "单曲循环",
        }
    }

    /// 三种模式必须用不同图标，否则用户看不出当前处于哪种循环状态。
    fn icon(self) -> IconName {
        match self {
            // 顺序播放放到列表末尾就停，用带斜杠的循环图标表示「不循环」。
            RepeatMode::Off => IconName::RepeatOff,
            RepeatMode::All => IconName::Repeat,
            RepeatMode::One => IconName::Repeat1,
        }
    }

    fn to_engine(self) -> Repeat {
        match self {
            RepeatMode::Off => Repeat::Off,
            RepeatMode::All => Repeat::All,
            RepeatMode::One => Repeat::One,
        }
    }
}

/// 列表排序方式。`Manual` 是用户拖出来的顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortKey {
    Manual,
    Title,
    Artist,
    Duration,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            SortKey::Manual => SortKey::Title,
            SortKey::Title => SortKey::Artist,
            SortKey::Artist => SortKey::Duration,
            SortKey::Duration => SortKey::Manual,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortKey::Manual => "排序：默认",
            SortKey::Title => "排序：标题",
            SortKey::Artist => "排序：艺人",
            SortKey::Duration => "排序：时长",
        }
    }
}

/// 一个播放列表。
///
/// `id` 只做内部标识：异步探测标签回来时要靠它找回「当初加入的那个列表」，
/// 用户中途切换 / 删除列表时不会把标签写到别的列表里。
struct Playlist {
    id: u64,
    name: String,
    tracks: Vec<Arc<TrackMeta>>,
}

/// 命名对话框要做的事：新建一个列表，还是给已有列表改名。
#[derive(Clone, Copy)]
enum NameTarget {
    New,
    Rename(u64),
}

/// 拖动列表行的载荷。`from` 是按下时的行号，落点由接收行决定。
#[derive(Clone)]
struct TrackDrag {
    from: usize,
    title: String,
}

impl Render for TrackDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.title.clone())
    }
}

pub struct PlayerView {
    player: Option<Player>,
    device_error: Option<String>,

    playlists: Vec<Playlist>,
    /// 当前显示的列表下标，始终指向 `playlists` 里的有效项。
    active: usize,
    /// 正在播放的曲目。用路径而不是下标记录：重排 / 删除曲目后依然指得准。
    playing: Option<Arc<TrackMeta>>,
    next_playlist_id: u64,
    snapshot: Option<Snapshot>,

    progress: Entity<SliderState>,
    volume: Entity<SliderState>,
    /// 用户正在拖进度条：此时不要用播放位置覆盖滑块。
    scrubbing: bool,
    /// 刚发出的 seek 目标与时间。引擎走位之前别让进度条先弹回旧位置。
    seek_pending: Option<(f64, Instant)>,

    spectrum: Spectrum,
    levels: Vec<f32>,

    cover: Option<Arc<RenderImage>>,
    cover_for: Option<PathBuf>,

    repeat: RepeatMode,
    /// 随机播放是独立开关：随机 × 循环才有完整语义。
    shuffle: bool,
    /// 当前列表里可见的行：存的是 `tracks` 的下标，按显示顺序。
    view: Vec<usize>,
    sort: SortKey,
    /// 列表内搜索的关键字。空表示不过滤。
    query: String,
    has_ffmpeg: bool,
    dark: bool,
    /// 一次性提示（例如「这个格式需要 ffmpeg」），过几秒自动消失。
    notice: Option<(String, Instant)>,
    /// 已经打开的原生对话框标题：连点会叠出多个窗口，重复点击只把已有窗口提到前台。
    dialog_open: Option<&'static str>,
    /// 播放列表保存位置；`None` 表示这次运行不持久化（单元测试）。
    library_path: Option<PathBuf>,
    focus: FocusHandle,
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
            focus,
        };

        // 上次退出时的播放列表先回来；命令行文件随后追加到当前列表。
        this.restore_library(cx);

        // 从命令行拿到的文件（`ngy-audio-player a.mp3 b.flac`）。
        let args: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .collect();
        if !args.is_empty() {
            this.add_paths(args, false, window, cx);
        }

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

    fn active_tracks(&self) -> &[Arc<TrackMeta>] {
        self.playlists
            .get(self.active)
            .map(|playlist| playlist.tracks.as_slice())
            .unwrap_or(&[])
    }

    /// 正在播放的曲目在当前列表里的位置。不在当前列表（例如刚切到别的列表）就是 `None`。
    fn current_index(&self) -> Option<usize> {
        let playing = self.playing.as_ref()?;
        self.active_tracks()
            .iter()
            .position(|track| same_track(track, playing))
    }

    /// 进度条能不能拖动：得真有一首曲子装进了解码器，而且知道总时长。
    ///
    /// 播完（`Ended`）仍算「可以拖」：引擎会把会话重新开起来，用户拖着回到中间
    /// 是合理操作。`Idle` / `Loading` / `Failed` 时没得可 seek，直接把滑块置灰。
    fn can_seek(&self) -> bool {
        snapshot_can_seek(self.snapshot.as_ref())
    }

    /// 重算可见行。列表增删、标签回填、排序方式或搜索关键字变化之后都要调一次。
    ///
    /// `view` 存的是 `tracks` 的下标：**行号（可见位置）和真实下标从此是两回事**，
    /// 渲染和播放用行号，增删改重排用真实下标。搜索是在这里做的一次过滤，
    /// 所以过滤后的列表同时也是播放队列。
    fn refresh_view(&mut self) {
        let tracks = &self.playlists[self.active].tracks;
        let needle = self.query.trim().to_lowercase();
        let mut view: Vec<usize> = (0..tracks.len())
            .filter(|index| track_matches(&tracks[*index], &needle))
            .collect();
        match self.sort {
            SortKey::Manual => {}
            SortKey::Title => view.sort_by(|a, b| {
                tracks[*a]
                    .title
                    .to_lowercase()
                    .cmp(&tracks[*b].title.to_lowercase())
                    .then(a.cmp(b))
            }),
            SortKey::Artist => view.sort_by(|a, b| {
                let key = |index: usize| {
                    let track = &tracks[index];
                    (
                        track.artist.to_lowercase(),
                        track.album.to_lowercase(),
                        track.title.to_lowercase(),
                    )
                };
                key(*a).cmp(&key(*b))
            }),
            SortKey::Duration => view.sort_by_key(|index| tracks[*index].duration),
        }
        self.view = view;
    }

    /// 按显示顺序取出可见曲目。队列就是「用户看到的这一份」。
    fn visible_tracks(&self) -> Vec<Arc<TrackMeta>> {
        let tracks = self.active_tracks();
        self.view
            .iter()
            .filter_map(|index| tracks.get(*index))
            .cloned()
            .collect()
    }

    /// 播第 `position` 行（可见顺序）。
    fn play_position(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.visible_tracks();
        let Some(track) = items.get(position).cloned() else {
            return;
        };
        self.playing = Some(track.clone());
        self.scrubbing = false;
        self.seek_pending = None;
        if let Some(player) = &self.player {
            player.play(items, position);
        }
        self.load_cover(&track, cx);
        self.notice = None;
        self.progress.update(cx, |state, cx| {
            state.set_value(0.0, window, cx);
        });
        cx.notify();
    }

    /// 播「真实下标」为 `index` 的那首（命令行、拖放、cue 导入用）。
    fn play_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(position) = self.view.iter().position(|item| *item == index) {
            self.play_position(position, window, cx);
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.view.len();
        if count == 0 {
            return;
        }
        // 还没开始播时按「下一首」就是从头开始，按「上一首」从末尾开始。
        if self.playing.is_none() {
            let position = if delta < 0 { count - 1 } else { 0 };
            self.play_position(position, window, cx);
            return;
        }
        let Some(player) = &self.player else {
            return;
        };
        self.scrubbing = false;
        self.seek_pending = None;
        if delta < 0 {
            player.prev();
        } else {
            player.next();
        }
        cx.notify();
    }

    /// 列表增删 / 重排 / 排序方式变化之后同步给引擎。
    ///
    /// 顺手重算可见行：所有改动列表的地方本来就要调这个函数，把刷新挂在这里
    /// 就不会漏掉某一处（漏一处的表现是「列表里少了一行」这种难查的问题）。
    fn sync_queue(&mut self) {
        self.refresh_view();
        if let Some(player) = &self.player {
            player.set_queue(self.visible_tracks());
        }
    }

    fn mode(&self) -> QueueMode {
        QueueMode {
            repeat: self.repeat.to_engine(),
            shuffle: self.shuffle,
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = &self.player {
            player.toggle_pause();
            cx.notify();
        }
    }

    fn seek_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        if !self.can_seek() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if let Some(player) = &self.player {
            let target = seek_target_seconds(snapshot.position + delta, snapshot.duration);
            player.seek(target);
            self.seek_pending = Some((target, Instant::now()));
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.position = target;
            }
            cx.notify();
        }
    }

    fn load_cover(&mut self, track: &TrackMeta, cx: &mut Context<Self>) {
        if self.cover_for.as_ref() == Some(&track.path) {
            return;
        }
        self.cover_for = Some(track.path.clone());
        self.cover = None;

        let Some(CoverArt { bytes, .. }) = track.cover.clone() else {
            cx.notify();
            return;
        };

        // 解码一张 1000×1000 的 JPEG 大约几毫秒，但列表切换时不该卡住 UI 线程。
        let task = cx
            .background_executor()
            .spawn(async move { cover::decode(&bytes, 640) });
        cx.spawn(async move |this, cx| {
            let decoded = task.await;
            let _ = this.update(cx, |this, cx| {
                this.cover = decoded;
                cx.notify();
            });
        })
        .detach();
    }

    /// `play_first` 为真时，无论当前是否在播，都立刻切到这批文件的第一首。
    fn add_paths(
        &mut self,
        paths: Vec<PathBuf>,
        play_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // .cue 不是音频文件而是一张分轨表：展开成多条曲目再入列表。
        let (cues, audio): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|path| cue::is_cue_path(path));
        if !cues.is_empty() {
            self.add_cue_sheets(cues, play_first, window, cx);
        }

        let mut paths: Vec<PathBuf> = audio
            .into_iter()
            .filter(|path| format::is_audio_path(path))
            .collect();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            self.notice = Some(("这些文件不是支持的音频格式".into(), Instant::now()));
            cx.notify();
            return;
        }

        // 同一个文件只进列表一次：重复拖放、文件夹里重叠的目录都会撞上这一条。
        let mut seen: HashSet<String> = self
            .active_tracks()
            .iter()
            .map(|track| path_key(&track.path))
            .collect();
        let requested_first = paths.first().map(|path| path_key(path));
        let mut duplicates = 0usize;
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| {
                let fresh = seen.insert(path_key(path));
                if !fresh {
                    duplicates += 1;
                }
                fresh
            })
            .collect();

        if paths.is_empty() {
            // 「打开文件」点了列表里已有的那首：切过去播，而不是什么都不发生。
            if play_first
                && let Some(key) = requested_first
                && let Some(index) = self
                    .active_tracks()
                    .iter()
                    .position(|track| path_key(&track.path) == key)
            {
                self.play_index(index, window, cx);
                return;
            }
            self.notice = Some((
                format!("已经在当前列表里，跳过 {duplicates} 首"),
                Instant::now(),
            ));
            cx.notify();
            return;
        }

        let list_id = self.playlists[self.active].id;
        let start = self.playlists[self.active].tracks.len();
        // 先放占位：文件名立刻出现在列表里，标签慢慢补。
        self.playlists[self.active].tracks.extend(
            paths
                .iter()
                .map(|path| Arc::new(TrackMeta::placeholder(path))),
        );
        self.probe_tracks(
            list_id,
            paths.iter().map(|path| (path.clone(), None)).collect(),
            cx,
        );
        self.sync_queue();
        // 路径已经定了，标签只是展示信息：先落盘，「退出后列表还在」不依赖探测。
        self.save_library();

        let added = paths.len();
        if play_first || self.playing.is_none() {
            self.play_index(start, window, cx);
        } else {
            let text = if duplicates > 0 {
                format!("已加入 {added} 首，跳过 {duplicates} 首重复")
            } else {
                format!("已加入 {added} 首")
            };
            self.notice = Some((text, Instant::now()));
            cx.notify();
        }
    }

    /// 展开 cue 分轨表。要读一次整轨标签（最后一首的终点靠它），放后台跑。
    fn add_cue_sheets(
        &mut self,
        cues: Vec<PathBuf>,
        play_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_executor().spawn(async move {
            let mut tracks = Vec::new();
            for cue in cues {
                match cue::expand(&cue) {
                    Ok(mut expanded) => tracks.append(&mut expanded),
                    Err(error) => eprintln!("[cue] {}: {error:#}", cue.display()),
                }
            }
            tracks
        });
        cx.spawn_in(window, async move |this, cx| {
            let tracks = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.insert_tracks(tracks, play_first, window, cx);
            });
        })
        .detach();
    }

    /// 把已经带着完整元数据（含时间窗）的曲目直接放进列表。
    fn insert_tracks(
        &mut self,
        metas: Vec<TrackMeta>,
        play_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if metas.is_empty() {
            self.notice = Some(("这张 cue 里没有可用的分轨".into(), Instant::now()));
            cx.notify();
            return;
        }

        let mut seen: HashSet<String> = self
            .active_tracks()
            .iter()
            .map(|track| track_key(track))
            .collect();
        let start = self.playlists[self.active].tracks.len();
        let mut added = 0usize;
        for meta in metas {
            if seen.insert(track_key(&meta)) {
                self.playlists[self.active].tracks.push(Arc::new(meta));
                added += 1;
            }
        }
        if added == 0 {
            self.notice = Some(("这些分轨已经在列表里了".into(), Instant::now()));
            cx.notify();
            return;
        }

        self.sync_queue();
        self.save_library();
        if play_first || self.playing.is_none() {
            self.play_index(start, window, cx);
        } else {
            self.notice = Some((format!("已加入 {added} 首分轨"), Instant::now()));
            cx.notify();
        }
    }

    /// 后台读标签，回来后按「列表 id + 路径」回填。
    ///
    /// 用 id 而不是下标：用户中途重排、删歌、切列表都不会写错位置。刚启动恢复
    /// 播放列表时也走这条路——先把路径铺进列表，标签晚几百毫秒出现没关系。
    fn probe_tracks(
        &mut self,
        list_id: u64,
        entries: Vec<(PathBuf, Option<TimeWindow>)>,
        cx: &mut Context<Self>,
    ) {
        if entries.is_empty() {
            return;
        }
        let task = cx.background_executor().spawn(async move {
            entries
                .into_iter()
                .map(|(path, window)| {
                    Arc::new(match window {
                        Some(window) => TrackMeta::probe_window(&path, window),
                        None => TrackMeta::probe(&path),
                    })
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let metas = task.await;
            let _ = this.update(cx, |this, cx| {
                let playing = this.playing.clone();
                let updated = this
                    .playlists
                    .iter_mut()
                    .find(|list| list.id == list_id)
                    .and_then(|list| {
                        for meta in metas {
                            // 分轨曲目的路径相同，必须连时间窗一起比，否则整张碟
                            // 会被同一个标签覆盖成同名曲目。
                            if let Some(slot) = list
                                .tracks
                                .iter_mut()
                                .find(|track| same_track(track, &meta))
                            {
                                *slot = meta;
                            }
                        }
                        let playing = playing?;
                        list.tracks
                            .iter()
                            .find(|track| same_track(track, &playing))
                            .cloned()
                    });

                // 标签回来之后重新看一次封面：`play_track` 当时拿到的还是占位曲目，
                // 它没有封面，`cover_for` 却已经把路径记下了——不把这条记录清掉，
                // `load_cover` 会以为「这首看过、就是没封面」，封面永远不上屏。
                if let Some(track) = updated
                    && this
                        .playing
                        .as_ref()
                        .is_some_and(|playing| same_track(playing, &track))
                {
                    // 引擎里存的可能还是占位曲目（没有时长），把真实标签补进去，
                    // 否则进度条算不出比例、总时长显示 0:00。
                    if let Some(player) = &this.player {
                        player.refresh_metadata(track.clone());
                    }
                    if this.cover.is_none() && this.cover_for.as_ref() == Some(&track.path) {
                        this.cover_for = None;
                    }
                    this.playing = Some(track.clone());
                    this.load_cover(&track, cx);
                }
                // 标签回来了：按标题/艺人排序的列表要跟着重排。
                this.sync_queue();
                cx.notify();
            });
        })
        .detach();
    }

    fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(title) = self.dialog_open {
            focus_dialog(title);
            return;
        }
        self.dialog_open = Some(FILE_DIALOG_TITLE);
        let extensions = format::dialog_extensions();
        let dialog = rfd::FileDialog::new()
            .add_filter("音频文件", extensions)
            .add_filter("所有文件", &["*"])
            .set_title(FILE_DIALOG_TITLE)
            // 指定父窗口后才是真模态：打开期间主窗口不可操作，选择框也始终跟着主窗口。
            .set_parent(window);
        let task = cx
            .background_executor()
            .spawn(async move { dialog.pick_files().unwrap_or_default() });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dialog_open = None;
                if files.is_empty() {
                    return;
                }
                this.add_paths(files, true, window, cx);
            });
        })
        .detach();
    }

    fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(title) = self.dialog_open {
            focus_dialog(title);
            return;
        }
        self.dialog_open = Some(FOLDER_DIALOG_TITLE);
        let dialog = rfd::FileDialog::new()
            .set_title(FOLDER_DIALOG_TITLE)
            .set_parent(window);
        let task = cx.background_executor().spawn(async move {
            let Some(folder) = dialog.pick_folder() else {
                return Vec::new();
            };
            scan_folder(&folder)
        });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dialog_open = None;
                if files.is_empty() {
                    return;
                }
                this.add_paths(files, false, window, cx);
            });
        })
        .detach();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        Theme::change(
            if self.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );
        cx.notify();
    }

    /// 从当前列表移除一行；移掉的正是正在播的那首就停下，
    /// 免得界面已经没这首歌、引擎还在出声。
    fn remove_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(track) = self.active_tracks().get(index).cloned() else {
            return;
        };
        self.playlists[self.active].tracks.remove(index);
        self.sync_queue();
        if self
            .playing
            .as_ref()
            .is_some_and(|playing| same_track(playing, &track))
        {
            self.playing = None;
            self.cover = None;
            self.cover_for = None;
            if let Some(player) = &self.player {
                player.stop();
            }
        }
        self.save_library();
        cx.notify();
    }

    fn remove_current(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.current_index() {
            self.remove_track(index, cx);
        }
    }

    /// 上移 / 下移一行。只改列表顺序，不影响正在播放的曲目。
    fn move_track(&mut self, index: usize, delta: isize, cx: &mut Context<Self>) {
        let Some(target) = index.checked_add_signed(delta) else {
            return;
        };
        self.move_track_to(index, target, cx);
    }

    /// 把第 `from` 行搬到 `to` 的位置（拖动排序）。
    fn move_track_to(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let tracks = &mut self.playlists[self.active].tracks;
        if from >= tracks.len() || to >= tracks.len() || from == to {
            return;
        }
        let track = tracks.remove(from);
        tracks.insert(to, track);
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 新建一个播放列表。名字来自命名对话框，留空时退回「列表 N」。
    fn create_playlist(&mut self, name: String, cx: &mut Context<Self>) {
        let id = self.next_playlist_id;
        self.next_playlist_id += 1;
        let name = name.trim();
        let name = if name.is_empty() {
            format!("列表 {id}")
        } else {
            name.to_string()
        };
        self.playlists.push(Playlist {
            id,
            name: name.clone(),
            tracks: Vec::new(),
        });
        self.active = self.playlists.len() - 1;
        self.notice = Some((format!("已新建「{name}」"), Instant::now()));
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 下一个新列表的默认名字。用户不填也不会撞出一个同名列表。
    fn default_playlist_name(&self) -> String {
        format!("列表 {}", self.next_playlist_id)
    }

    fn rename_playlist(&mut self, id: u64, name: String, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(list) = self.playlists.iter_mut().find(|list| list.id == id) else {
            return;
        };
        list.name = name.to_string();
        self.notice = Some((format!("已重命名为「{name}」"), Instant::now()));
        self.save_library();
        cx.notify();
    }

    fn switch_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.playlists.len() || index == self.active {
            return;
        }
        self.active = index;
        self.notice = None;
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 删除一个播放列表：至少保留一个；删掉正在看的那个就落到相邻的列表。
    /// 播放不因此中断——正在响的曲目哪怕来自被删的列表，也会一直播完。
    fn remove_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.playlists.len() <= 1 || index >= self.playlists.len() {
            return;
        }
        self.playlists.remove(index);
        if index == self.active {
            self.active = index.min(self.playlists.len() - 1);
        } else if index < self.active {
            self.active -= 1;
        }
        self.sync_queue();
        self.save_library();
        cx.notify();
    }

    /// 搜索当前列表。走和改名一样的原生输入框：输入框的事件在 gpui-component
    /// 里没有稳定的公开订阅面，用对话框拿「确认后的值」最稳。
    fn open_search_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 带着当前关键字打开：再点一次搜索能看见、也能改掉现在的过滤条件。
        let current = self.query.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current)
                .placeholder("标题、艺人或专辑，留空恢复全部")
        });
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title("搜索当前列表")
                .description("只过滤你看到的这份列表，不改动文件。")
                .ok_text("应用")
                .cancel_text("取消")
                .child(Input::new(&input))
                .on_ok({
                    let input = input.clone();
                    let view = view.clone();
                    move |_, _, cx| {
                        let query = input.read(cx).value().to_string();
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| {
                                this.query = query.trim().to_string();
                                this.sync_queue();
                                cx.notify();
                            });
                        }
                        true
                    }
                })
        });
    }

    /// 打开新建 / 重命名的输入对话框。
    fn open_name_dialog(
        &mut self,
        target: NameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, description, initial) = match target {
            NameTarget::New => (
                "新建播放列表",
                "给新列表起个名字，之后还能改。",
                self.default_playlist_name(),
            ),
            NameTarget::Rename(id) => {
                let Some(list) = self.playlists.iter().find(|list| list.id == id) else {
                    return;
                };
                ("重命名播放列表", "列表里的曲目不会变。", list.name.clone())
            }
        };

        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .placeholder("列表名称")
        });
        // 这个 builder 可能被重复调用；聚焦只做一次，否则会把焦点从输入框抢走。
        let focused = Cell::new(false);
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, window, cx| {
            if !focused.replace(true) {
                input.update(cx, |state, cx| {
                    // 预填的是「列表 3」这种默认名，全选后直接输入就能替换。
                    state.select_all(window, cx);
                    state.focus_handle(cx).focus(window, cx);
                });
            }
            alert
                .confirm()
                .title(title)
                .description(description)
                .ok_text("保存")
                .cancel_text("取消")
                .child(Input::new(&input))
                .on_ok({
                    let input = input.clone();
                    let view = view.clone();
                    move |_, _, cx| {
                        let name = input.read(cx).value().trim().to_string();
                        let Some(view) = view.upgrade() else {
                            return true;
                        };
                        if name.is_empty() {
                            view.update(cx, |this, cx| {
                                this.notice = Some(("列表名称不能为空".into(), Instant::now()));
                                cx.notify();
                            });
                            // 返回 false：对话框不关，用户可以接着改。
                            return false;
                        }
                        view.update(cx, |this, cx| this.apply_name(target, name, cx));
                        true
                    }
                })
        });
    }

    fn apply_name(&mut self, target: NameTarget, name: String, cx: &mut Context<Self>) {
        match target {
            NameTarget::New => self.create_playlist(name, cx),
            NameTarget::Rename(id) => self.rename_playlist(id, name, cx),
        }
    }

    /// 删列表不可逆，先问一句。真正删除在确认按钮的回调里。
    fn confirm_remove_playlist(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.playlists.len() <= 1 {
            return;
        }
        let Some(playlist) = self.playlists.get(index) else {
            return;
        };
        let id = playlist.id;
        let name = playlist.name.clone();
        let count = playlist.tracks.len();
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title("删除播放列表")
                .description(format!(
                    "确定删除「{name}」吗？列表里的 {count} 首曲目只会从列表移除，磁盘上的文件不会被删除。"
                ))
                .ok_text("删除")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("取消")
                .on_ok({
                    let view = view.clone();
                    // 确认之前列表可能已经变了，按 id 找回真正要删的那一张。
                    move |_, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| {
                                if let Some(index) =
                                    this.playlists.iter().position(|list| list.id == id)
                                {
                                    this.remove_playlist(index, cx);
                                }
                            });
                        }
                        true
                    }
                })
        });
    }

    /// 把播放列表写成文本；失败只记日志，不该影响播放。
    fn save_library(&self) {
        let Some(path) = &self.library_path else {
            return;
        };
        if let Err(error) = write_library(path, &self.playlists, self.active) {
            eprintln!("[library] 保存播放列表失败: {error}");
        }
    }

    /// 读回上次的播放列表。标签在后台补，先把路径铺上去。
    fn restore_library(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.library_path else {
            return;
        };
        let Some(saved) = read_library(path) else {
            return;
        };
        if saved.playlists.is_empty() {
            return;
        }

        self.playlists = saved
            .playlists
            .into_iter()
            .enumerate()
            .map(|(index, (name, entries))| Playlist {
                id: index as u64 + 1,
                name,
                tracks: entries
                    .iter()
                    .map(|entry| {
                        Arc::new(match entry.window {
                            Some(window) => TrackMeta::placeholder_window(&entry.path, window),
                            None => TrackMeta::placeholder(&entry.path),
                        })
                    })
                    .collect(),
            })
            .collect();
        self.next_playlist_id = self.playlists.len() as u64 + 1;
        self.active = saved.active.min(self.playlists.len() - 1);

        let probes: Vec<(u64, Vec<ProbeEntry>)> = self
            .playlists
            .iter()
            .map(|playlist| {
                (
                    playlist.id,
                    playlist
                        .tracks
                        .iter()
                        .map(|track| (track.path.clone(), track.window))
                        .collect(),
                )
            })
            .collect();
        for (list_id, entries) in probes {
            self.probe_tracks(list_id, entries, cx);
        }
        self.sync_queue();
    }
}

/// 判断「同一个文件」用的键：先规范化掉 `.` / `..` 与大小写盘符的差异，
/// Windows 上再统一小写。规范化失败（文件刚被删）就退回原始路径。
fn path_key(path: &Path) -> String {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = full.to_string_lossy().into_owned();
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

/// 列表内搜索的匹配规则：标题 / 艺人 / 专辑任一命中即可（不区分大小写）。
///
/// `needle` 由调用方转成小写；空关键字直接放行，留空就是「不过滤」。
fn track_matches(track: &TrackMeta, needle: &str) -> bool {
    needle.is_empty()
        || track.title.to_lowercase().contains(needle)
        || track.artist.to_lowercase().contains(needle)
        || track.album.to_lowercase().contains(needle)
}

/// 待探测的一条曲目：路径，以及分轨的时间窗（整轨为 `None`）。
type ProbeEntry = (PathBuf, Option<TimeWindow>);

/// 曲目的去重键：路径 + 时间窗。分轨曲目共用同一个文件，只比路径会把整张碟当重复。
fn track_key(track: &TrackMeta) -> String {
    match track.window {
        Some(window) => format!(
            "{}\u{1}{}\u{1}{}",
            path_key(&track.path),
            window.start.as_millis(),
            window.end.as_millis()
        ),
        None => path_key(&track.path),
    }
}

// —— 播放列表持久化 ——
//
// 只存「列表名 + 文件路径」：标题、时长、封面都能从文件重新读出来，存下来反而
// 会在换机器 / 改标签之后变成假信息。格式是带转义的制表符文本，够简单、出问题
// 时还能用文本编辑器看一眼。

/// 磁盘上的一条曲目。分轨曲目要连时间窗一起存，否则重启之后整张碟会变回一条。
#[derive(Debug, PartialEq)]
struct SavedTrack {
    path: PathBuf,
    window: Option<TimeWindow>,
}

/// 磁盘上的一份播放列表。
struct SavedLibrary {
    active: usize,
    playlists: Vec<(String, Vec<SavedTrack>)>,
}

/// 配置文件位置。单元测试里返回 `None`：测试不该读写用户真实的配置目录。
fn library_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(config_dir()?.join("ngy-audio-player").join("library.txt"))
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
}

/// 制表符是字段分隔符；名字和路径里可能真有换行，转义掉才不会被解析成两行。
fn escape_field(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn serialize_library(playlists: &[Playlist], active: usize) -> String {
    let mut text = String::from("# 声波播放列表（自动生成，手动改需要保持这个格式）\nversion 2\n");
    text.push_str(&format!("active {active}\n"));
    for playlist in playlists {
        text.push_str("list\t");
        text.push_str(&escape_field(&playlist.name));
        text.push('\n');
        for track in &playlist.tracks {
            text.push_str("track\t");
            text.push_str(&escape_field(&track.path.to_string_lossy()));
            // 整轨曲目只写路径（和 version 1 一样），分轨才追加起止毫秒。
            if let Some(window) = track.window {
                text.push('\t');
                text.push_str(&window.start.as_millis().to_string());
                text.push('\t');
                text.push_str(&window.end.as_millis().to_string());
            }
            text.push('\n');
        }
    }
    text
}

fn parse_library(text: &str) -> Option<SavedLibrary> {
    let mut active = 0usize;
    let mut playlists: Vec<(String, Vec<SavedTrack>)> = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') || line.starts_with("version ") {
            continue;
        }
        if let Some(value) = line.strip_prefix("active ") {
            active = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("list\t") {
            playlists.push((unescape_field(value), Vec::new()));
        } else if let Some(value) = line.strip_prefix("track\t") {
            if value.is_empty() {
                continue;
            }
            let mut fields = value.split('\t');
            let path = PathBuf::from(unescape_field(fields.next().unwrap_or_default()));
            let start = fields.next().and_then(|text| text.parse::<u64>().ok());
            let end = fields.next().and_then(|text| text.parse::<u64>().ok());
            let window = match (start, end) {
                (Some(start), Some(end)) if end > start => Some(TimeWindow {
                    start: Duration::from_millis(start),
                    end: Duration::from_millis(end),
                }),
                _ => None,
            };
            if let Some((_, tracks)) = playlists.last_mut() {
                tracks.push(SavedTrack { path, window });
            }
        }
    }

    if playlists.is_empty() {
        return None;
    }
    let active = active.min(playlists.len() - 1);
    Some(SavedLibrary { active, playlists })
}

fn read_library(path: &Path) -> Option<SavedLibrary> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_library(&text)
}

fn write_library(path: &Path, playlists: &[Playlist], active: usize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 列表就几 KB，直接写比「临时文件 + rename」更省事；崩溃最多丢掉最后一次改动。
    std::fs::write(path, serialize_library(playlists, active))
}

/// 目标是否落在「有曲目、能 seek」的状态里。
fn snapshot_can_seek(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot.duration > 0.0
            && matches!(
                snapshot.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Ended
            )
    })
}

/// 按 0..1 的比例算 seek 目标（秒）。
fn seek_target(ratio: f32, duration: f64) -> f64 {
    seek_target_seconds(ratio as f64 * duration, duration)
}

/// 把秒数夹进文件真正能落到的范围：末尾 50ms 留给解码器的末帧保护。
fn seek_target_seconds(target: f64, duration: f64) -> f64 {
    if !target.is_finite() {
        return 0.0;
    }
    let upper = (duration - SEEK_END_MARGIN).max(0.0);
    target.clamp(0.0, upper)
}

/// 从 SliderValue 里取单值。
fn value_value(value: &SliderValue) -> Option<f32> {
    match value {
        SliderValue::Single(value) => Some(*value),
        SliderValue::Range(start, _) => Some(*start),
    }
}

/// 把已经打开的原生选择框提到前台。
#[cfg(windows)]
fn focus_dialog(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 标题是以 NUL 结尾的 UTF-16 字符串；句柄只传给 user32 做窗口激活。
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
fn focus_dialog(_title: &str) {}

fn scan_folder(folder: &std::path::Path) -> Vec<PathBuf> {
    // 只扫两层：音乐库通常就是「专辑/曲目」两层，再深多半是在扫游戏资源目录。
    const MAX_FILES: usize = 2000;
    let mut found = Vec::new();
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 2 {
                    stack.push((path, depth + 1));
                }
            } else if format::is_audio_path(&path) {
                found.push(path);
                if found.len() >= MAX_FILES {
                    return found;
                }
            }
        }
    }
    found
}

fn format_time(seconds: f64) -> String {
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

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 颜色先抄成值：下面要建很多闭包，借 `cx` 会冲突。
        let theme = cx.theme();
        let background = theme.background;
        let muted = theme.muted;
        let muted_foreground = theme.muted_foreground;
        let foreground = theme.foreground;
        let border = theme.border;
        let primary = theme.primary;
        let danger = theme.danger;

        let snapshot = self.snapshot.clone();
        let state = snapshot
            .as_ref()
            .map(|snapshot| snapshot.state)
            .unwrap_or(PlaybackState::Idle);
        let position = snapshot
            .as_ref()
            .map(|snapshot| snapshot.position)
            .unwrap_or(0.0);
        let duration = snapshot
            .as_ref()
            .map(|snapshot| snapshot.duration)
            .unwrap_or(0.0);
        let volume = snapshot
            .as_ref()
            .map(|snapshot| snapshot.volume)
            .unwrap_or(0.8);

        let current_track = self.playing.clone();
        let title = current_track
            .as_ref()
            .map(|track| track.title.clone())
            .unwrap_or_else(|| "还没有正在播放的曲目".into());
        let artist = current_track
            .as_ref()
            .map(|track| {
                if track.artist.is_empty() {
                    // 没有艺人标签是常态（纯音乐、试听文件），别在这儿塞引导语，
                    // 那会让人误以为正在播的曲目信息丢了。
                    if track.album.is_empty() {
                        "未知艺人".to_string()
                    } else {
                        track.album.clone()
                    }
                } else {
                    track.artist.clone()
                }
            })
            .unwrap_or_else(|| "把音频文件拖进来，或者点右上角打开".into());
        let spec_line = current_track
            .as_ref()
            .map(|track| track.spec_line())
            .unwrap_or_else(|| {
                "支持 MP3 / FLAC / WAV / M4A / OGG / Opus / WMA / APE / DSD 等".into()
            });
        let album = current_track
            .as_ref()
            .map(|track| track.album.clone())
            .unwrap_or_default();

        let backend = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.backend)
            .map(|backend| backend.label().to_string());
        let backend_note = match (&backend, self.has_ffmpeg) {
            (Some(name), _) => format!("解码：{name}"),
            (None, true) => "解码：待命".into(),
            (None, false) => "解码：内置（未装 ffmpeg，部分格式不可用）".into(),
        };

        let playing = matches!(state, PlaybackState::Playing);
        let play_icon = if playing {
            IconName::Pause
        } else {
            IconName::Play
        };
        // 没有装进解码器的曲目时进度条置灰：拖它只会得到一个没有意义的位置。
        let can_seek = snapshot_can_seek(snapshot.as_ref());

        let notice = self
            .notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(text, _)| text.clone());

        let error = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.error.clone())
            .or_else(|| self.device_error.clone());

        let mut root = v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "space" => this.toggle(cx),
                    "left" => this.seek_by(-5.0, cx),
                    "right" => this.seek_by(5.0, cx),
                    "up" => {
                        if let Some(player) = &this.player {
                            let volume = (player.snapshot().volume + 0.05).min(1.0);
                            player.set_volume(volume);
                            this.volume
                                .update(cx, |state, cx| state.set_value(volume, window, cx));
                        }
                    }
                    "down" => {
                        if let Some(player) = &this.player {
                            let volume = (player.snapshot().volume - 0.05).max(0.0);
                            player.set_volume(volume);
                            this.volume
                                .update(cx, |state, cx| state.set_value(volume, window, cx));
                        }
                    }
                    "n" => this.step(1, window, cx),
                    "p" => this.step(-1, window, cx),
                    "delete" | "backspace" => this.remove_current(cx),
                    _ => {}
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.add_paths(paths.paths().to_vec(), true, window, cx);
            }));

        // —— 顶部 ——
        // 自绘标题栏：Windows / Linux 上系统标题栏是透明的，必须自己给窗口按钮，
        // 否则用户只能靠 Alt+F4 关窗。macOS 保留系统红绿灯。
        const SHOW_WINDOW_CONTROLS: bool = !cfg!(target_os = "macos");
        let danger_color = danger;
        let control = |id: &'static str, icon: IconName, danger: bool| {
            let mut button = Button::new(id).ghost().small().icon(icon);
            if danger {
                button = button.text_color(danger_color);
            }
            button
        };

        root = root.child(
            h_flex()
                .h(px(56.0))
                .flex_none()
                .px_4()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(border)
                .child(
                    // 这一块是窗口拖动区：只包标题，不包按钮（否则点按钮会变成拖窗）。
                    h_flex()
                        .id("title-area")
                        .flex_1()
                        .h_full()
                        .items_center()
                        .gap_3()
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                        .child(img("app-icon.png").size(px(28.0)).flex_shrink_0())
                        .child(
                            v_flex()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("声波"),
                                )
                                .child(div().text_xs().text_color(muted_foreground).child(
                                    format!("{} 首 · {}", self.active_tracks().len(), backend_note),
                                )),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("open-files")
                                .outline()
                                .small()
                                .icon(IconName::FilePlus)
                                .label("文件")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.open_files(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("open-folder")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("文件夹")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.open_folder(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("theme")
                                .ghost()
                                .small()
                                .icon(if self.dark {
                                    IconName::Sun
                                } else {
                                    IconName::Moon
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_theme(window, cx)
                                    }),
                                ),
                        )
                        .when(SHOW_WINDOW_CONTROLS, |this| {
                            this.child(div().w(px(1.0)).h(px(20.0)).mx_1().bg(border))
                                .child(
                                    control("win-min", IconName::Minus, false)
                                        .on_click(|_, window, _| window.minimize_window()),
                                )
                                .child(
                                    control("win-max", IconName::Maximize2, false)
                                        .on_click(|_, window, _| window.zoom_window()),
                                )
                                .child(
                                    control("win-close", IconName::X, true)
                                        .on_click(|_, window, _| window.remove_window()),
                                )
                        }),
                ),
        );

        // —— 错误 / 提示条 ——
        if let Some(error) = error {
            root = root.child(
                h_flex()
                    .px_6()
                    .py_2()
                    .gap_2()
                    .items_start()
                    .bg(danger.opacity(0.08))
                    .border_b_1()
                    .border_color(danger.opacity(0.25))
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(14.0))
                            .text_color(danger),
                    )
                    .child(div().text_xs().text_color(danger).child(error)),
            );
        } else if let Some(notice) = notice {
            root = root.child(
                h_flex()
                    .px_6()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .bg(primary.opacity(0.07))
                    .child(Icon::new(IconName::Info).size(px(14.0)).text_color(primary))
                    .child(div().text_xs().text_color(primary).child(notice)),
            );
        }

        // —— 主体：列表 + 正在播放 ——
        // 播放列表标签栏：点标签切换；铅笔改名；× 删掉整个列表（先确认，最后一个不给删）。
        let mut playlist_bar = h_flex()
            .flex_none()
            .px_2()
            .py_2()
            .gap_1()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(border);

        // 标签上的小图标：点击区比图标大一圈，顺手拦住冒泡，别触发切列表。
        let tab_action = |id: String, icon: IconName, danger: bool| {
            div()
                .id(id)
                .test_support()
                .flex()
                .items_center()
                .justify_center()
                // 图标本身只有 12px，把点击区撑到 16px 才好点。
                .size(px(16.0))
                .rounded_full()
                .cursor_pointer()
                .opacity(0.45)
                .hover(|this| this.opacity(1.0))
                .when(danger, |this| this.text_color(danger_color))
                .child(Icon::new(icon).size(px(12.0)))
        };

        for (index, playlist) in self.playlists.iter().enumerate() {
            let selected = index == self.active;
            let removable = self.playlists.len() > 1;
            let editable = playlist.id;
            playlist_bar = playlist_bar.child(
                h_flex()
                    .id(format!("playlist-tab:{index}"))
                    .test_support()
                    .h(px(26.0))
                    .px_2()
                    .gap_1()
                    .rounded_md()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .when(selected, |this| {
                        this.bg(primary.opacity(0.14)).text_color(primary)
                    })
                    .when(!selected, |this| this.text_color(muted_foreground))
                    .hover(|this| this.bg(muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_playlist(index, cx)))
                    .child(
                        div()
                            .id(format!("playlist-name:{index}"))
                            .test_support()
                            .max_w(px(110.0))
                            .truncate()
                            .child(playlist.name.clone()),
                    )
                    .child(
                        tab_action(
                            format!("playlist-rename:{index}"),
                            IconName::PencilLine,
                            false,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_name_dialog(NameTarget::Rename(editable), window, cx);
                            },
                        )),
                    )
                    .when(removable, |this| {
                        this.child(
                            tab_action(format!("playlist-remove:{index}"), IconName::X, true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // 不拦下来的话，点 × 会先把这个列表切到前台再弹确认。
                                    cx.stop_propagation();
                                    this.confirm_remove_playlist(index, window, cx);
                                })),
                        )
                    }),
            );
        }

        playlist_bar = playlist_bar.child(
            div()
                .id("new-playlist")
                .test_support()
                .child(
                    Button::new("new-playlist-button")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .tooltip("新建播放列表"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_name_dialog(NameTarget::New, window, cx)
                })),
        );

        playlist_bar = playlist_bar.child(
            div()
                .id("search-tracks")
                .test_support()
                .child(
                    Button::new("search-tracks-button")
                        .ghost()
                        .small()
                        .icon(IconName::Search)
                        .tooltip("搜索当前列表"),
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_search_dialog(window, cx))),
        );

        // 过滤生效时留一个能一键清掉的标记：列表里少了几首，看起来会像丢了数据。
        if !self.query.is_empty() {
            playlist_bar = playlist_bar.child(
                h_flex()
                    .id("search-filter")
                    .test_support()
                    .h(px(26.0))
                    .px_2()
                    .gap_1()
                    .rounded_full()
                    .items_center()
                    .text_xs()
                    .bg(primary.opacity(0.14))
                    .text_color(primary)
                    .child(
                        div()
                            .max_w(px(160.0))
                            .truncate()
                            .child(format!("搜索：{}", self.query)),
                    )
                    .child(
                        div()
                            .id("search-clear")
                            .test_support()
                            .size(px(16.0))
                            .rounded_full()
                            .cursor_pointer()
                            .child(Icon::new(IconName::X).size(px(12.0)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.query.clear();
                                this.sync_queue();
                                cx.notify();
                            })),
                    ),
            );
        }

        let tracks = self.active_tracks();
        let current_index = self.current_index();

        let mut list = v_flex()
            .id("playlist")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_1();

        if tracks.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("列表是空的。\n\n拖文件进来，或者点右上角「文件」/「文件夹」。"),
            );
        } else if self.view.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("没有匹配的曲目。换个关键字，或者点上面的搜索标记清掉过滤。"),
            );
        }

        // 行号跟着可见顺序走，`index` 仍是 `tracks` 里的真实下标——
        // 增删、重排、拖拽都用它，点击播放用行号。
        for (position, index) in self.view.clone().into_iter().enumerate() {
            let Some(track) = tracks.get(index) else {
                continue;
            };
            let selected = current_index == Some(index);
            let is_current = selected && playing;
            let duration_text = track
                .duration
                .map(|duration| format_time(duration.as_secs_f64()))
                .unwrap_or_else(|| "—".into());
            let subtitle = if track.artist.is_empty() {
                track.format.clone()
            } else {
                format!("{} · {}", track.artist, track.format)
            };
            // group 名带行号：同名的话，鼠标划过一行会让每一行的按钮都亮起来。
            let row_group = format!("track-row:{index}");
            // Button 是 RenderOnce，挂不上 test_support；包一层也能顺手给它一个可点击的 id。
            let action = |id: &'static str, icon: IconName, tooltip: &'static str| {
                div()
                    .id(format!("{id}:{index}"))
                    .test_support()
                    .cursor_pointer()
                    .child(
                        Button::new((id, index))
                            .ghost()
                            .small()
                            .compact()
                            .icon(icon)
                            .tooltip(tooltip),
                    )
            };
            list = list.child(
                h_flex()
                    .id(format!("track:{index}"))
                    .test_support()
                    .group(row_group.clone())
                    .flex_none()
                    .h(px(52.0))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(primary.opacity(0.10))
                            .border_l_2()
                            .border_color(primary)
                    })
                    .hover(|this| this.bg(muted))
                    // 整行可拖：按住任何位置拖动都能换顺序，松手时由落点行接手。
                    .on_drag(
                        TrackDrag {
                            from: index,
                            title: track.title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .drag_over::<TrackDrag>(move |style, _, _, _| {
                        style
                            .bg(primary.opacity(0.10))
                            .border_1()
                            .border_color(primary)
                    })
                    .on_drop(cx.listener(move |this, drag: &TrackDrag, _, cx| {
                        this.move_track_to(drag.from, index, cx);
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_position(position, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(18.0))
                            .flex()
                            .justify_center()
                            .text_xs()
                            .text_color(if selected { primary } else { muted_foreground })
                            .child(if is_current {
                                Icon::new(IconName::AudioLines)
                                    .size(px(14.0))
                                    .text_color(primary)
                                    .into_any_element()
                            } else if selected {
                                Icon::new(IconName::Pause)
                                    .size(px(14.0))
                                    .text_color(primary)
                                    .into_any_element()
                            } else {
                                div().child(format!("{}", position + 1)).into_any_element()
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .text_color(if selected { primary } else { foreground })
                                    .child(track.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .child(duration_text),
                    )
                    .child(
                        // 拖拽手柄只是个提示：真正可拖的是整行。
                        div()
                            .flex_none()
                            .opacity(0.35)
                            .group_hover(row_group.clone(), |this| this.opacity(0.8))
                            .child(
                                Icon::new(IconName::GripVertical)
                                    .size(px(14.0))
                                    .text_color(muted_foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_0p5()
                            .items_center()
                            // 平时淡一点，鼠标落在这一行才完全显出来：
                            // 一直亮着的小图标会把标题挤得没有呼吸空间。
                            .opacity(0.35)
                            .group_hover(row_group, |this| this.opacity(1.0))
                            .child(action("track-up", IconName::ChevronUp, "上移").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    // 行本身也在监听点击，不拦住就会顺带切歌。
                                    cx.stop_propagation();
                                    this.move_track(index, -1, cx);
                                }),
                            ))
                            .child(
                                action("track-down", IconName::ChevronDown, "下移").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.move_track(index, 1, cx);
                                    }),
                                ),
                            )
                            .child(
                                action("track-remove", IconName::Trash, "从列表移除").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.remove_track(index, cx);
                                    }),
                                ),
                            ),
                    ),
            );
        }

        // 正在播放面板
        let cover_element: AnyElement = match &self.cover {
            Some(image) => img(image.clone())
                .size(px(300.0))
                .rounded_xl()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(px(300.0))
                .rounded_xl()
                .bg(muted)
                .border_1()
                .border_color(border)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Music)
                        .size(px(88.0))
                        .text_color(muted_foreground.opacity(0.55)),
                )
                .into_any_element(),
        };

        let spectrum_element = h_flex()
            .h(px(SPECTRUM_HEIGHT))
            .w_full()
            .max_w(px(420.0))
            .items_end()
            .gap(px(2.0))
            .children(self.levels.iter().map(|level| {
                // 静音时留一条 2px 的基线：看起来像「待机」，而不是界面画错了。
                let height = (level * SPECTRUM_HEIGHT).max(2.0);
                div()
                    .flex_1()
                    .h(px(height))
                    .rounded(px(2.0))
                    .bg(primary.opacity(0.30 + 0.70 * level))
            }));

        let now_playing = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap_5()
            .px_8()
            .child(cover_element)
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .max_w(px(520.0))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_center()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted_foreground)
                            .text_center()
                            .child(artist),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .text_center()
                            .child(if album.is_empty() {
                                spec_line
                            } else {
                                format!("{album} · {spec_line}")
                            }),
                    ),
            )
            .child(spectrum_element);

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(border)
                    .bg(muted.opacity(0.35))
                    .child(playlist_bar)
                    .child(list),
            )
            .child(now_playing);

        root = root.child(body);

        // —— 底部传输控制 ——
        // 随机是独立开关：开着的时候用主色，否则关掉了也看不出来。
        let shuffle_button = {
            let button = Button::new("shuffle")
                .ghost()
                .small()
                .icon(IconName::Shuffle)
                .tooltip(if self.shuffle {
                    "随机播放：开"
                } else {
                    "随机播放：关"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.shuffle = !this.shuffle;
                    if let Some(player) = &this.player {
                        player.set_mode(this.mode());
                    }
                    cx.notify();
                }));
            if self.shuffle {
                button.primary()
            } else {
                button
            }
        };
        let controls = h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_full()
            .bg(muted.opacity(0.55))
            .items_center()
            .child(
                Button::new("prev")
                    .ghost()
                    .icon(IconName::SkipBack)
                    .on_click(cx.listener(|this, _, window, cx| this.step(-1, window, cx))),
            )
            .child(
                // Button 是 RenderOnce，挂不上 test_support，包一层给它一个可观测的身份。
                div().id("play-slot").test_support().child(
                    Button::new("play")
                        .primary()
                        .rounded_full()
                        .w(px(44.0))
                        .h(px(44.0))
                        .icon(play_icon)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
                ),
            )
            .child(
                Button::new("next")
                    .ghost()
                    .icon(IconName::SkipForward)
                    .on_click(cx.listener(|this, _, window, cx| this.step(1, window, cx))),
            )
            .child(shuffle_button)
            .child(
                // 排序是循环切换：默认（拖出来的顺序）→ 标题 → 艺人 → 时长。
                div()
                    .id("sort")
                    .test_support()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .hover(|this| this.bg(muted))
                    .child(self.sort.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sort = this.sort.next();
                        this.sync_queue();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("repeat")
                    .ghost()
                    .small()
                    .icon(self.repeat.icon())
                    .tooltip(self.repeat.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repeat = this.repeat.next();
                        if let Some(player) = &this.player {
                            player.set_mode(this.mode());
                        }
                        cx.notify();
                    })),
            );

        let footer =
            v_flex()
                .flex_none()
                .px_6()
                .pt_3()
                .pb_4()
                .gap_2()
                .border_t_1()
                .border_color(border)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .w(px(44.0))
                                .text_xs()
                                .text_color(muted_foreground)
                                .child(format_time(position)),
                        )
                        .child(
                            div().flex_1().child(
                                Slider::new(&self.progress)
                                    .horizontal()
                                    // 没在播东西时滑块是灰的：点它不该产生任何位置变化。
                                    .disabled(!can_seek),
                            ),
                        )
                        .child(
                            div()
                                .w(px(44.0))
                                .text_xs()
                                .text_color(muted_foreground)
                                .child(format_time(duration)),
                        ),
                )
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(h_flex().w(px(220.0)).gap_2().items_center().child(
                            div().text_xs().text_color(muted_foreground).child(
                                if tracks.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        "{}/{}",
                                        current_index.map(|index| index + 1).unwrap_or(0),
                                        tracks.len()
                                    )
                                },
                            ),
                        ))
                        .child(controls)
                        .child(
                            h_flex()
                                .w(px(220.0))
                                .gap_2()
                                .items_center()
                                .justify_end()
                                .child(
                                    Icon::new(if volume <= 0.001 {
                                        IconName::VolumeX
                                    } else {
                                        IconName::Volume2
                                    })
                                    .size(px(16.0))
                                    .text_color(muted_foreground),
                                )
                                .child(
                                    div()
                                        .w(px(110.0))
                                        .child(Slider::new(&self.volume).horizontal()),
                                ),
                        ),
                );

        root.child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui_kit::*` 把 gpui 的 `test` 属性带进了作用域，它会盖掉 std 的 `#[test]`，
    // 结果是 `#[gpui_kit::test]` 展开出来的 `#[test]` 又指回 gpui 的那个 —— 递归到底。
    // 这一行把 std 的那个拿回来。
    use core::prelude::v1::test;
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::TestAppContext;
    use gpui_kit::component::dialog::{Cancel, Confirm};
    use gpui_kit::test::TestWindowExt as _;

    /// 窗口的根必须自己实现 Render，`Entity<T>` 不算；所以套一层壳。
    struct Harness {
        view: Entity<PlayerView>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.view.clone()
        }
    }

    fn samples() -> Vec<PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        ["tone.flac", "tone.mp3", "tone.opus"]
            .iter()
            .map(|name| dir.join(name))
            .filter(|path| path.is_file())
            .collect()
    }

    #[test]
    fn repeat_modes_use_distinct_icons() {
        let icons = [
            RepeatMode::Off.icon(),
            RepeatMode::All.icon(),
            RepeatMode::One.icon(),
        ];
        for (index, icon) in icons.iter().enumerate() {
            assert!(
                !icons[index + 1..].contains(icon),
                "三种循环模式的图标必须能区分"
            );
        }
    }

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

        cx.update(|app| view.update(app, |view, cx| view.move_track(0, 1, cx)));
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

    #[gpui_kit::test]
    fn playlists_are_restored_from_disk(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let dir =
            std::env::temp_dir().join(format!("ngy-audio-player-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("library.txt");

        // 造一份「上次退出时」的列表，然后从一个全新的视图里恢复。
        let playlists = vec![
            Playlist {
                id: 1,
                name: "通勤".into(),
                tracks: vec![Arc::new(TrackMeta::placeholder(Path::new("a.mp3")))],
            },
            Playlist {
                id: 2,
                name: "夜里".into(),
                tracks: vec![
                    Arc::new(TrackMeta::placeholder(Path::new("b.flac"))),
                    Arc::new(TrackMeta::placeholder(Path::new("c.wav"))),
                ],
            },
        ];
        write_library(&path, &playlists, 1).expect("应当能写进临时目录");

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
    fn row_buttons_move_and_remove_tracks(cx: &mut TestAppContext) {
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

        // 第三行点「上移」：顺序换了，正在播的曲目不该被换掉。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("track-up:2", cx);
        })
        .unwrap();
        let after = cx.update(|app| names(app));
        assert_eq!(
            after,
            vec![before[0].clone(), before[2].clone(), before[1].clone()],
            "上移应当只交换相邻两行"
        );
        assert_eq!(
            cx.update(|app| view.read(app).current_index()),
            Some(0),
            "点上移不该顺带切歌"
        );

        // 再点第三行的删除：只少这一行。
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("track-remove:2", cx);
        })
        .unwrap();
        assert_eq!(
            cx.update(|app| names(app)),
            vec![before[0].clone(), before[2].clone()]
        );
    }

    /// 手搓一条带标签的曲目：搜索只看元数据，不依赖 testdata 与解码。
    fn tagged(path: &str, title: &str, artist: &str, album: &str) -> Arc<TrackMeta> {
        let mut meta = TrackMeta::placeholder(Path::new(path));
        meta.title = title.into();
        meta.artist = artist.into();
        meta.album = album.into();
        Arc::new(meta)
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

    /// 带 Root 的窗口：对话框挂在 Root 上，只有这种窗口里才能点开确认框。
    fn root_window(
        cx: &mut TestAppContext,
        slot: Rc<RefCell<Option<Entity<PlayerView>>>>,
    ) -> gpui_kit::WindowHandle<Root> {
        cx.add_window(move |window, cx| {
            let view = cx.new(|cx| PlayerView::new(window, cx));
            *slot.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        })
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

    #[test]
    fn library_text_round_trips_names_and_paths() {
        let playlist = |name: &str, paths: &[&str]| Playlist {
            id: 1,
            name: name.into(),
            tracks: paths
                .iter()
                .map(|path| Arc::new(TrackMeta::placeholder(Path::new(path))))
                .collect(),
        };

        // 名字里带制表符 / 换行 / 反斜杠，路径里带 Windows 盘符：都是真实会遇到的。
        let playlists = vec![
            playlist("夜里的\t歌\n单", &[r"C:\音乐\一 - 二.flac", "/tmp/三.mp3"]),
            playlist("空列表", &[]),
        ];
        let text = serialize_library(&playlists, 1);
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
        let playlists = vec![Playlist {
            id: 1,
            name: "整轨".into(),
            tracks: vec![Arc::new(meta)],
        }];

        let saved = parse_library(&serialize_library(&playlists, 0)).expect("应当能解回来");
        assert_eq!(saved.playlists[0].1[0].window, Some(window));
    }

    #[test]
    fn library_active_index_is_clamped() {
        let playlist = |name: &str| Playlist {
            id: 1,
            name: name.into(),
            tracks: Vec::new(),
        };
        let text = serialize_library(&[playlist("A"), playlist("B")], 9);
        assert_eq!(
            parse_library(&text).unwrap().active,
            1,
            "越界的 active 要收回来"
        );
        assert!(parse_library("").is_none(), "空文件不算一份列表");
        assert!(parse_library("version 1\nactive 0\n").is_none());
    }

    #[test]
    fn seek_target_stays_off_the_last_frames() {
        assert!((seek_target(1.0, 10.0) - 9.95).abs() < 1e-9);
        assert_eq!(seek_target(0.0, 10.0), 0.0);
        assert_eq!(seek_target(0.5, 10.0), 5.0);
        assert_eq!(seek_target(1.0, 0.0), 0.0);
        assert_eq!(seek_target_seconds(-3.0, 10.0), 0.0);
    }
}
