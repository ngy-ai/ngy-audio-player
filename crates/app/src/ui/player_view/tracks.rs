//! 列表行上的动作：移除、定位文件、拖动换序。

use std::time::Instant;

use gpui_kit::*;

use ngy_audio_engine::engine::same_track;

use crate::shell;

use super::view::PlayerView;

impl PlayerView {
    /// 从当前列表移除一行；移掉的正是正在播的那首就停下，
    /// 免得界面已经没这首歌、引擎还在出声。
    pub(super) fn remove_track(&mut self, index: usize, cx: &mut Context<Self>) {
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

    pub(super) fn remove_current(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.current_index() {
            self.remove_track(index, cx);
        }
    }

    /// 在文件管理器里定位某一首（Windows 上是「打开所在文件夹并选中」）。
    pub(super) fn reveal_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(track) = self.active_tracks().get(index) else {
            return;
        };
        if let Err(error) = shell::reveal(&track.path) {
            self.notice = Some((format!("定位文件失败：{error:#}"), Instant::now()));
            cx.notify();
        }
    }

    /// 把第 `from` 行搬到 `to` 的位置（拖动排序）。
    pub(super) fn move_track_to(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
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
}
