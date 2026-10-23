//! 音频输出：cpal 在三个平台上的统一封装。
//!
//! 线程模型：
//!
//! ```text
//! 解码线程 ──push_slice──▶ 环形缓冲 ──pop_slice──▶ 设备回调（无锁、不分配）
//! ```
//!
//! 设备回调里只做三件事：读队列、乘音量、写设备格式。任何会阻塞的操作
//! （加锁、分配、IO）都不允许出现在这里 —— 一次 10ms 的卡顿就是一声爆音。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait as _};
use cpal::{FromSample, SizedSample};
use ringbuf::HeapRb;
use ringbuf::traits::Consumer as _;

use crate::state::Shared;

/// 环形缓冲能存多少秒。2 秒足够吸收解码抖动，又不会让 seek 之后拖太久。
const BUFFER_SECONDS: f32 = 2.0;

pub type OutputProducer = ringbuf::Prod<Arc<HeapRb<f32>>>;
type OutputConsumer = ringbuf::Cons<Arc<HeapRb<f32>>>;

/// 实际打开的设备参数，UI 上会显示。
#[derive(Clone, Debug)]
pub struct OutputConfig {
    pub device: String,
    pub rate: u32,
    pub channels: u16,
    pub sample_format: String,
}

/// 打开默认输出设备，返回常驻的 stream、参数、以及给解码线程的写入端。
pub fn open(shared: &Arc<Shared>) -> anyhow::Result<(cpal::Stream, OutputConfig, OutputProducer)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("系统里没有可用的音频输出设备")?;
    let device_name = device.to_string();

    let supported = device
        .default_output_config()
        .with_context(|| format!("设备 {device_name} 没有可用的输出配置"))?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let rate = config.sample_rate;
    let channels = config.channels;
    let capacity = (rate as f32 * channels as f32 * BUFFER_SECONDS) as usize;
    // 刻意用非 caching 的 `Prod`/`Cons`：`Caching` 版本会把写指针缓存在本地，
    // 音频回调要等到缓存刷出才看得见新数据 —— 那是几十毫秒的额外延迟。
    let ring = Arc::new(HeapRb::<f32>::new(capacity));
    let producer = ringbuf::Prod::new(ring.clone());
    let consumer = ringbuf::Cons::new(ring);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, shared.clone(), consumer),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, shared.clone(), consumer),
        cpal::SampleFormat::U16 => build::<u16>(&device, config, shared.clone(), consumer),
        cpal::SampleFormat::I32 => build::<i32>(&device, config, shared.clone(), consumer),
        cpal::SampleFormat::I8 => build::<i8>(&device, config, shared.clone(), consumer),
        cpal::SampleFormat::U8 => build::<u8>(&device, config, shared.clone(), consumer),
        other => bail!("这个设备的采样格式还不支持：{other:?}"),
    }?;

    stream.play().context("启动音频输出流失败")?;

    Ok((
        stream,
        OutputConfig {
            device: device_name,
            rate,
            channels,
            sample_format: format!("{sample_format:?}"),
        },
        producer,
    ))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    mut consumer: OutputConsumer,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let out_channels = config.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    let mut sink = [0f32; 2048];
    let mut played_frames: u64 = 0;
    let mut local_epoch = shared.seek_epoch.load(Ordering::Relaxed);

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / out_channels.max(1);
                if frames == 0 {
                    return;
                }

                // seek / 切歌：把还没播的旧数据倒掉，否则会先听到一小段上一处的音频。
                if shared.flush.swap(false, Ordering::SeqCst) {
                    let mut drained = 0;
                    loop {
                        let read = consumer.pop_slice(&mut sink);
                        if read == 0 {
                            break;
                        }
                        drained += read;
                        // 上限只是防呆阀：必须大于环形缓冲总容量（2 秒 × 声道数），
                        // 否则一次清不干净，剩下的旧音频还会被放出来。
                        if drained > sink.len() * 256 {
                            break;
                        }
                    }
                    played_frames = 0;
                    local_epoch = shared.seek_epoch.load(Ordering::Relaxed);
                }

                if !shared.should_render() {
                    data.fill(T::EQUILIBRIUM);
                    return;
                }

                // 位置时钟跟着回调走：每写出去多少帧，时间就前进多少。
                let epoch = shared.seek_epoch.load(Ordering::Relaxed);
                if epoch != local_epoch {
                    local_epoch = epoch;
                    played_frames = 0;
                }

                scratch.resize(frames * 2, 0.0);
                let got = consumer.pop_slice(&mut scratch[..frames * 2]);
                if got < frames * 2 {
                    // 欠载：缺的部分补静音。此时时间照走，进度条不会卡住。
                    scratch[got..frames * 2].fill(0.0);
                }

                let gain = shared.gain();
                for frame in 0..frames {
                    let left = scratch[frame * 2] * gain;
                    let right = scratch[frame * 2 + 1] * gain;
                    let base = frame * out_channels;
                    if out_channels == 1 {
                        data[base] = T::from_sample((left + right) * 0.5);
                    } else {
                        data[base] = T::from_sample(left);
                        data[base + 1] = T::from_sample(right);
                        for channel in 2..out_channels {
                            data[base + channel] = T::EQUILIBRIUM;
                        }
                    }
                }

                // 频谱图只要最近的一小段。
                if let Ok(mut viz) = shared.viz.try_lock() {
                    let take = frames.min(1024);
                    viz.clear();
                    viz.reserve(take);
                    for frame in 0..take {
                        viz.push((scratch[frame * 2] + scratch[frame * 2 + 1]) * 0.5);
                    }
                }

                played_frames += frames as u64;
                let baseline = shared.baseline_ms.load(Ordering::Relaxed);
                let rate = shared.rate.load(Ordering::Relaxed).max(1) as u64;
                let position_ms = baseline + played_frames * 1000 / rate;
                shared.position_ms.store(position_ms, Ordering::Relaxed);
            },
            move |error| {
                // 设备掉了（拔耳机、切默认设备）。不 panic，让 UI 通过状态提示用户。
                eprintln!("[output] 音频流错误: {error}");
            },
            None,
        )
        .context("创建音频输出流失败")?;

    Ok(stream)
}

/// 列出可用输出设备（UI 里做设备选择时用）。
pub fn devices() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|iter| iter.map(|device| device.to_string()).collect())
        .unwrap_or_default()
}
