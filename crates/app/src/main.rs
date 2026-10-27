// 发布版不弹控制台黑框：双击打开时少一次窗口创建，也避免视觉闪烁。
// debug 版保留控制台，方便看解码失败的原因。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    install_panic_hook();
    ngy_audio_player::app::run()
}

/// 发布版没有标准输出，崩溃时至少要留下线索。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[panic] {info}");
        previous(info);
    }));
}
