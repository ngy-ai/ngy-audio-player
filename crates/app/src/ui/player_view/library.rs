//! 播放列表持久化：只存「列表名 + 文件路径」。
//!
//! 标题、时长、封面都能从文件重新读出来，存下来反而会在换机器 / 改标签之后变成
//! 假信息。格式是带转义的制表符文本，够简单、出问题时还能用文本编辑器看一眼。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use ngy_audio_engine::meta::{TimeWindow, TrackMeta};

use super::import::ProbeEntry;
use super::queue::Playlist;
use super::view::PlayerView;

/// 磁盘上的一条曲目。分轨曲目要连时间窗一起存，否则重启之后整张碟会变回一条。
#[derive(Debug, PartialEq)]
pub(super) struct SavedTrack {
    pub(super) path: PathBuf,
    pub(super) window: Option<TimeWindow>,
}

/// 磁盘上的一份播放列表。
pub(super) struct SavedLibrary {
    pub(super) active: usize,
    pub(super) playlists: Vec<(String, Vec<SavedTrack>)>,
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

pub(super) fn serialize_library(playlists: &[&Playlist], active: usize) -> String {
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

pub(super) fn parse_library(text: &str) -> Option<SavedLibrary> {
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

pub(super) fn read_library(path: &Path) -> Option<SavedLibrary> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_library(&text)
}

pub(super) fn write_library(
    path: &Path,
    playlists: &[&Playlist],
    active: usize,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 列表就几 KB，直接写比「临时文件 + rename」更省事；崩溃最多丢掉最后一次改动。
    std::fs::write(path, serialize_library(playlists, active))
}

impl PlayerView {
    /// 把播放列表写成文本；失败只记日志，不该影响播放。
    pub(super) fn save_library(&self) {
        let Some(path) = &self.library_path else {
            return;
        };
        // 「外部打开」不落盘，所以写盘用的下标要在一份过滤过的视图里重新算。
        let lists: Vec<&Playlist> = self
            .playlists
            .iter()
            .filter(|list| !list.transient)
            .collect();
        let active = self
            .playlists
            .get(self.active)
            .and_then(|current| lists.iter().position(|list| std::ptr::eq(*list, current)))
            .unwrap_or(0);
        if let Err(error) = write_library(path, &lists, active) {
            eprintln!("[library] 保存播放列表失败: {error}");
        }
    }

    /// 读回上次的播放列表。标签在后台补，先把路径铺上去。
    pub(super) fn restore_library(&mut self, cx: &mut Context<Self>) {
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
                transient: false,
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
