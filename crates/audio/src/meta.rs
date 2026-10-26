//! 曲目元数据：标题、艺人、专辑、时长、参数，以及内嵌封面。
//!
//! 一律走 lofty：它覆盖 mp3/flac/m4a/ogg/opus/wav/aiff/wv/ape 的标签与封面，
//! 而且读不了标签时**不影响播放**——所以这里所有失败都降级成 `None` / 文件名。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::format;

/// 内嵌封面（原始字节，交给 UI 去解码）。
#[derive(Clone)]
pub struct CoverArt {
    pub bytes: Vec<u8>,
    pub mime: String,
}

impl std::fmt::Debug for CoverArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CoverArt({} bytes, {})", self.bytes.len(), self.mime)
    }
}

/// 曲目在整轨文件里的时间窗（CUE 分轨）。
///
/// 引擎与解码器对外看到的时间轴始终是「窗口内的 0..len」：分轨的进度条从 0
/// 开始、总时长是这一首的长度，而不是整轨的长度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeWindow {
    /// 在文件里的起点。
    pub start: Duration,
    /// 在文件里的终点。
    pub end: Duration,
}

impl TimeWindow {
    pub fn len(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len().is_zero()
    }
}

/// 一首曲子的全部展示信息。
#[derive(Clone, Debug)]
pub struct TrackMeta {
    pub path: PathBuf,
    /// 有值表示这一条是从整轨里切出来的分轨。
    pub window: Option<TimeWindow>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<Duration>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bit_depth: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    /// 显示用格式名（`FLAC` / `MPEG Audio` …）。
    pub format: String,
    pub cover: Option<CoverArt>,
}

impl TrackMeta {
    /// 只从路径构造（标签还没读出来时的占位）。
    ///
    /// 顺手把「艺人 - 标题」文件名拆开：列表在标签读出来之前就能显示得像样，
    /// 而不是一屏 `01 - 某某 - 某某.flac`。
    pub fn placeholder(path: &Path) -> Self {
        let stem = file_stem(path);
        let (artist, title) = match split_artist_title(&stem) {
            Some((artist, title)) => (artist, title),
            None => (String::new(), stem),
        };
        Self {
            path: path.to_path_buf(),
            window: None,
            title,
            artist,
            album: String::new(),
            duration: None,
            sample_rate: None,
            channels: None,
            bit_depth: None,
            bitrate_kbps: None,
            format: format::label_for(path),
            cover: None,
        }
    }

    /// 占位曲目 + 时间窗：从磁盘恢复列表时用，不读标签。
    pub fn placeholder_window(path: &Path, window: TimeWindow) -> Self {
        let mut meta = Self::placeholder(path);
        meta.window = Some(window);
        meta.duration = Some(window.len());
        meta
    }

    /// 读标签，然后把这条曲目钉在整轨的某一段时间上。
    ///
    /// 时长必须换成窗口长度：整轨的时长写进进度条会让分轨「一打开就播完了」。
    pub fn probe_window(path: &Path, window: TimeWindow) -> Self {
        let mut meta = Self::probe(path);
        meta.window = Some(window);
        meta.duration = Some(window.len());
        meta
    }

    /// 整轨里的一首分轨。文件级标签先读，再用 CUE 里写的标题 / 艺人覆盖：
    /// 整轨文件的标签描述的是整张碟，CUE 里的才是「这首是谁唱的」。
    pub fn from_cue(
        path: &Path,
        window: TimeWindow,
        title: Option<&str>,
        artist: Option<&str>,
    ) -> Self {
        let mut meta = Self::probe_window(path, window);
        if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
            meta.title = title.to_string();
        }
        if let Some(artist) = artist.map(str::trim).filter(|artist| !artist.is_empty()) {
            meta.artist = artist.to_string();
        }
        meta
    }

    /// 读标签与音频参数。任何一步失败都只是少一点信息，不视为错误。
    pub fn probe(path: &Path) -> Self {
        let mut meta = Self::placeholder(path);

        let Ok(tagged) = lofty::read_from_path(path) else {
            return meta;
        };

        use lofty::file::{AudioFile as _, TaggedFileExt as _};
        let props = tagged.properties();
        if props.duration() > Duration::ZERO {
            meta.duration = Some(props.duration());
        }
        meta.sample_rate = props.sample_rate();
        meta.channels = props.channels().map(u16::from);
        meta.bit_depth = props.bit_depth();
        meta.bitrate_kbps = props.audio_bitrate().or_else(|| props.overall_bitrate());

        // 有些容器（典型是 WMA / ASF）lofty 拿不到时长，界面上会显示成「—」。
        // 这时问一次 ffprobe：它在后台线程里跑，几十毫秒的进程启动不影响交互。
        if meta.duration.is_none() {
            meta.duration = ffprobe_duration(path);
        }

        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            use lofty::tag::Accessor as _;
            if let Some(title) = tag.title().filter(|t| !t.trim().is_empty()) {
                meta.title = title.trim().to_string();
            }
            if let Some(artist) = tag.artist().filter(|t| !t.trim().is_empty()) {
                meta.artist = artist.trim().to_string();
            }
            if let Some(album) = tag.album().filter(|t| !t.trim().is_empty()) {
                meta.album = album.trim().to_string();
            }
            if let Some(picture) = tag.pictures().first()
                && !picture.data().is_empty()
            {
                meta.cover = Some(CoverArt {
                    bytes: picture.data().to_vec(),
                    mime: picture
                        .mime_type()
                        .map(|mime| mime.as_str().to_string())
                        .unwrap_or_else(|| "image/jpeg".into()),
                });
            }
        }

        meta
    }

    /// 一行显示用：`艺人 — 标题`；没有艺人时只有标题。
    pub fn display_title(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.artist, self.title)
        }
    }

    /// 副标题：`FLAC · 44.1 kHz · 16 bit · 1000 kbps`，缺的项自动跳过。
    pub fn spec_line(&self) -> String {
        let mut parts = vec![self.format.clone()];
        if let Some(rate) = self.sample_rate {
            parts.push(format!("{:.1} kHz", rate as f32 / 1000.0));
        }
        if let Some(depth) = self.bit_depth {
            parts.push(format!("{depth} bit"));
        }
        if let Some(kbps) = self.bitrate_kbps {
            parts.push(format!("{kbps} kbps"));
        }
        parts.join(" · ")
    }
}

/// 用 ffprobe 读时长（秒）。失败就返回 `None`，不影响播放。
fn ffprobe_duration(path: &Path) -> Option<Duration> {
    let probe = crate::decode::ffmpeg::ffprobe_path()?;
    let output = crate::decode::ffmpeg::command(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let seconds: f64 = text.trim().parse().ok()?;
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "未知曲目".into())
}

/// 从 `艺人 - 标题` / `艺人 – 标题` 里拆出两段；拆不出就返回 `None`。
fn split_artist_title(stem: &str) -> Option<(String, String)> {
    for sep in [" - ", " – ", " — ", "_-_"] {
        if let Some((artist, title)) = stem.split_once(sep) {
            let (artist, title) = (artist.trim(), title.trim());
            if !artist.is_empty() && !title.is_empty() {
                return Some((artist.to_string(), title.to_string()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_artist_from_stem() {
        assert_eq!(
            split_artist_title("Radiohead - Creep"),
            Some(("Radiohead".into(), "Creep".into()))
        );
        assert_eq!(split_artist_title("没有分隔符"), None);
    }

    #[test]
    fn placeholder_uses_stem_when_no_tag() {
        let meta = TrackMeta::placeholder(Path::new("/music/Bonobo - Kerala.flac"));
        assert_eq!(meta.artist, "Bonobo");
        assert_eq!(meta.title, "Kerala");
        assert_eq!(meta.format, "FLAC");
    }
}
