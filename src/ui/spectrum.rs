//! 频谱：给界面画一排会跳的柱子。
//!
//! 只做「够看」的精度：1024 点 Hann 窗，按对数频率切成若干段，取每段峰值。
//! 这不是分析工具，所以没有做窗函数归一化和 dB 标定，只把结果压到 0..1 让画面好看。

use std::sync::Arc;

use rustfft::{FftPlanner, num_complex::Complex};

const FFT_SIZE: usize = 1024;
/// 只画到 16kHz：再往上基本都是编码噪声，视觉上没信息量。
const MAX_FREQ: f32 = 16_000.0;
const MIN_FREQ: f32 = 40.0;
/// 假定设备采样率 48k 来算频率轴。它的唯一影响是柱子的疏密分布，
/// 不影响「有没有声音」这个事实，所以不值得为此把设备参数传进来。
const ASSUMED_RATE: f32 = 48_000.0;

pub struct Spectrum {
    fft: Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex<f32>>,
    levels: Vec<f32>,
    /// 每根柱子的频率上界（bin 下标）。
    edges: Vec<usize>,
}

impl Spectrum {
    pub fn new(bands: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Hann 窗：不加减的话，块边界的不连续会在频谱上糊出一层底噪。
        let window = (0..FFT_SIZE)
            .map(|index| {
                let x = std::f32::consts::TAU * index as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();

        // 对数分布：低频给更多柱子，符合听觉。
        let mut edges = Vec::with_capacity(bands);
        let ratio = MAX_FREQ / MIN_FREQ;
        for band in 0..bands {
            let low = MIN_FREQ * ratio.powf(band as f32 / bands as f32);
            let index = (low / ASSUMED_RATE * FFT_SIZE as f32) as usize;
            edges.push(index.clamp(1, FFT_SIZE / 2 - 1));
        }

        Self {
            fft,
            window,
            buffer: vec![Complex::default(); FFT_SIZE],
            levels: vec![0.0; bands],
            edges,
        }
    }

    /// 用最新的一段样本刷新柱子，返回 0..1 的电平。
    pub fn update(&mut self, samples: &[f32]) -> &[f32] {
        self.buffer.fill(Complex::default());
        let take = samples.len().min(FFT_SIZE);
        let offset = samples.len() - take;
        for index in 0..take {
            self.buffer[index] = Complex::new(samples[offset + index] * self.window[index], 0.0);
        }
        self.fft.process(&mut self.buffer);

        let bands = self.levels.len();
        for band in 0..bands {
            let start = self.edges[band];
            let end = self
                .edges
                .get(band + 1)
                .copied()
                .unwrap_or(FFT_SIZE / 2)
                .max(start + 1);
            let mut peak = 0.0f32;
            for bin in start..end.min(FFT_SIZE / 2) {
                peak = peak.max(self.buffer[bin].norm());
            }
            // 归一化 + 开方压一下动态范围，否则弱信号永远贴地。
            let normalized = (peak / (FFT_SIZE as f32 * 0.25)).clamp(0.0, 1.0).sqrt();
            let previous = self.levels[band];
            // 快升慢降：跳上去要跟手，落下来要有余韵。
            self.levels[band] = if normalized > previous {
                normalized
            } else {
                previous * 0.72 + normalized * 0.28
            };
        }
        &self.levels
    }

    pub fn silence(&mut self) -> &[f32] {
        for level in &mut self.levels {
            *level *= 0.6;
        }
        &self.levels
    }
}
