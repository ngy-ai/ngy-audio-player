//! CUE 分轨表的解析与展开。
//!
//! 「整轨 + 一份 .cue」是按专辑发行的常见形态：音频文件只有一个，曲目信息全在
//! cue 里。这里把 cue 展开成多条独立曲目（见 `docs/adr/0005`）：文件只打开一次，
//! 每条曲目只在自己的时间窗里播放。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::meta::{TimeWindow, TrackMeta};

/// 一个 cue 文件。一张碟可能被拆成几个 FILE，所以这里是列表。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueSheet {
    pub files: Vec<CueFile>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CueFile {
    /// cue 里原样写的文件名（可能是相对路径，也可能用 `\` 分隔）。
    pub name: String,
    pub tracks: Vec<CueTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CueTrack {
    pub number: u32,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// 文件内绝对起点。
    pub start: Duration,
    /// 下一首的起点；最后一首为 `None`，由整轨时长补。
    pub end: Option<Duration>,
}

/// 解析 cue 文本。语法不认识的行直接跳过——cue 的方言很多（`REM`、`CATALOG`、
/// 各种 `FLAGS`），为了一个无关字段让整张碟读不出来不值得。
pub fn parse(text: &str) -> CueSheet {
    let mut sheet = CueSheet::default();
    let mut current: Option<CueFile> = None;
    let mut album_artist: Option<String> = None;

    for line in text.lines() {
        let tokens = tokenize(line);
        let Some(command) = tokens.first() else {
            continue;
        };
        let rest = &tokens[1..];
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                if let Some(file) = current.take() {
                    sheet.files.push(file);
                }
                let name = rest.first().cloned().unwrap_or_default();
                if !name.is_empty() {
                    current = Some(CueFile {
                        name,
                        tracks: Vec::new(),
                    });
                }
            }
            "TRACK" => {
                let Some(file) = current.as_mut() else {
                    continue;
                };
                file.tracks.push(CueTrack {
                    number: rest.first().and_then(|text| text.parse().ok()).unwrap_or(0),
                    title: None,
                    artist: None,
                    start: Duration::ZERO,
                    end: None,
                });
            }
            "TITLE" => {
                // 出现在第一条 TRACK 之前是整张碟的标题，分轨用不上。
                if let Some(track) = current.as_mut().and_then(|file| file.tracks.last_mut()) {
                    track.title = rest.first().cloned();
                }
            }
            "PERFORMER" => {
                if let Some(track) = current.as_mut().and_then(|file| file.tracks.last_mut()) {
                    track.artist = rest.first().cloned();
                } else if album_artist.is_none() {
                    album_artist = rest.first().cloned();
                }
            }
            "INDEX" => {
                // 只要 INDEX 01：INDEX 00 是 pregap（上一首的静音尾巴）。
                let is_first = rest
                    .first()
                    .is_some_and(|index| index == "01" || index == "1");
                if is_first
                    && let Some(start) = rest.get(1).and_then(|text| parse_time(text))
                    && let Some(track) = current.as_mut().and_then(|file| file.tracks.last_mut())
                {
                    track.start = start;
                }
            }
            _ => {}
        }
    }
    if let Some(file) = current {
        sheet.files.push(file);
    }

    for file in &mut sheet.files {
        // 碟级艺人回填给没写艺人的分轨。
        for track in &mut file.tracks {
            if track.artist.is_none() {
                track.artist = album_artist.clone();
            }
        }
        // 终点就是下一首的起点；最后一首留给整轨时长。
        for index in 0..file.tracks.len() {
            file.tracks[index].end = file.tracks.get(index + 1).map(|next| next.start);
        }
    }

    sheet
}

/// 把一个 cue 文件展开成可以直接进列表的曲目。
///
/// 会读一次整轨的标签与时长：最后一首的终点、以及没有 `TITLE` 时的兜底信息都靠它。
pub fn expand(cue_path: &Path) -> anyhow::Result<Vec<TrackMeta>> {
    let bytes = std::fs::read(cue_path)
        .with_context(|| format!("读不了 cue 文件：{}", cue_path.display()))?;
    let text = decode_text(&bytes);
    let sheet = parse(&text);
    let dir = cue_path.parent().unwrap_or(Path::new("."));

    let mut tracks = Vec::new();
    for file in &sheet.files {
        let path = resolve(dir, &file.name);
        // 只有最后一首需要整轨时长兜底，别的都能从下一首推出来。
        let total = if file.tracks.iter().any(|track| track.end.is_none()) {
            TrackMeta::probe(&path).duration
        } else {
            None
        };

        for track in &file.tracks {
            let Some(end) = track.end.or(total).filter(|end| *end > track.start) else {
                // 不知道终点就不知道切到哪，宁可跳过也不要放出一段越界的音频。
                continue;
            };
            let window = TimeWindow {
                start: track.start,
                end,
            };
            let mut meta = TrackMeta::from_cue(
                &path,
                window,
                track.title.as_deref(),
                track.artist.as_deref(),
            );
            // 没写 TITLE 时 `from_cue` 会保留整轨标题，一屏全是同一个名字；
            // 用序号兜底，至少能分清第几首。
            if track
                .title
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            {
                meta.title = format!("Track {:02}", track.number);
            }
            tracks.push(meta);
        }
    }

    if tracks.is_empty() {
        bail!("这张 cue 里没有可用的分轨");
    }
    Ok(tracks)
}

/// 是不是 cue 分轨表。
pub fn is_cue_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
}

/// cue 里的文件名 → 实际文件。
///
/// 老 cue 常写 `\` 分隔并且和真实文件名差个大小写，直接 join 会扑空。
fn resolve(dir: &Path, name: &str) -> PathBuf {
    let normalized = name.replace('\\', std::path::MAIN_SEPARATOR_STR);
    let candidate = dir.join(&normalized);
    if candidate.is_file() {
        return candidate;
    }
    let Some(stem) = Path::new(&normalized)
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return candidate;
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_stem()
                .and_then(|other| other.to_str())
                .is_some_and(|other| other.eq_ignore_ascii_case(stem))
            {
                return path;
            }
        }
    }
    candidate
}

/// cue 没有编码声明，中文碟又大量是 GBK 存的：UTF-8 解不出来就按 GBK 解，
/// 否则整张碟的曲名都会是乱码。
fn decode_text(bytes: &[u8]) -> String {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    };
    text.trim_start_matches('\u{feff}').to_string()
}

/// 按空白切词，成对引号里的内容算一个词。
fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let mut token = String::new();
        if ch == '"' {
            for next in chars.by_ref() {
                if next == '"' {
                    break;
                }
                token.push(next);
            }
        } else {
            token.push(ch);
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    break;
                }
                token.push(next);
                chars.next();
            }
        }
        tokens.push(token);
    }
    tokens
}

/// `mm:ss:ff`，其中 ff 是 1/75 秒。
fn parse_time(text: &str) -> Option<Duration> {
    let mut parts = text.split(':');
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    let frames: u64 = parts.next().unwrap_or("0").trim().parse().ok()?;
    // cue 的秒最多 59、帧最多 74；超出的多半是写坏的字段，别拿它算时间窗。
    if seconds >= 60 || frames >= 75 {
        return None;
    }
    let micros = minutes * 60_000_000 + seconds * 1_000_000 + frames * 1_000_000 / 75;
    Some(Duration::from_micros(micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = r#"
REM GENRE Rock
PERFORMER "Pink Floyd"
TITLE "The Wall"
FILE "The Wall.flac" WAVE
  TRACK 01 AUDIO
    TITLE "In the Flesh?"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "The Thin Ice"
    INDEX 01 03:19:12
  TRACK 03 AUDIO
    TITLE "Another Brick in the Wall"
    INDEX 01 05:47:00
"#;

    #[test]
    fn parses_tracks_and_windows() {
        let sheet = parse(SHEET);
        assert_eq!(sheet.files.len(), 1);
        let file = &sheet.files[0];
        assert_eq!(file.name, "The Wall.flac");
        assert_eq!(file.tracks.len(), 3);
        assert_eq!(file.tracks[0].start, Duration::ZERO);
        assert_eq!(file.tracks[0].end, Some(file.tracks[1].start));
        assert_eq!(file.tracks[2].end, None);
        assert_eq!(file.tracks[1].title.as_deref(), Some("The Thin Ice"));
        // 碟级 PERFORMER 回填到每一条分轨。
        assert_eq!(file.tracks[0].artist.as_deref(), Some("Pink Floyd"));
    }

    #[test]
    fn parses_time_with_frames() {
        assert_eq!(
            parse_time("03:19:12"),
            Some(Duration::from_micros(
                3 * 60_000_000 + 19 * 1_000_000 + 12 * 1_000_000 / 75
            ))
        );
        assert_eq!(parse_time("00:60:00"), None);
    }

    #[test]
    fn splits_multiple_files() {
        let sheet = parse(
            "FILE \"a.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nFILE b.flac WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        );
        assert_eq!(sheet.files.len(), 2);
        assert_eq!(sheet.files[1].name, "b.flac");
    }

    #[test]
    fn ignores_unknown_directives_and_pregap() {
        let sheet = parse(
            "FILE \"x.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 00 00:00:00\nINDEX 01 00:02:00\nFLAGS DCP\n",
        );
        assert_eq!(sheet.files[0].tracks[0].start, Duration::from_secs(2));
    }
}
