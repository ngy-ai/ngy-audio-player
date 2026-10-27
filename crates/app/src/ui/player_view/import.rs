//! 把路径收进列表：去重、占位、后台补标签、cue 分轨展开。
//!
//! 文件一进列表就先落盘、先出占位行，标签晚几百毫秒出现没关系。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::*;

use ngy_audio_engine::cue;
use ngy_audio_engine::engine::same_track;
use ngy_audio_engine::format;
use ngy_audio_engine::meta::{TimeWindow, TrackMeta};

use super::queue::{path_key, track_key};
use super::view::PlayerView;

/// 待探测的一条曲目：路径，以及分轨的时间窗（整轨为 `None`）。
pub(super) type ProbeEntry = (PathBuf, Option<TimeWindow>);

impl PlayerView {
    /// `play_first` 为真时，无论当前是否在播，都立刻切到这批文件的第一首。
    pub(super) fn add_paths(
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
    pub(super) fn probe_tracks(
        &mut self,
        list_id: u64,
        entries: Vec<ProbeEntry>,
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
}
