//! 统一解码接口。
//!
//! 播放器面对的是「一条 2 声道交错 f32 流」，至于这条流是 Symphonia 解出来的，
//! 还是系统 ffmpeg 转出来的，上层不需要知道。

pub mod ffmpeg;
pub mod symphonia_backend;

use std::path::Path;
use std::time::Duration;

use crate::meta::TimeWindow;

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
    ///
    /// 分轨曲目的 `to` 是**窗口内**的时间，绝对位置由解码器自己换算。
    fn seek(&mut self, to: Duration) -> anyhow::Result<()>;
}

/// 打开一条解码流。
///
/// 策略是**先试内置解码器**：它对主流格式更快、能精确 seek、不用起子进程。
/// 只有它打不开（不支持的编码 / 容器，例如 Opus、WMA、APE、DSD）才落到 ffmpeg。
/// 按扩展名预先分类是不行的：`.ogg` 里既可能是 Vorbis，也可能是 Opus。
pub fn open(path: &Path, target_rate: u32) -> anyhow::Result<Box<dyn Decoder>> {
    open_range(path, target_rate, None)
}

/// 打开一条解码流，并把它限制在整轨的某一段时间里（CUE 分轨）。
///
/// 窗口对外的表现是「一个独立的、从 0 开始的短曲子」：时长是窗口长度，seek 目标
/// 也是窗口内的相对时间。两条后端各自负责换算成文件内的绝对位置。
pub fn open_range(
    path: &Path,
    target_rate: u32,
    window: Option<TimeWindow>,
) -> anyhow::Result<Box<dyn Decoder>> {
    let builtin = symphonia_backend::open(path, window);
    match builtin {
        Ok(decoder) => Ok(decoder),
        Err(builtin_error) => match ffmpeg::open(path, target_rate, window) {
            Ok(decoder) => Ok(decoder),
            Err(ffmpeg_error) => Err(anyhow::anyhow!(
                "这个文件两种解码路径都失败了\n内置解码器：{builtin_error:#}\nffmpeg：{ffmpeg_error:#}"
            )),
        },
    }
}

/// 5.1 / 7.1 下混到立体声用的系数（ITU-R BS.775）：中置与环绕各按 -3dB 摊进左右。
const SURROUND_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// 把任意声道数的交错样本转成 2 声道。
///
/// 单声道复制成左右两路；多声道按 BS.775 下混：中置与环绕按 0.707 加进左右，
/// LFE 丢弃。以前的做法是直接取前两路，结果是 AC-3 / DTS 里的对白（在中置）
/// 完全消失，听起来像「伴奏在响、人声没了」。
///
/// 下混后峰值可能超过 0 dBFS（三个声道满幅时理论上是 2.4），所以过一遍软限幅：
/// 0.9 以下原样，以上平滑压到 1.0，不会像硬削顶那样冒出刺耳的谐波。
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
            for chunk in samples.chunks_exact(channels) {
                let mut left = chunk[0];
                let mut right = chunk.get(1).copied().unwrap_or(chunk[0]);
                match channels {
                    // L R C
                    3 => {
                        left += chunk[2] * SURROUND_GAIN;
                        right += chunk[2] * SURROUND_GAIN;
                    }
                    // L R BL BR
                    4 => {
                        left += chunk[2] * SURROUND_GAIN;
                        right += chunk[3] * SURROUND_GAIN;
                    }
                    // L R C Ls Rs
                    5 => {
                        left += (chunk[2] + chunk[3]) * SURROUND_GAIN;
                        right += (chunk[2] + chunk[4]) * SURROUND_GAIN;
                    }
                    // L R C LFE Ls Rs
                    6 => {
                        left += (chunk[2] + chunk[4]) * SURROUND_GAIN;
                        right += (chunk[2] + chunk[5]) * SURROUND_GAIN;
                    }
                    // L R C LFE Cs Ls Rs：后中置两路各分一半
                    7 => {
                        let rear = chunk[4] * SURROUND_GAIN;
                        left += (chunk[2] + chunk[5]) * SURROUND_GAIN + rear;
                        right += (chunk[2] + chunk[6]) * SURROUND_GAIN + rear;
                    }
                    // L R C LFE Ls Rs Lsd Rsd
                    8 => {
                        left += (chunk[2] + chunk[4] + chunk[6]) * SURROUND_GAIN;
                        right += (chunk[2] + chunk[5] + chunk[7]) * SURROUND_GAIN;
                    }
                    // 不认识的布局：只保留前两路，至少不会把对白声道混进左声道。
                    _ => {}
                }
                out.push(limit(left));
                out.push(limit(right));
            }
        }
    }
}

/// 软限幅：只作用于本来就会被削顶的样本。
fn limit(sample: f32) -> f32 {
    const KNEE: f32 = 0.9;
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }
    let over = (magnitude - KNEE) / (1.0 - KNEE);
    sample.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
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
    fn front_pair_passes_through() {
        let mut out = Vec::new();
        to_stereo(vec![1.0, -1.0], 2, &mut out);
        assert_eq!(out, vec![1.0, -1.0]);
    }

    #[test]
    fn center_is_shared_by_both_channels() {
        let mut out = Vec::new();
        // 一帧 5.1：L R C LFE Ls Rs，只有中置有信号。
        to_stereo(vec![0.0, 0.0, 1.0, 0.0, 0.0, 0.0], 6, &mut out);
        let expected = SURROUND_GAIN;
        assert!((out[0] - expected).abs() < 1e-6, "{:?}", out);
        assert!((out[1] - expected).abs() < 1e-6, "{:?}", out);
    }

    #[test]
    fn surround_channels_join_the_same_side() {
        let mut out = Vec::new();
        to_stereo(vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0], 6, &mut out);
        let expected = SURROUND_GAIN;
        assert!((out[0] - expected).abs() < 1e-6, "{:?}", out);
        assert!((out[1] - expected).abs() < 1e-6, "{:?}", out);
    }

    #[test]
    fn lfe_is_dropped() {
        let mut out = Vec::new();
        to_stereo(vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 6, &mut out);
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn loud_downmix_does_not_exceed_full_scale() {
        let mut out = Vec::new();
        to_stereo(vec![1.0, 1.0, 1.0, 0.0, 1.0, 1.0], 6, &mut out);
        assert!(out.iter().all(|sample| sample.abs() <= 1.0), "{:?}", out);
    }
}
