//! 队列模型的小测试。

use super::super::queue::RepeatMode;

#[test]
fn repeat_modes_use_distinct_icons() {
    let icons = [
        RepeatMode::Off.icon(),
        RepeatMode::All.icon(),
        RepeatMode::One.icon(),
    ];
    for (index, icon) in icons.iter().enumerate() {
        assert!(
            !icons[index + 1..].contains(icon),
            "三种循环模式的图标必须能区分"
        );
    }
}
