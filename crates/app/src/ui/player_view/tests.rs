//! 视图行为测试：按被测动作分文件，夹具在 `harness`。
//!
//! 依赖 `testdata/` 的用例在样本缺失时跳过，方便没有样本的环境跑其余部分。

mod harness;
mod library;
mod menu;
mod playback;
mod playlists;
mod queue;
mod rows;
mod seek;
