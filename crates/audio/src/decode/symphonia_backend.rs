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
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use super::{Backend, Decoder, StreamSpec, to_stereo};

pub struct SymphoniaDecoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// 源文件自己的声道数，用来决定怎么做立体声映射。
    source_channels: usize,
    spec: StreamSpec,
    interleaved: Vec<f32>,
    stereo: Vec<f32>,
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
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
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

    let mut decoder = SymphoniaDecoder {
        reader,
        decoder,
        track_id,
        source_channels,
        spec: StreamSpec { rate, channels: 2 },
        interleaved: Vec::new(),
        stereo: Vec::new(),
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
        let time = Time::try_new(to.as_secs() as i64, to.subsec_nanos())
            .context("seek 目标超出可表示范围")?;
        self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time { time, track_id: Some(self.track_id) },
        )?;
        // seek 之后解码器状态与新的包不连续，必须 reset。
        self.decoder.reset();
        Ok(())
    }
}
