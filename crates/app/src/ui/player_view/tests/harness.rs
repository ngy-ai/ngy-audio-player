//! 测试公用夹具：窗口外壳、样本列表、手工曲目。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::Root;
use gpui_kit::*;

use ngy_audio_engine::meta::TrackMeta;

use super::super::view::PlayerView;

/// 窗口的根必须自己实现 Render，`Entity<T>` 不算；所以套一层壳。
pub(super) struct Harness {
    pub(super) view: Entity<PlayerView>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.view.clone()
    }
}

pub(super) fn samples() -> Vec<PathBuf> {
    // 样本在工作区根，不在 crate 目录里。
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    ["tone.flac", "tone.mp3", "tone.opus"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// 手搓一条带标签的曲目：搜索只看元数据，不依赖 testdata 与解码。
pub(super) fn tagged(path: &str, title: &str, artist: &str, album: &str) -> Arc<TrackMeta> {
    let mut meta = TrackMeta::placeholder(Path::new(path));
    meta.title = title.into();
    meta.artist = artist.into();
    meta.album = album.into();
    Arc::new(meta)
}

/// 带 Root 的窗口：对话框挂在 Root 上，只有这种窗口里才能点开确认框。
pub(super) fn root_window(
    cx: &mut TestAppContext,
    slot: Rc<RefCell<Option<Entity<PlayerView>>>>,
) -> gpui_kit::WindowHandle<Root> {
    cx.add_window(move |window, cx| {
        let view = cx.new(|cx| PlayerView::new(window, cx));
        *slot.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    })
}
