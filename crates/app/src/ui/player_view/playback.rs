//! 播放动作：开始播某一行、上一首 / 下一首、暂停、相对 seek，以及封面跟着换。

use std::time::Instant;

use gpui_kit::*;

use ngy_audio_engine::meta::{CoverArt, TrackMeta};

use crate::ui::cover;

use super::seek::seek_target_seconds;
use super::view::PlayerView;

impl PlayerView {
    /// 播第 `position` 行（可见顺序）。
    pub(super) fn play_position(
        &mut self,
        position: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn play_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(position) = self.view.iter().position(|item| *item == index) {
            self.play_position(position, window, cx);
        }
    }

    pub(super) fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// 播放 / 暂停。
    ///
    /// 还没有当前曲目时（刚打开、或者正在播的那首刚被删掉），引擎里只有队列、
    /// 没有装载任何东西，`toggle_pause` 会静默地什么都不做；这时从当前列表的
    /// 第一首开始，点播放才有反应。
    pub(super) fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.playing.is_none() {
            self.play_position(0, window, cx);
            return;
        }
        if let Some(player) = &self.player {
            player.toggle_pause();
            cx.notify();
        }
    }

    pub(super) fn seek_by(&mut self, delta: f64, cx: &mut Context<Self>) {
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

    pub(super) fn load_cover(&mut self, track: &TrackMeta, cx: &mut Context<Self>) {
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
}
