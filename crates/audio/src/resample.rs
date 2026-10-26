//! 采样率转换：设备采样率与素材采样率不一致时（44.1k 素材 → 48k 设备）用它。
//!
//! 用 rubato 的 FFT 重采样（同步、固定比例）。相比线性插值，它把 20kHz 以上的
//! 镜像能量真正滤掉了——线性插值在 44.1→48 时会在 24kHz 附近糊成一片嘶声。

use anyhow::Context;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as _};

/// 流式重采样器：吃任意大小的交错块，吐交错块。
pub struct StreamResampler {
    inner: Fft<f32>,
    channels: usize,
    /// 还没凑够一整个 FFT 块的交错输入。
    pending: Vec<f32>,
    out_scratch: Vec<f32>,
}

impl StreamResampler {
    pub fn new(from: u32, to: u32, channels: usize) -> anyhow::Result<Self> {
        // chunk_size 1024 帧：延迟约 21ms@48k，对播放器是合适的折中。
        let inner = Fft::<f32>::new(from as usize, to as usize, 1024, channels, FixedSync::Input)
            .context("初始化重采样器失败")?;
        Ok(Self {
            inner,
            channels,
            pending: Vec::with_capacity(8192 * channels),
            out_scratch: Vec::new(),
        })
    }

    /// 喂一段交错样本，把能产出的结果追加到 `out`。
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        self.pending.extend_from_slice(input);
        self.pump(out);
    }

    /// 把 `pending` 里够一整块的输入全部推给 rubato。
    ///
    /// 关键细节：**每一轮都要重新问 `input_frames_next()` / `output_frames_next()`**。
    /// `FixedSync::Input` 下 rubato 每处理完一块都会重算下一次的输出帧数（960、1280
    /// 这样来回变），沿用循环外取到的旧值会被它判成「输出缓冲不够」——那块输入就白丢了。
    /// 这个坑的表现很隐蔽：音频听起来只是「短了一截」，进度条与判尾时刻跟着一起错。
    fn pump(&mut self, out: &mut Vec<f32>) {
        let channels = self.channels;
        let mut consumed = 0;

        loop {
            let need = self.inner.input_frames_next();
            let capacity = self.inner.output_frames_next();
            let required = need * channels;
            if self.pending.len() - consumed < required {
                break;
            }

            let input_adapter = match InterleavedSlice::new(
                &self.pending[consumed..consumed + required],
                channels,
                need,
            ) {
                Ok(adapter) => adapter,
                Err(_) => break,
            };

            self.out_scratch.resize(capacity * channels, 0.0);
            let mut output_adapter =
                match InterleavedSlice::new_mut(&mut self.out_scratch, channels, capacity) {
                    Ok(adapter) => adapter,
                    Err(_) => break,
                };

            // 出错就停下，不推进游标：下轮再试，绝不静默丢样本。
            let Ok((_, produced)) =
                self.inner
                    .process_into_buffer(&input_adapter, &mut output_adapter, None)
            else {
                break;
            };

            out.extend_from_slice(&self.out_scratch[..produced * channels]);
            consumed += required;
        }

        if consumed > 0 {
            self.pending.drain(..consumed);
        }
    }

    /// 曲尾收尾：把不足一块的残留补零送出去，再额外喂两块静音把 FFT 内部
    /// 延迟里还压着的采样推出来。否则最后一小段会被直接削掉（听起来是尾音断了）。
    ///
    /// 代价是多出十几毫秒静音，对播放器无所谓。
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if !self.pending.is_empty() {
            let target = self.inner.input_frames_next() * self.channels;
            if self.pending.len() < target {
                self.pending.resize(target, 0.0);
            }
            self.pump(out);
        }

        for _ in 0..2 {
            let zeros = vec![0.0; self.inner.input_frames_next() * self.channels];
            self.pending.extend_from_slice(&zeros);
            self.pump(out);
        }
    }

    /// seek 之后必须重置，否则 FFT 的历史会跨过不连续点。
    pub fn reset(&mut self) {
        self.inner.reset();
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn upsamples_to_target_length() {
        let source_rate = 44_100u32;
        let target_rate = 48_000u32;
        let frames = source_rate as usize; // 1 秒
        let mut input = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let t = index as f32 / source_rate as f32;
            let sample = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            input.push(sample);
            input.push(sample);
        }

        let mut resampler = StreamResampler::new(source_rate, target_rate, 2).unwrap();
        let mut out = Vec::new();
        // 故意用比一帧对齐的块（4608 帧）喂：引擎里就是这么喂的，
        // 而且非 2 的幂的块长最容易暴露「输出缓冲尺寸没重算」的问题。
        for chunk in input.chunks(4608 * 2) {
            resampler.process(chunk, &mut out);
        }
        resampler.finish(&mut out);

        let produced_frames = out.len() / 2;
        let expected = target_rate as usize;
        // 曲尾额外补了两块静音，所以只会多不会少；少一点点也不该出现。
        assert!(
            produced_frames + target_rate as usize / 100 >= expected,
            "输出 {produced_frames} 帧，比期望的 {expected} 帧少了（有样本被丢掉）"
        );
        assert!(
            produced_frames <= expected + 4096,
            "输出 {produced_frames} 帧，比期望的 {expected} 帧多太多"
        );
        // 振幅不该被重采样改变太多。
        let level = rms(&out);
        assert!((0.30..0.40).contains(&level), "RMS 异常：{level}");
    }

    #[test]
    fn silence_stays_silent() {
        let mut resampler = StreamResampler::new(44_100, 48_000, 2).unwrap();
        let mut out = Vec::new();
        resampler.process(&vec![0.0; 4096 * 2], &mut out);
        assert!(out.iter().all(|sample| *sample == 0.0));
    }
}
