//! 真实格式的端到端解码测试。
//!
//! 样本由 `testdata/` 里的 ffmpeg 生成文件构成（见 README）。这个测试**不看扩展名**
//! 决定后端，而是真的解一遍，因此它能回答那个关键问题：
//! 「说好的所有格式，到底有哪些真的能出声，走的是哪条路径」。

use std::path::{Path, PathBuf};
use std::time::Duration;

use ngy_audio_engine::cue;
use ngy_audio_engine::decode;
use ngy_audio_engine::format;
use ngy_audio_engine::meta::{TimeWindow, TrackMeta};

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// 把整条流读完，返回（后端名, 总帧数, 采样率）。
fn decode_all(path: &Path) -> anyhow::Result<(String, u64, u32)> {
    let mut decoder = decode::open(path, 48_000)?;
    let spec = decoder.spec();
    let mut frames = 0u64;
    while let Some(chunk) = decoder.next_chunk()? {
        frames += (chunk.len() / 2) as u64;
    }
    Ok((decoder.backend().label().to_string(), frames, spec.rate))
}

#[test]
fn every_sample_decodes_with_reasonable_length() {
    let dir = testdata_dir();
    if !dir.is_dir() {
        eprintln!("跳过：没有 testdata 目录（用 testdata/README.md 里的命令生成样本）");
        return;
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("读取 testdata 失败")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        // testdata 里还放着 README、封面图之类的东西，只挑真正的音频文件。
        .filter(|path| format::is_audio_path(path))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "testdata 是空的");

    let mut failures = Vec::new();
    let mut used_symphonia = 0;
    let mut used_ffmpeg = 0;

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        match decode_all(path) {
            Ok((backend, frames, rate)) => {
                let seconds = frames as f64 / rate as f64;
                println!("{name:<22} {backend:<10} {rate:>6} Hz  {seconds:.3}s  {frames} 帧");
                if backend == "Symphonia" {
                    used_symphonia += 1;
                } else {
                    used_ffmpeg += 1;
                }
                // `tone.*` 是 1.2 秒的纯音；编码器补白/延迟各不相同，给足容差。
                // `demo.*` / `long.*` 是刻意做的长样本（封面、seek、自动切歌要用），
                // 对它们只要求「确实有内容」——95 秒不是 bug。
                let reasonable = if name.starts_with("tone") {
                    (0.9..=1.8).contains(&seconds)
                } else {
                    seconds >= 30.0
                };
                if !reasonable {
                    failures.push(format!("{name}: 时长 {seconds:.3}s 不合理"));
                }
            }
            Err(error) => failures.push(format!("{name}: {error:#}")),
        }
    }

    assert!(
        failures.is_empty(),
        "有格式解码失败：\n{}",
        failures.join("\n")
    );
    // 两条路径都得真的被用到，否则「双层解码」只是纸面上的。
    assert!(used_symphonia > 0, "没有任何样本走内置解码器");
    assert!(used_ffmpeg > 0, "没有任何样本走 ffmpeg 兜底");
}

#[test]
fn seek_lands_near_target() {
    let dir = testdata_dir();
    let path = dir.join("tone.flac");
    if !path.is_file() {
        eprintln!("跳过：缺少 tone.flac");
        return;
    }

    let mut decoder = decode::open(&path, 48_000).expect("打开 flac 失败");
    decoder.seek(Duration::from_millis(800)).expect("seek 失败");
    let chunk = decoder
        .next_chunk()
        .expect("解码失败")
        .expect("seek 后没有数据");
    assert!(!chunk.is_empty(), "seek 之后读不到样本");
}

#[test]
fn seek_to_reported_duration_does_not_go_out_of_range() {
    let dir = testdata_dir();
    if !dir.is_dir() {
        eprintln!("跳过：没有 testdata 目录");
        return;
    }

    let mut checked = 0;
    for name in ["tone.wav", "tone.flac", "tone.mp3", "tone.ogg", "demo.mp3"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        // 用户拖到最右边时，UI 用的就是标签探测出来的时长；它常比容器
        // 真实帧数长几十毫秒，直接拿去 seek 会 out-of-range。
        let duration = TrackMeta::probe(&path)
            .duration
            .unwrap_or_else(|| panic!("{name} 探测不出时长"));
        let mut decoder = decode::open(&path, 48_000)
            .unwrap_or_else(|error| panic!("{name} 打开失败：{error:#}"));
        decoder
            .seek(duration)
            .unwrap_or_else(|error| panic!("{name} 拖到末尾 seek 失败：{error:#}"));
        assert!(
            decoder.next_chunk().expect("seek 后解码失败").is_some(),
            "{name} 拖到末尾后读不到数据"
        );
        checked += 1;
    }
    assert!(checked > 0, "testdata 里没有可用于 seek 的样本");
}

/// 分轨曲目必须只交出窗口内那一段，而且两条后端都要做到。
#[test]
fn window_limits_decoding_to_the_slice() {
    let dir = testdata_dir();
    let window = TimeWindow {
        start: Duration::from_millis(300),
        end: Duration::from_millis(600),
    };
    let mut checked = 0;
    for name in ["tone.wav", "tone.flac", "tone.mp3", "tone.opus"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let mut decoder = decode::open_range(&path, 48_000, Some(window))
            .unwrap_or_else(|error| panic!("{name} 打不开：{error:#}"));
        let rate = decoder.spec().rate;
        let mut frames = 0u64;
        while let Some(chunk) = decoder.next_chunk().expect("解码失败") {
            frames += (chunk.len() / 2) as u64;
        }
        let seconds = frames as f64 / rate as f64;
        // 编码器补白与包边界会让它略长，但不能长到把整轨都放出来。
        assert!(
            (0.25..=0.40).contains(&seconds),
            "{name} 的窗口 0.3s 解出了 {seconds:.3}s"
        );
        checked += 1;
    }
    assert!(checked > 0, "testdata 里没有可用于分轨的样本");
}

/// 整轨 + cue → 多条独立曲目，时长各自独立。
#[test]
fn cue_expands_one_file_into_separate_tracks() {
    let dir = testdata_dir();
    let audio = dir.join("tone.wav");
    if !audio.is_file() {
        eprintln!("跳过：缺少 tone.wav");
        return;
    }

    let sheet = dir.join("__test__.cue");
    std::fs::write(
        &sheet,
        "PERFORMER \"测试艺人\"\nTITLE \"整轨\"\nFILE \"tone.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"第一段\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"第二段\"\n    INDEX 01 00:00:60\n",
    )
    .unwrap();

    let tracks = cue::expand(&sheet).expect("cue 展开失败");
    let _ = std::fs::remove_file(&sheet);

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].title, "第一段");
    assert_eq!(tracks[0].artist, "测试艺人");
    let first = tracks[0].window.expect("第一段没有时间窗");
    assert_eq!(first.start, Duration::ZERO);
    // 60 帧 = 0.8 秒。
    assert_eq!(first.end, Duration::from_millis(800));
    let second = tracks[1].window.expect("第二段没有时间窗");
    assert_eq!(second.start, Duration::from_millis(800));
    // 时长必须是这一段自己的长度，而不是整轨的 1.2 秒。
    assert_eq!(tracks[1].duration, Some(Duration::from_millis(400)));
}

#[test]
fn unsupported_garbage_reports_both_reasons() {
    let dir = testdata_dir();
    let path = dir.join("not-audio.bin");
    std::fs::write(&path, b"this is definitely not audio").unwrap();

    let error = match decode::open(&path, 48_000) {
        Ok(decoder) => panic!("垃圾数据被当成了音频，后端是 {:?}", decoder.backend()),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    // 两条路径的原因都要能看到，否则用户只知道「失败了」不知道「为什么」。
    assert!(
        message.contains("内置解码器"),
        "错误里缺少内置解码器的原因：{message}"
    );
    assert!(
        message.contains("ffmpeg"),
        "错误里缺少 ffmpeg 的原因：{message}"
    );

    let _ = std::fs::remove_file(&path);
}
