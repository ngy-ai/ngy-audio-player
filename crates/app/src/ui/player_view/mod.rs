//! 主视图：播放列表、正在播放、传输控制。
//!
//! 拆成「状态 → 动作 → 渲染」三层：
//!
//! - `view` 持有 [`PlayerView`] 的字段与定时刷新；
//! - `queue` / `playback` / `import` / `tracks` / `playlists` / `dialogs` /
//!   `library` / `file_picker` / `file_assoc` / `menu` 各自管一类动作；
//! - `render` 只负责把状态画出来，不修改状态；
//! - `seek` / `settings` 是没有视图依赖的纯工具。

mod dialogs;
mod file_assoc;
mod file_picker;
mod import;
mod library;
mod menu;
mod playback;
mod playlists;
mod queue;
mod render;
mod seek;
mod settings;
mod tracks;
mod view;

#[cfg(test)]
mod tests;

pub use view::PlayerView;
