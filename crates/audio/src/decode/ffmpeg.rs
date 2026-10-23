//! 兜底解码器：系统 ffmpeg 子进程。
//!
//! 为什么用子进程而不是 `ffmpeg-next`（libav 绑定）：
//!
//! * 绑定需要编译期找到 libav*，三平台各自一套构建链，发布包会从 10MB 变成 100MB+；
//! * 用户机器上本来就有 ffmpeg 的概率远高于「愿意为一个播放器装整套开发库」；
//! * 子进程崩溃不会带走播放器进程。
//!
//! 代价是多一次进程启动（几十毫秒，只在切歌时发生），以及需要用户在 PATH 里装 ffmpeg。
//! 没装的时候 `available()` 返回 `None`，UI 会明说「这个格式需要 ffmpeg」。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, bail};

use super::{Backend, Decoder, StreamSpec};

/// 每次向 ffmpeg 要多少帧。4096 帧 ≈ 85ms@48k，既够大（系统调用少）又够小（seek 响应快）。
const CHUNK_FRAMES: usize = 4096;
const CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 4;

/// 找到 ffmpeg 可执行文件（结果缓存，进程内只探一次）。
pub fn ffmpeg_path() -> Option<&'static Path> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = Vec::new();

            if let Some(explicit) = std::env::var_os("NGY_AUDIO_FFMPEG") {
                candidates.push(PathBuf::from(explicit));
            }
            // PATH 里的每一个目录。
            if let Some(path) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path) {
                    candidates.push(dir.join(exe_name()));
                }
            }
            // 常见的「装了但没进 PATH」位置。
            #[cfg(windows)]
            {
                for base in ["C:/ffmpeg/bin", "C:/Program Files/ffmpeg/bin", "D:/ffmpeg/bin"] {
                    candidates.push(PathBuf::from(base).join(exe_name()));
                }
            }
            #[cfg(target_os = "macos")]
            {
                for base in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
                    candidates.push(PathBuf::from(base).join(exe_name()));
                }
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                for base in ["/usr/bin", "/usr/local/bin", "/snap/bin", "/app/bin"] {
                    candidates.push(PathBuf::from(base).join(exe_name()));
                }
            }

            candidates.into_iter().find(|candidate| works(candidate))
        })
        .as_deref()
}

/// 是否具备 ffmpeg 兜底能力。
pub fn available() -> bool {
    ffmpeg_path().is_some()
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }
}

/// 真跑一次 `-version`：同名文件不代表能执行（权限、架构不符、AppLocker 都会挡）。
fn works(candidate: &Path) -> bool {
    if !candidate.is_file() {
        return false;
    }
    Command::new(candidate)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .hide_window()
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn open(path: &Path, target_rate: u32) -> anyhow::Result<Box<dyn Decoder>> {
    let exe = ffmpeg_path()
        .context("找不到 ffmpeg。装一个（或在设置里指定 NGY_AUDIO_FFMPEG）就能播这个格式")?
        .to_path_buf();
    let mut decoder = FfmpegDecoder::spawn(exe, path, target_rate, Duration::ZERO)
        .with_context(|| format!("ffmpeg 无法解码：{}", path.display()))?;

    // 只 spawn 成功不代表这个文件是音频：ffmpeg 是先起进程、后探测输入的。
    // 必须真读到样本才算打开成功，否则错误会被推迟到播放中途才冒出来。
    match decoder.next_chunk() {
        Ok(Some(chunk)) => decoder.primer = Some(chunk),
        Ok(None) => bail!("文件里没有可解码的音频数据"),
        Err(error) => return Err(error),
    }

    Ok(Box::new(decoder))
}

pub struct FfmpegDecoder {
    exe: PathBuf,
    path: PathBuf,
    rate: u32,
    child: Child,
    stderr_tail: Arc<Mutex<String>>,
    buffer: Vec<u8>,
    /// 本次连接是否已经交出过样本。用来区分「正常的流结束」与「一开就失败」。
    produced_any: bool,
    /// 打开时试解出来的第一块。
    primer: Option<Vec<f32>>,
}

impl FfmpegDecoder {
    fn spawn(
        exe: PathBuf,
        path: &Path,
        rate: u32,
        start: Duration,
    ) -> anyhow::Result<Self> {
        let mut command = Command::new(&exe);
        command
            .arg("-nostdin")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error");
        if start > Duration::ZERO {
            // 放在 -i 之前是 input seek：直接跳到目标附近的包，不必解码前面的全部内容。
            command.arg("-ss").arg(format!("{:.3}", start.as_secs_f64()));
        }
        command
            .arg("-i")
            .arg(path)
            // 只要第一条音轨，忽略视频/字幕。视频容器也能因此只解音频。
            .args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
            .args(["-ac", &CHANNELS.to_string()])
            .args(["-ar", &rate.to_string()])
            // 统一成 32 位浮点裸流：与内部表示一致，省掉一层整数转换。
            .args(["-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .hide_window();

        let mut child = command
            .spawn()
            .with_context(|| format!("启动 ffmpeg 失败：{}", exe.display()))?;

        // 把 stderr 读进一个环形尾巴：ffmpeg 失败的原因全在这几句话里，
        // 不排空管道还可能让它在日志多时卡死。
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::Builder::new()
                .name("ffmpeg-stderr".into())
                .spawn(move || {
                    let mut chunk = [0u8; 1024];
                    while let Ok(read) = stderr.read(&mut chunk) {
                        if read == 0 {
                            break;
                        }
                        if let Ok(mut tail) = tail.lock() {
                            tail.push_str(&String::from_utf8_lossy(&chunk[..read]));
                            if tail.len() > 4096 {
                                let cut = tail.len() - 4096;
                                // 只按字符边界截断，避免把中文截成半个。
                                let cut = (cut..tail.len())
                                    .find(|index| tail.is_char_boundary(*index))
                                    .unwrap_or(tail.len());
                                tail.drain(..cut);
                            }
                        }
                    }
                })
                .ok();
        }

        Ok(Self {
            exe,
            path: path.to_path_buf(),
            rate,
            child,
            stderr_tail,
            buffer: vec![0u8; CHUNK_FRAMES * CHANNELS * BYTES_PER_SAMPLE],
            produced_any: false,
            primer: None,
        })
    }

    fn stderr_message(&self) -> String {
        // ffmpeg 可能还在把错误写完，给它一点时间再读。
        std::thread::sleep(Duration::from_millis(30));
        let text = self
            .stderr_tail
            .lock()
            .map(|tail| tail.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() { "ffmpeg 没有给出原因（可能这条音轨不存在）".into() } else { text }
    }
}

impl Decoder for FfmpegDecoder {
    fn backend(&self) -> Backend {
        Backend::Ffmpeg
    }

    fn spec(&self) -> StreamSpec {
        StreamSpec { rate: self.rate, channels: CHANNELS as u16 }
    }

    fn next_chunk(&mut self) -> anyhow::Result<Option<Vec<f32>>> {
        if let Some(chunk) = self.primer.take() {
            return Ok(Some(chunk));
        }

        let Some(stdout) = self.child.stdout.as_mut() else {
            bail!("ffmpeg 的输出管道已关闭");
        };

        let mut filled = 0;
        while filled < self.buffer.len() {
            match stdout.read(&mut self.buffer[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("读取 ffmpeg 输出失败"),
            }
        }

        if filled == 0 {
            // 一个字节都没给出来：这不是「播完了」，是 ffmpeg 直接失败。
            if !self.produced_any {
                let message = self.stderr_message();
                bail!("{message}");
            }
            return Ok(None);
        }

        // 最后一个块可能不是整数帧：截掉零头，避免左右声道错位。
        let frame_bytes = CHANNELS * BYTES_PER_SAMPLE;
        let filled = filled - filled % frame_bytes;
        let samples: Vec<f32> = self.buffer[..filled]
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        self.produced_any = true;
        Ok(Some(samples))
    }

    fn seek(&mut self, to: Duration) -> anyhow::Result<()> {
        // ffmpeg 没有「原地 seek」这种便宜事：杀掉重启，用 -ss 从目标处重新开流。
        let _ = self.child.kill();
        let _ = self.child.wait();
        let restarted = Self::spawn(self.exe.clone(), &self.path, self.rate, to)?;
        *self = restarted;
        Ok(())
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Windows 上不想每次切歌都闪一下黑窗。
trait HideWindow {
    fn hide_window(&mut self) -> &mut Self;
}

impl HideWindow for Command {
    fn hide_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

/// 起一个 ffmpeg 家族的进程（ffmpeg / ffprobe），统一带上「别弹黑窗」。
pub(crate) fn command(exe: impl AsRef<Path>) -> Command {
    let mut command = Command::new(exe.as_ref());
    command.hide_window();
    command
}

/// 同目录下的 ffprobe；没有就返回 None。
pub(crate) fn ffprobe_path() -> Option<PathBuf> {
    let ffmpeg = ffmpeg_path()?;
    let name = if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" };
    let probe = ffmpeg.with_file_name(name);
    probe.is_file().then_some(probe)
}
