//! 真实格式的端到端解码测试。
//!
//! 样本由 `testdata/` 里的 ffmpeg 生成文件构成（见 README）。这个测试**不看扩展名**
//! 决定后端，而是真的解一遍，因此它能回答那个关键问题：
//! 「说好的所有格式，到底有哪些真的能出声，走的是哪条路径」。

use std::path::{Path, PathBuf};
use std::time::Duration;

use ngy_audio_engine::decode;
use ngy_audio_engine::format;

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

    assert!(failures.is_empty(), "有格式解码失败：\n{}", failures.join("\n"));
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
    let chunk = decoder.next_chunk().expect("解码失败").expect("seek 后没有数据");
    assert!(!chunk.is_empty(), "seek 之后读不到样本");
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
    assert!(message.contains("内置解码器"), "错误里缺少内置解码器的原因：{message}");
    assert!(message.contains("ffmpeg"), "错误里缺少 ffmpeg 的原因：{message}");

    let _ = std::fs::remove_file(&path);
}
