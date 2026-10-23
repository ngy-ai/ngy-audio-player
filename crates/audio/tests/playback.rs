//! 真实设备上的播放链路测试：打开输出设备 → 加载 → 播放 → 位置推进。
//!
//! 没有音频设备的环境（CI、无声卡的容器）会直接跳过，不报失败。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ngy_audio_engine::{PlaybackState, Player, TrackMeta};

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

#[test]
fn plays_a_tone_and_advances_position() {
    let path = testdata_dir().join("tone.flac");
    if !path.is_file() {
        eprintln!("跳过：缺少 testdata/tone.flac");
        return;
    }

    let player = match Player::new() {
        Ok(player) => player,
        Err(error) => {
            eprintln!("跳过：没有可用输出设备（{error:#}）");
            return;
        }
    };
    println!("设备：{:?}", player.config());

    player.load(Arc::new(TrackMeta::probe(&path)));
    player.set_paused(false);

    // 等第一块数据真的送到回调。解码 + 重采样 + 上缓冲通常几十毫秒。
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_playing = false;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        let snapshot = player.snapshot();
        if snapshot.state == PlaybackState::Playing && snapshot.position > 0.15 {
            saw_playing = true;
            break;
        }
        if snapshot.state == PlaybackState::Failed {
            panic!("播放失败：{:?}", snapshot.error);
        }
    }

    let snapshot = player.snapshot();
    println!(
        "状态={:?} 位置={:.3}s 时长={:.3}s 解码={:?}",
        snapshot.state, snapshot.position, snapshot.duration, snapshot.backend
    );
    assert!(saw_playing, "位置没有推进：{snapshot:?}");

    // 回调应该把最近的样本写给频谱分析。
    let samples = player.spectrum();
    assert!(!samples.is_empty(), "音频回调没有提供可视化样本");
    let energy = samples.iter().map(|s| s.abs()).sum::<f32>() / samples.len() as f32;
    println!("可视样本 {} 个，平均幅度 {energy:.4}", samples.len());
    assert!(energy > 0.001, "可视样本是静音的（平均幅度 {energy}）");

    // 暂停之后位置应当停住。
    player.set_paused(true);
    std::thread::sleep(Duration::from_millis(200));
    let paused = player.snapshot();
    std::thread::sleep(Duration::from_millis(300));
    let still = player.snapshot();
    assert_eq!(paused.state, PlaybackState::Paused);
    assert!(
        (still.position - paused.position).abs() < 0.05,
        "暂停后位置还在走：{} -> {}",
        paused.position,
        still.position
    );

    // seek 应该落到目标附近。
    player.seek(0.9);
    player.set_paused(false);
    std::thread::sleep(Duration::from_millis(300));
    let after_seek = player.snapshot();
    println!("seek 后位置={:.3}s", after_seek.position);
    assert!(
        after_seek.position > 0.85,
        "seek 没有生效，位置只有 {:.3}s",
        after_seek.position
    );

    // 判「播完」必须等到声音真的放完。缓冲判空逻辑写错时（比如清缓冲把新数据
    // 一起倒了、或者重采样丢样本），这里会看到位置刚走到零头就报 Ended。
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut ended = player.snapshot();
    while Instant::now() < deadline && ended.state != PlaybackState::Ended {
        std::thread::sleep(Duration::from_millis(50));
        ended = player.snapshot();
    }
    println!(
        "结束时状态={:?} 位置={:.3}s 时长={:.3}s",
        ended.state, ended.position, ended.duration
    );
    assert_eq!(ended.state, PlaybackState::Ended, "没能正常播完：{ended:?}");
    assert!(
        ended.position + 0.05 >= ended.duration,
        "还没播到结尾就报了播完：位置 {:.3}s / 时长 {:.3}s",
        ended.position,
        ended.duration
    );
}
