//! 队列模型：播放列表、排序 / 循环方式，以及「哪些曲目算同一首」。

use std::path::Path;
use std::sync::Arc;

use gpui_kit::assets::IconName;

use ngy_audio_engine::meta::TrackMeta;
use ngy_audio_engine::{QueueMode, Repeat};

use super::view::PlayerView;

/// 一个播放列表。
///
/// `id` 只做内部标识：异步探测标签回来时要靠它找回「当初加入的那个列表」，
/// 用户中途切换 / 删除列表时不会把标签写到别的列表里。
pub(super) struct Playlist {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) tracks: Vec<Arc<TrackMeta>>,
    /// 「外部打开」：由双击 / 命令行交进来，进程退出即丢，不写进库文件。
    pub(super) transient: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub(super) fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "顺序播放",
            RepeatMode::All => "列表循环",
            RepeatMode::One => "单曲循环",
        }
    }

    /// 三种模式必须用不同图标，否则用户看不出当前处于哪种循环状态。
    pub(super) fn icon(self) -> IconName {
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
pub(super) enum SortKey {
    Manual,
    Title,
    Artist,
    Duration,
}

impl SortKey {
    pub(super) fn next(self) -> Self {
        match self {
            SortKey::Manual => SortKey::Title,
            SortKey::Title => SortKey::Artist,
            SortKey::Artist => SortKey::Duration,
            SortKey::Duration => SortKey::Manual,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            SortKey::Manual => "排序：默认",
            SortKey::Title => "排序：标题",
            SortKey::Artist => "排序：艺人",
            SortKey::Duration => "排序：时长",
        }
    }
}

/// 判断「同一个文件」用的键：先规范化掉 `.` / `..` 与大小写盘符的差异，
/// Windows 上再统一小写。规范化失败（文件刚被删）就退回原始路径。
pub(super) fn path_key(path: &Path) -> String {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = full.to_string_lossy().into_owned();
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

/// 曲目的去重键：路径 + 时间窗。分轨曲目共用同一个文件，只比路径会把整张碟当重复。
pub(super) fn track_key(track: &TrackMeta) -> String {
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

/// 列表内搜索的匹配规则：标题 / 艺人 / 专辑任一命中即可（不区分大小写）。
///
/// `needle` 由调用方转成小写；空关键字直接放行，留空就是「不过滤」。
pub(super) fn track_matches(track: &TrackMeta, needle: &str) -> bool {
    needle.is_empty()
        || track.title.to_lowercase().contains(needle)
        || track.artist.to_lowercase().contains(needle)
        || track.album.to_lowercase().contains(needle)
}

impl PlayerView {
    /// 重算可见行。列表增删、标签回填、排序方式或搜索关键字变化之后都要调一次。
    ///
    /// `view` 存的是 `tracks` 的下标：**行号（可见位置）和真实下标从此是两回事**，
    /// 渲染和播放用行号，增删改重排用真实下标。搜索是在这里做的一次过滤，
    /// 所以过滤后的列表同时也是播放队列。
    pub(super) fn refresh_view(&mut self) {
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
    pub(super) fn visible_tracks(&self) -> Vec<Arc<TrackMeta>> {
        let tracks = self.active_tracks();
        self.view
            .iter()
            .filter_map(|index| tracks.get(*index))
            .cloned()
            .collect()
    }

    /// 列表增删 / 重排 / 排序方式变化之后同步给引擎。
    ///
    /// 顺手重算可见行：所有改动列表的地方本来就要调这个函数，把刷新挂在这里
    /// 就不会漏掉某一处（漏一处的表现是「列表里少了一行」这种难查的问题）。
    pub(super) fn sync_queue(&mut self) {
        self.refresh_view();
        if let Some(player) = &self.player {
            player.set_queue(self.visible_tracks());
        }
    }

    pub(super) fn mode(&self) -> QueueMode {
        QueueMode {
            repeat: self.repeat.to_engine(),
            shuffle: self.shuffle,
        }
    }
}
