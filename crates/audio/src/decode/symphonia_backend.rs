//! 内置解码器：Symphonia（纯 Rust，无外部依赖）。
//!
//! 覆盖 MP1/2/3、AAC、ALAC、FLAC、PCM/WAV、AIFF、CAF、Vorbis（Ogg）、
//! MP4/M4A、Matroska/WebM 容器。
//!
//! 它**不能**解 Opus、WMA、APE、DSD、AMR、AC-3 —— 那些由 `ffmpeg` 后端接手。

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Duration as UnitsDuration, Time};

use super::{Backend, Decoder, StreamSpec, to_stereo};

/// 末尾留的余量。几个解码器在精确末帧 seek 会报 EOF / out-of-range，
/// 而标签探测出的时长又常比容器真实帧数长几毫秒到几十毫秒。
const SEEK_END_MARGIN: Duration = Duration::from_millis(50);

pub struct SymphoniaDecoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// 源文件自己的声道数，用来决定怎么做立体声映射。
    source_channels: usize,
    spec: StreamSpec,
    interleaved: Vec<f32>,
    stereo: Vec<f32>,
    /// 容器真实可 seek 的上界；元数据时长不可信时用它兜底。
    seek_end: Option<Duration>,
    /// 打开时试解出来的第一块，不能丢。
    primer: Option<Vec<f32>>,
}

pub fn open(path: &Path) -> anyhow::Result<Box<dyn Decoder>> {
    let file = File::open(path).with_context(|| format!("打不开文件：{}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }

    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .with_context(|| format!("内置解码器不认识的容器：{}", path.display()))?;

    let track = reader
        .default_track(TrackType::Audio)
        .or_else(|| reader.first_track(TrackType::Audio))
        .context("文件里没有音频轨")?;

    let track_id = track.id;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(params)) => params.clone(),
        _ => bail!("音频轨缺少编码参数"),
    };

    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("内置解码器不支持这个音频编码")?;

    // 以解码器给出的参数为准：HE-AAC / SBR 会让实际采样率是容器里写的两倍。
    let decoded = decoder.codec_params();
    let rate = decoded
        .sample_rate
        .or(params.sample_rate)
        .filter(|rate| *rate > 0)
        .context("无法确定采样率")?;
    let source_channels = decoded
        .channels
        .as_ref()
        .map(|channels| channels.count())
        .or_else(|| params.channels.as_ref().map(|channels| channels.count()))
        .filter(|count| *count > 0)
        .unwrap_or(2);

    // 在把 reader 移进结构体之前先算好，顺便避开借用冲突。
    let seek_end = seekable_end(track);

    let mut decoder = SymphoniaDecoder {
        reader,
        decoder,
        track_id,
        source_channels,
        spec: StreamSpec { rate, channels: 2 },
        interleaved: Vec::new(),
        stereo: Vec::new(),
        seek_end,
        primer: None,
    };

    // 试解一块再算成功。Symphonia 对裸 MPEG 音频（mp3 / ADTS）的探测非常宽松，
    // 任意二进制都可能被判成「音频流」——真解出样本才算这个文件能播。
    match decoder.next_chunk() {
        Ok(Some(chunk)) => decoder.primer = Some(chunk),
        Ok(None) => bail!("文件里没有可解码的音频数据"),
        Err(error) => return Err(error.context("内置解码器读不出音频数据")),
    }

    Ok(Box::new(decoder))
}

impl Decoder for SymphoniaDecoder {
    fn backend(&self) -> Backend {
        Backend::Symphonia
    }

    fn spec(&self) -> StreamSpec {
        self.spec
    }

    fn next_chunk(&mut self) -> anyhow::Result<Option<Vec<f32>>> {
        if let Some(chunk) = self.primer.take() {
            return Ok(Some(chunk));
        }
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                // 文件尾部被截断：把已经解出来的放完，不要当致命错误。
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(buffer) => {
                    self.interleaved.clear();
                    buffer.copy_to_vec_interleaved::<f32>(&mut self.interleaved);
                    let raw = std::mem::take(&mut self.interleaved);
                    to_stereo(raw, self.source_channels, &mut self.stereo);
                    if self.stereo.is_empty() {
                        continue;
                    }
                    return Ok(Some(std::mem::take(&mut self.stereo)));
                }
                // 单个包坏掉不影响整首歌，丢掉继续。
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn seek(&mut self, to: Duration) -> anyhow::Result<()> {
        // 标签/ffprobe 报的时长可能比容器真实帧数长（编码器延迟、尾部 padding
        // 都算进去了）。直接拿它 seek 到最右边，Symphonia 会判 out-of-range。
        // 这里按容器自己的帧数收一收，并给末帧留点余量。
        let to = match self.seek_end {
            Some(end) => to.min(end.saturating_sub(SEEK_END_MARGIN)),
            None => to,
        };
        let time = Time::try_new(to.as_secs() as i64, to.subsec_nanos())
            .context("seek 目标超出可表示范围")?;
        self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;
        // seek 之后解码器状态与新的包不连续，必须 reset；打开时试解的那块
        // 属于旧位置，也必须丢掉，否则会先响一下开头。
        self.decoder.reset();
        self.primer = None;
        Ok(())
    }
}

/// 容器里真实可 seek 的上界（秒）。
fn seekable_end(track: &Track) -> Option<Duration> {
    let time_base = track.time_base?;
    let frames = track.num_frames?;
    let end_ts = track.start_ts.checked_add(UnitsDuration::from(frames))?;
    let end = time_base.calc_time(end_ts)?;
    let seconds = end.as_secs_f64();
    (seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}
