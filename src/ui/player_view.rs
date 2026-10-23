//! 主视图：播放列表、正在播放、传输控制。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use ngy_audio_engine::format;
use ngy_audio_engine::meta::{CoverArt, TrackMeta};
use ngy_audio_engine::{PlaybackState, Player, Snapshot};

use super::cover;
use super::spectrum::Spectrum;

const SIDEBAR_WIDTH: f32 = 320.0;
const SPECTRUM_BANDS: usize = 44;
const SPECTRUM_HEIGHT: f32 = 96.0;
/// 定时刷新间隔。频谱要跳得顺，但也没必要每帧算 FFT。
const TICK: Duration = Duration::from_millis(80);

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
}

pub struct PlayerView {
    player: Option<Player>,
    device_error: Option<String>,

    tracks: Vec<Arc<TrackMeta>>,
    current: Option<usize>,
    snapshot: Option<Snapshot>,

    progress: Entity<SliderState>,
    volume: Entity<SliderState>,
    /// 用户正在拖进度条：此时不要用播放位置覆盖滑块。
    scrubbing: bool,

    spectrum: Spectrum,
    levels: Vec<f32>,

    cover: Option<Arc<RenderImage>>,
    cover_for: Option<PathBuf>,

    repeat: RepeatMode,
    last_finished: u64,
    has_ffmpeg: bool,
    dark: bool,
    /// 一次性提示（例如「这个格式需要 ffmpeg」），过几秒自动消失。
    notice: Option<(String, Instant)>,
    /// 文件选择框已经打开：原生对话框是阻塞调用，连点会叠出多个窗口。
    file_dialog_open: bool,
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
        let progress = cx.new(|_| SliderState::new().min(0.0).max(1.0).default_value(0.0));
        let volume = cx.new(|_| SliderState::new().min(0.0).max(1.0).default_value(0.8));

        // 进度条：拖动过程中只更新数字，松手才真的 seek —— 边拖边 seek 会让
        // 解码线程反复重启，反而卡顿。
        cx.subscribe_in(&progress, window, |this, _, event: &SliderEvent, window, cx| {
            let (value, released) = match event {
                SliderEvent::Change(value) => (value_value(value), false),
                SliderEvent::Release(value) => (value_value(value), true),
            };
            let Some(ratio) = value else { return };
            let duration = this
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.duration)
                .unwrap_or(0.0);
            if released {
                this.scrubbing = false;
                if let Some(player) = &this.player {
                    player.seek((ratio as f64 * duration).max(0.0));
                }
            } else {
                this.scrubbing = true;
                if let Some(snapshot) = &mut this.snapshot {
                    snapshot.position = ratio as f64 * duration;
                }
            }
            let _ = window;
            cx.notify();
        })
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

        let this = Self {
            player,
            device_error,
            tracks: Vec::new(),
            current: None,
            snapshot: None,
            progress,
            volume,
            scrubbing: false,
            spectrum: Spectrum::new(SPECTRUM_BANDS),
            levels: vec![0.0; SPECTRUM_BANDS],
            cover: None,
            cover_for: None,
            repeat: RepeatMode::All,
            last_finished: 0,
            has_ffmpeg: ngy_audio_engine::has_ffmpeg(),
            dark,
            notice: None,
            file_dialog_open: false,
            focus,
        };

        // 从命令行拿到的文件（`ngy-audio-player a.mp3 b.flac`）。
        let args: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .collect();
        let mut this = this;
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
        let snapshot = player.snapshot();

        // 频谱：有声音才算，静音时让柱子自己落下去。
        let samples = player.spectrum();
        if samples.is_empty() {
            self.levels = self.spectrum.silence().to_vec();
        } else {
            self.levels = self.spectrum.update(&samples).to_vec();
        }

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

        // 自动下一首。
        if snapshot.finished_serial != self.last_finished {
            self.last_finished = snapshot.finished_serial;
            self.on_finished(window, cx);
        }

        self.snapshot = Some(snapshot);
        cx.notify();
    }

    fn on_finished(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.repeat {
            RepeatMode::One => {
                if let Some(index) = self.current {
                    self.play_index(index, window, cx);
                }
            }
            RepeatMode::All => {
                if !self.tracks.is_empty() {
                    let next = self.current.map(|index| (index + 1) % self.tracks.len()).unwrap_or(0);
                    self.play_index(next, window, cx);
                }
            }
            RepeatMode::Off => {
                let Some(current) = self.current else { return };
                if current + 1 < self.tracks.len() {
                    self.play_index(current + 1, window, cx);
                }
            }
        }
    }

    fn play_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(track) = self.tracks.get(index).cloned() else {
            return;
        };
        self.current = Some(index);
        self.scrubbing = false;
        if let Some(player) = &self.player {
            player.load(track.clone());
            player.set_paused(false);
        }
        self.load_cover(&track, cx);
        self.notice = None;
        self.progress.update(cx, |state, cx| {
            state.set_value(0.0, window, cx);
        });
        cx.notify();
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tracks.is_empty() {
            return;
        }
        let current = self.current.unwrap_or(0) as isize;
        let count = self.tracks.len() as isize;
        let next = (current + delta).rem_euclid(count);
        self.play_index(next as usize, window, cx);
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = &self.player {
            player.toggle_pause();
            cx.notify();
        }
    }

    fn seek_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else { return };
        if let Some(player) = &self.player {
            let target = (snapshot.position + delta).clamp(0.0, snapshot.duration.max(0.0));
            player.seek(target);
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
        let task = cx.background_executor().spawn(async move {
            cover::decode(&bytes, 640)
        });
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
        let mut paths: Vec<PathBuf> = paths
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

        let start = self.tracks.len();
        // 先放占位：文件名立刻出现在列表里，标签慢慢补。
        self.tracks
            .extend(paths.iter().map(|path| Arc::new(TrackMeta::placeholder(path))));

        let count = paths.len();
        let task = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .map(|path| Arc::new(TrackMeta::probe(&path)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let metas = task.await;
            let _ = this.update(cx, |this, cx| {
                for (offset, meta) in metas.into_iter().enumerate() {
                    if let Some(slot) = this.tracks.get_mut(start + offset) {
                        *slot = meta;
                    }
                }
                // 标签回来之后重新看一次封面：`play_index` 当时拿到的还是占位曲目，
                // 它没有封面，`cover_for` 却已经把路径记下了——不把这条记录清掉，
                // `load_cover` 会以为「这首看过、就是没封面」，封面永远不上屏。
                if let Some(index) = this.current
                    && let Some(track) = this.tracks.get(index).cloned()
                {
                    // 引擎里存的可能还是占位曲目（没有时长），把真实标签补进去，
                    // 否则进度条算不出比例、总时长显示 0:00。
                    if let Some(player) = &this.player {
                        player.refresh_metadata(track.clone());
                    }
                    if this.cover.is_none() && this.cover_for.as_ref() == Some(&track.path) {
                        this.cover_for = None;
                    }
                    this.load_cover(&track, cx);
                }
                cx.notify();
            });
        })
        .detach();

        if play_first || self.current.is_none() {
            self.play_index(start, window, cx);
        } else {
            self.notice = Some((format!("已加入 {count} 首"), Instant::now()));
            cx.notify();
        }
    }

    fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_dialog_open {
            return;
        }
        self.file_dialog_open = true;
        let extensions = format::dialog_extensions();
        let task = cx.background_executor().spawn(async move {
            rfd::FileDialog::new()
                .add_filter("音频文件", extensions)
                .add_filter("所有文件", &["*"])
                .set_title("选择音频文件")
                .pick_files()
                .unwrap_or_default()
        });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.file_dialog_open = false;
                if files.is_empty() {
                    return;
                }
                this.add_paths(files, true, window, cx);
            });
        })
        .detach();
    }

    fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            let Some(folder) = rfd::FileDialog::new().set_title("选择音乐文件夹").pick_folder()
            else {
                return Vec::new();
            };
            scan_folder(&folder)
        });
        cx.spawn_in(window, async move |this, cx| {
            let files = task.await;
            if files.is_empty() {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| this.add_paths(files, false, window, cx));
        })
        .detach();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        Theme::change(
            if self.dark { ThemeMode::Dark } else { ThemeMode::Light },
            Some(window),
            cx,
        );
        cx.notify();
    }

    fn remove_current(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.current else { return };
        self.tracks.remove(index);
        if self.tracks.is_empty() {
            self.current = None;
            self.cover = None;
            self.cover_for = None;
            if let Some(player) = &self.player {
                player.stop();
            }
        } else {
            self.current = Some(index.min(self.tracks.len() - 1));
        }
        cx.notify();
    }
}

/// 从 SliderValue 里取单值。
fn value_value(value: &SliderValue) -> Option<f32> {
    match value {
        SliderValue::Single(value) => Some(*value),
        SliderValue::Range(start, _) => Some(*start),
    }
}

fn scan_folder(folder: &std::path::Path) -> Vec<PathBuf> {
    // 只扫两层：音乐库通常就是「专辑/曲目」两层，再深多半是在扫游戏资源目录。
    const MAX_FILES: usize = 2000;
    let mut found = Vec::new();
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
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
        let state = snapshot.as_ref().map(|snapshot| snapshot.state).unwrap_or(PlaybackState::Idle);
        let position = snapshot.as_ref().map(|snapshot| snapshot.position).unwrap_or(0.0);
        let duration = snapshot.as_ref().map(|snapshot| snapshot.duration).unwrap_or(0.0);
        let volume = snapshot.as_ref().map(|snapshot| snapshot.volume).unwrap_or(0.8);

        let current_track = self
            .current
            .and_then(|index| self.tracks.get(index))
            .cloned();
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
            .unwrap_or_else(|| "支持 MP3 / FLAC / WAV / M4A / OGG / Opus / WMA / APE / DSD 等".into());
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
        let play_icon = if playing { IconName::Pause } else { IconName::Play };

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
                            this.volume.update(cx, |state, cx| state.set_value(volume, window, cx));
                        }
                    }
                    "down" => {
                        if let Some(player) = &this.player {
                            let volume = (player.snapshot().volume - 0.05).max(0.0);
                            player.set_volume(volume);
                            this.volume.update(cx, |state, cx| state.set_value(volume, window, cx));
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
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("声波"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_foreground)
                                        .child(format!(
                                            "{} 首 · {}",
                                            self.tracks.len(),
                                            backend_note
                                        )),
                                ),
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
                                .on_click(cx.listener(|this, _, window, cx| this.open_files(window, cx))),
                        )
                        .child(
                            Button::new("open-folder")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("文件夹")
                                .on_click(cx.listener(|this, _, window, cx| this.open_folder(window, cx))),
                        )
                        .child(
                            Button::new("theme")
                                .ghost()
                                .small()
                                .icon(if self.dark { IconName::Sun } else { IconName::Moon })
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_theme(window, cx))),
                        )
                        .when(SHOW_WINDOW_CONTROLS, |this| {
                            this.child(
                                div()
                                    .w(px(1.0))
                                    .h(px(20.0))
                                    .mx_1()
                                    .bg(border),
                            )
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
                    .child(Icon::new(IconName::TriangleAlert).size(px(14.0)).text_color(danger))
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
        let mut list = v_flex()
            .id("playlist")
            .size_full()
            .overflow_y_scroll()
            .p_2()
            .gap_1();

        if self.tracks.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("列表是空的。\n\n拖文件进来，或者点右上角「文件」/「文件夹」。"),
            );
        }

        for (index, track) in self.tracks.iter().enumerate() {
            let selected = self.current == Some(index);
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
            list = list.child(
                h_flex()
                    .id(format!("track:{index}"))
                    .test_support()
                    .flex_none()
                    .h(px(52.0))
                    .px_3()
                    .gap_3()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(primary.opacity(0.10))
                            .border_l_2()
                            .border_color(primary)
                    })
                    .hover(|this| this.bg(muted))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_index(index, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(20.0))
                            .flex()
                            .justify_center()
                            .text_xs()
                            .text_color(if selected { primary } else { muted_foreground })
                            .child(if is_current {
                                Icon::new(IconName::AudioLines).size(px(14.0)).text_color(primary).into_any_element()
                            } else if selected {
                                Icon::new(IconName::Pause).size(px(14.0)).text_color(primary).into_any_element()
                            } else {
                                div().child(format!("{}", index + 1)).into_any_element()
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
                    .child(div().text_sm().text_color(muted_foreground).text_center().child(artist))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .text_center()
                            .child(if album.is_empty() { spec_line } else { format!("{album} · {spec_line}") }),
                    ),
            )
            .child(spectrum_element);

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(border)
                    .bg(muted.opacity(0.35))
                    .child(list),
            )
            .child(now_playing);

        root = root.child(body);

        // —— 底部传输控制 ——
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
            .child(
                Button::new("repeat")
                    .ghost()
                    .small()
                    .icon(match self.repeat {
                        RepeatMode::One => IconName::Repeat1,
                        _ => IconName::Repeat,
                    })
                    .tooltip(self.repeat.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repeat = this.repeat.next();
                        cx.notify();
                    })),
            );

        let footer = v_flex()
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
                    .child(div().flex_1().child(Slider::new(&self.progress).horizontal()))
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
                    .child(
                        h_flex().w(px(220.0)).gap_2().items_center().child(
                            div()
                                .text_xs()
                                .text_color(muted_foreground)
                                .child(if self.tracks.is_empty() {
                                    String::new()
                                } else {
                                    format!("{}/{}", self.current.map(|index| index + 1).unwrap_or(0), self.tracks.len())
                                }),
                        ),
                    )
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
                            .child(div().w(px(110.0)).child(Slider::new(&self.volume).horizontal())),
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
                view.update(cx, |view, cx| view.add_paths(paths.clone(), false, window, cx));
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
            (view.tracks.len(), view.current)
        });
        assert_eq!(state.0, 3, "三首样本都该进列表");
        assert_eq!(state.1, Some(0), "加载完成后应当从第一首开始");

        cx.update_window(handle.into(), |_, window, cx| {
            // 点第二行。命中测试要真的落在那一行上，否则这里会直接报错。
            window.click("track:1", cx);
            window.render_frame(cx);
        })
        .unwrap();

        let current = cx.update(|app| view.read(app).current);
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
                view.update(cx, |view, cx| view.add_paths(first_two.clone(), false, window, cx));
                *slot.borrow_mut() = Some(view.clone());
                Harness { view }
            })
        };

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();

        let view = slot.borrow().clone().expect("视图没有建出来");
        assert_eq!(cx.update(|app| view.read(app).current), Some(0));

        let third = paths[2].clone();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.add_paths(vec![third], true, window, cx));
        })
        .unwrap();

        assert_eq!(
            cx.update(|app| view.read(app).current),
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
        assert_eq!(cx.update(|app| view.read(app).tracks.len()), 0);
    }
}
