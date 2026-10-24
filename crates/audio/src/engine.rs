//! 播放引擎：UI 只跟 `Player` 说话，不碰线程、缓冲与状态机。

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use ringbuf::traits::{Observer as _, Producer as _};

use crate::decode::{self, Decoder};
use crate::meta::TrackMeta;
use crate::output::{self, OutputConfig, OutputProducer};
use crate::resample::StreamResampler;
use crate::state::{PlaybackState, Shared, Snapshot};

/// 一次加载请求。UI 可以在 `TrackMeta` 还没读完标签时先塞占位进来，
/// 引擎只用 `path` 与 `duration`。
pub type Track = Arc<TrackMeta>;

enum Command {
    Load(Track),
    SetPaused(bool),
    Seek(f64),
    /// 清空当前曲目。
    Stop,
}

pub struct Player {
    shared: Arc<Shared>,
    command_tx: Sender<Command>,
    /// cpal 的 stream 必须在整个播放期间存活，drop 即停声。
    _stream: cpal::Stream,
    config: OutputConfig,
}

impl Player {
    /// 打开默认输出设备并启动解码线程。
    pub fn new() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::new("默认设备".into()));
        let (stream, config, producer) = output::open(&shared)?;
        shared.set_device(config.rate, config.channels, config.device.clone());

        let (command_tx, command_rx) = channel();
        let worker_shared = shared.clone();
        let target_rate = config.rate;
        std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || decode_loop(worker_shared, command_rx, producer, target_rate))
            .context("启动解码线程失败")?;

        Ok(Self {
            shared,
            command_tx,
            _stream: stream,
            config,
        })
    }

    pub fn config(&self) -> &OutputConfig {
        &self.config
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot()
    }

    /// 频谱图数据：最近一小段单声道样本（可能为空）。
    pub fn spectrum(&self) -> Vec<f32> {
        self.shared
            .viz
            .try_lock()
            .map(|viz| viz.clone())
            .unwrap_or_default()
    }

    pub fn load(&self, track: Track) {
        let _ = self.command_tx.send(Command::Load(track));
    }

    /// 标签是后补的：列表先放占位曲目就能立刻开播，等 `lofty` 探测完再回填。
    /// 不回填的话，这条路径起来的曲子时长永远是 0——进度条没有比例、总时长显示 0:00。
    /// 只更新「当前正在播的这首」，避免把别的曲子的时长写到进度条上。
    pub fn refresh_metadata(&self, track: Track) {
        self.shared.refresh_track(track);
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.command_tx.send(Command::SetPaused(paused));
    }

    /// 在「播放/暂停」之间切换。没有音源时什么也不做。
    pub fn toggle_pause(&self) {
        let snapshot = self.shared.snapshot();
        match snapshot.state {
            PlaybackState::Playing => self.set_paused(true),
            PlaybackState::Paused | PlaybackState::Ended => self.set_paused(false),
            _ => {}
        }
    }

    /// 跳到 `seconds` 秒。
    ///
    /// 位置要等解码线程真的 seek 成功才更新：失败时进度条不该跳到一个
    /// 其实没到达的地方，否则用户会以为「跳过去了但没声音」。
    pub fn seek(&self, seconds: f64) {
        let _ = self.command_tx.send(Command::Seek(seconds));
    }

    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        // 音频回调每块都读这个原子量，不需要经过解码线程。
        self.shared
            .volume_bits
            .store(volume.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn stop(&self) {
        let _ = self.command_tx.send(Command::Stop);
    }

    /// 系统里有没有 ffmpeg —— 决定「所有格式」是不是真的成立。
    pub fn ffmpeg_available() -> bool {
        decode::ffmpeg::available()
    }
}

struct Session {
    decoder: Box<dyn Decoder>,
    resampler: Option<StreamResampler>,
    /// 上次没塞进环形缓冲的样本（缓冲满时会出现）。
    pending: Vec<f32>,
    finished: bool,
    /// 解码完毕、缓冲已空，等这一小段时间再判定「播完」，
    /// 免得设备缓冲里最后几十毫秒还没出声就被切歌。
    drain_deadline: Option<Instant>,
}

/// 请求音频回调清空环形缓冲，并等它真的清完（`flush` 由回调置回 false）。
///
/// 读指针只有回调能移动，所以解码线程只能「请求 + 等确认」。不等确认就往缓冲里
/// 写新数据的话，新数据会被这次清空顺手倒掉——短曲子会整首变成静音。
/// 设备被拔掉、流出不来时回调根本不会跑，所以只等一小会儿就放弃，别把解码线程卡死。
fn flush_and_wait(shared: &Shared) {
    shared
        .flush
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(150);
    while shared.flush.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn decode_loop(
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    mut producer: OutputProducer,
    target_rate: u32,
) {
    let mut session: Option<Session> = None;

    loop {
        // —— 1. 取命令 ——
        let command = if session.is_none() {
            // 没有会话时阻塞等待，别空转烧 CPU。
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        } else {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };

        if let Some(command) = command {
            match command {
                Command::Load(track) => {
                    // 先把上一首留在缓冲里的尾巴丢掉，**并且等回调真的丢完**再开始
                    // 解码新曲子。顺序很重要：如果先 mark_ready 再置 flush，回调可能
                    // 在新数据已经填满缓冲之后才跑，那次「清尾巴」会把整首新曲子一起
                    // 倒掉（短曲子直接变成一段静音，位置却照走）。
                    flush_and_wait(&shared);
                    session = start_session(&shared, track, target_rate);
                }
                Command::SetPaused(paused) => {
                    shared
                        .paused
                        .store(paused, std::sync::atomic::Ordering::Relaxed);
                    if !paused && shared.ended.load(std::sync::atomic::Ordering::Relaxed) {
                        // 在「播完」状态按播放：从头再来。最后一首播完后会话已经
                        // 丢掉，先重新开流；自动切歌时 Load 会先清掉 ended，不走这里。
                        if session.is_none()
                            && let Some(track) =
                                shared.track.lock().ok().and_then(|slot| slot.clone())
                        {
                            session = start_session(&shared, track, target_rate);
                        }
                        if let Some(session) = session.as_mut() {
                            let _ = session.decoder.seek(Duration::ZERO);
                            if let Some(resampler) = session.resampler.as_mut() {
                                resampler.reset();
                            }
                            session.finished = false;
                            session.drain_deadline = None;
                            // 播完时可能还剩一点重采样尾巴没推完，别让它在新一轮开头响。
                            session.pending.clear();
                            shared.seek_to(0.0);
                            shared
                                .ended
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
                Command::Seek(seconds) => {
                    let seconds = seconds.max(0.0);
                    // 播完之后会话已经丢掉（最后一遍播完就停在这里，切歌由 UI 负责）。
                    // 这时拖动进度条应该重新开流，而不是什么都不发生。
                    if session.is_none()
                        && shared.ended.load(std::sync::atomic::Ordering::Relaxed)
                        && let Some(track) = shared.track.lock().ok().and_then(|slot| slot.clone())
                    {
                        session = start_session(&shared, track, target_rate);
                    }
                    if let Some(session) = session.as_mut() {
                        let to = Duration::from_secs_f64(seconds);
                        match session.decoder.seek(to) {
                            Ok(()) => {
                                if let Some(resampler) = session.resampler.as_mut() {
                                    resampler.reset();
                                }
                                session.finished = false;
                                session.drain_deadline = None;
                                session.pending.clear();
                                // seek 之前缓冲里还压着最多两秒的「旧位置」音频，
                                // 不丢掉的话，松手以后要过一会儿才听到新位置的声音。
                                shared.clear_error();
                                shared.seek_to(seconds);
                                flush_and_wait(&shared);
                            }
                            Err(error) => {
                                // seek 失败不该停掉播放：继续从当前位置放，但要让用户知道。
                                shared.set_error(format!("跳转失败：{error:#}"));
                            }
                        }
                    }
                }
                Command::Stop => {
                    session = None;
                    if let Ok(mut track) = shared.track.lock() {
                        *track = None;
                    }
                    shared
                        .active
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .flush
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            continue;
        }

        let Some(active) = session.as_mut() else {
            continue;
        };

        // —— 2. 暂停时不生产 ——
        if shared.paused.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(12));
            continue;
        }

        // —— 3. 解码完毕：等缓冲排空 ——
        if active.finished {
            if producer.occupied_len() == 0 {
                let deadline = *active
                    .drain_deadline
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(80));
                if Instant::now() >= deadline {
                    shared.mark_finished();
                    session = None;
                }
            } else {
                active.drain_deadline = None;
            }
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }

        // —— 4. 先把上次没写完的补上 ——
        if !active.pending.is_empty() {
            let written = producer.push_slice(&active.pending);
            active.pending.drain(..written);
            if !active.pending.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
        }

        // 缓冲还够大就不再解码，让它自然填满。
        if producer.vacant_len() < 4096 {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        }

        // —— 5. 解一块 ——
        match active.decoder.next_chunk() {
            Ok(Some(chunk)) => {
                let mut out = Vec::with_capacity(chunk.len());
                match active.resampler.as_mut() {
                    Some(resampler) => resampler.process(&chunk, &mut out),
                    None => out = chunk,
                }
                let written = producer.push_slice(&out);
                if written < out.len() {
                    active.pending.extend_from_slice(&out[written..]);
                }
            }
            Ok(None) => {
                // 把重采样器里剩的尾巴也送出去。
                if let Some(resampler) = active.resampler.as_mut() {
                    let mut tail = Vec::new();
                    resampler.finish(&mut tail);
                    if !tail.is_empty() {
                        let written = producer.push_slice(&tail);
                        if written < tail.len() {
                            active.pending.extend_from_slice(&tail[written..]);
                        }
                    }
                }
                active.finished = true;
            }
            Err(error) => {
                shared.set_error(format!("{error:#}"));
                session = None;
            }
        }
    }
}

fn start_session(shared: &Arc<Shared>, track: Track, target_rate: u32) -> Option<Session> {
    shared.begin_load(track.clone());

    let decoder = match decode::open(&track.path, target_rate) {
        Ok(decoder) => decoder,
        Err(error) => {
            shared.set_error(format!("{error:#}"));
            return None;
        }
    };

    let spec = decoder.spec();
    let resampler = if spec.rate != target_rate {
        match StreamResampler::new(spec.rate, target_rate, spec.channels.max(1) as usize) {
            Ok(resampler) => Some(resampler),
            Err(error) => {
                shared.set_error(format!("{error:#}"));
                return None;
            }
        }
    } else {
        None
    };

    let duration_ms = track.duration.map(|d| d.as_millis() as u64).unwrap_or(0);
    shared.mark_ready(decoder.backend(), duration_ms);

    Some(Session {
        decoder,
        resampler,
        pending: Vec::new(),
        finished: false,
        drain_deadline: None,
    })
}

/// 支持的格式清单（给设置页 / 帮助文本用）。
pub fn supported_formats() -> Vec<&'static str> {
    crate::format::dialog_extensions().to_vec()
}

/// 路径 → 占位曲目（读标签在 UI 的后台线程里做）。
pub fn placeholder_track(path: PathBuf) -> Track {
    Arc::new(TrackMeta::placeholder(&path))
}

/// 一个给 UI 用的全局锁：避免同时打开两个 Player 抢设备。
pub fn player_slot() -> &'static Mutex<()> {
    static SLOT: Mutex<()> = Mutex::new(());
    &SLOT
}
