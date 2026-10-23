//! 统一解码接口。
//!
//! 播放器面对的是「一条 2 声道交错 f32 流」，至于这条流是 Symphonia 解出来的，
//! 还是系统 ffmpeg 转出来的，上层不需要知道。

pub mod ffmpeg;
pub mod symphonia_backend;

use std::path::Path;
use std::time::Duration;

/// 这条流是谁解出来的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// 内置纯 Rust 解码器。
    Symphonia,
    /// 系统 ffmpeg 子进程。
    Ffmpeg,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Symphonia => "Symphonia",
            Backend::Ffmpeg => "FFmpeg",
        }
    }
}

/// 解码流参数。`channels` 恒为 2 —— 所有后端都在内部下混/上混完成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSpec {
    pub rate: u32,
    pub channels: u16,
}

/// 一条可 seek 的解码流。块大小由实现决定（几百到几千帧），调用方不该假设。
pub trait Decoder: Send {
    fn backend(&self) -> Backend;
    fn spec(&self) -> StreamSpec;
    /// 下一块交错 f32 样本；`None` 表示流结束。
    fn next_chunk(&mut self) -> anyhow::Result<Option<Vec<f32>>>;
    /// 跳到 `to`。允许落在目标之前（解码器只能 seek 到包边界）。
    fn seek(&mut self, to: Duration) -> anyhow::Result<()>;
}

/// 打开一条解码流。
///
/// 策略是**先试内置解码器**：它对主流格式更快、能精确 seek、不用起子进程。
/// 只有它打不开（不支持的编码 / 容器，例如 Opus、WMA、APE、DSD）才落到 ffmpeg。
/// 按扩展名预先分类是不行的：`.ogg` 里既可能是 Vorbis，也可能是 Opus。
pub fn open(path: &Path, target_rate: u32) -> anyhow::Result<Box<dyn Decoder>> {
    let builtin = symphonia_backend::open(path);
    match builtin {
        Ok(decoder) => Ok(decoder),
        Err(builtin_error) => match ffmpeg::open(path, target_rate) {
            Ok(decoder) => Ok(decoder),
            Err(ffmpeg_error) => Err(anyhow::anyhow!(
                "这个文件两种解码路径都失败了\n内置解码器：{builtin_error:#}\nffmpeg：{ffmpeg_error:#}"
            )),
        },
    }
}

/// 把任意声道数的交错样本转成 2 声道。
///
/// 单声道复制成左右两路；多声道取前两路（5.1/7.1 的 ch0/ch1 就是 L/R 本身，
/// 比自己拍脑袋加权下混更不容易把对白声道混进来）。
pub fn to_stereo(samples: Vec<f32>, channels: usize, out: &mut Vec<f32>) {
    out.clear();
    match channels {
        0 => {}
        1 => {
            out.reserve(samples.len() * 2);
            for sample in samples {
                out.push(sample);
                out.push(sample);
            }
        }
        2 => out.extend_from_slice(&samples),
        _ => {
            out.reserve(samples.len() / channels * 2);
            for frame in samples.chunks_exact(channels) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated() {
        let mut out = Vec::new();
        to_stereo(vec![0.5, -0.5], 1, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn surround_keeps_front_pair() {
        let mut out = Vec::new();
        // 两帧 6 声道：L R C LFE Ls Rs
        to_stereo(vec![1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12.], 6, &mut out);
        assert_eq!(out, vec![1., 2., 7., 8.]);
    }
}
