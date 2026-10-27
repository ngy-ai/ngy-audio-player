//! 格式表：文件过滤器、显示标签、「内置解码器能不能吃下」的预期，以及
//! 哪些格式要登记到系统里当默认打开方式。
//!
//! 这里的 `builtin` 只是**预期**，不是判据 —— 实际能否解码永远以「试一次」为准。
//! 例如 `.ogg` 既可能是 Vorbis 也可能是 Opus，前者 Symphonia 能解、后者不能，
//! 只看扩展名会判错。所以 `decode::open` 的策略是：先试 Symphonia，失败再走 ffmpeg。
//!
//! 扩展名清单只有 [`ALL_EXTS`] 一份：文件对话框、系统登记、拖放过滤都从它派生，
//! 免得出现「对话框能选、双击却打不开」这种两处清单不同步的问题。

use std::sync::OnceLock;

/// 一个扩展名的元信息。
#[derive(Clone, Copy, Debug)]
pub struct FormatInfo {
    /// 显示用的格式名，例如 `FLAC`。
    pub label: &'static str,
    /// Symphonia 是否**通常**能解。
    pub builtin: bool,
    /// 是否登记到系统里，作为「声波能打开的音频文件」。
    pub shell: bool,
}

/// 全部认得的扩展名（不含点，小写）。唯一权威清单，其它列表都从这里派生。
pub const ALL_EXTS: &[&str] = &[
    // —— 内置解码器覆盖的容器 / 编码 ——
    "mp3", "mp2", "mp1", "mpa", "flac", "wav", "wave", "aiff", "aif", "aifc", "caf", "m4a", "m4b",
    "m4r", "m4p", "mp4", "alac", "aac", "adts", "ogg", "oga", "mka", "mkv", "webm",
    // —— 需要 ffmpeg ——
    "opus", "spx", "wma", "asf", "ape", "wv", "tta", "tak", "shn", "mpc", "dsf", "dff", "amr",
    "awb", "ac3", "eac3", "ec3", "dts", "dtshd", "ra", "rm", "rmvb", "au", "snd", "voc", "w64",
    "paf", "svx", "ircam", "sf", "mod", "xm", "s3m", "it", "mtm", "umx", "gme", "nsf", "spc",
    "vgm", "vgz", "ay", "gbs", "hes", "kss", "sap",
    // —— 视频容器：能放出其中的音轨，但不该被当成音频文件去抢默认程序 ——
    "avi", "mov", "flv", "wmv", "ts", "m2ts", "mpg", "mpeg", "3gp", "3g2", "ogv", "vob",
];

/// 查扩展名（不含点，大小写不敏感）。未知扩展名返回 `None`。
pub fn lookup(ext: &str) -> Option<FormatInfo> {
    let ext = ext.to_ascii_lowercase();
    let (label, builtin, shell) = match ext.as_str() {
        // —— 内置解码器覆盖的容器 / 编码 ——
        "mp3" | "mp2" | "mp1" | "mpa" => ("MPEG Audio", true, true),
        "flac" => ("FLAC", true, true),
        "wav" | "wave" => ("WAV", true, true),
        "aiff" | "aif" | "aifc" => ("AIFF", true, true),
        "caf" => ("CAF", true, true),
        // `.mp4` 通常是视频，其余是装了 AAC / ALAC 的音频容器。
        "m4a" | "m4b" | "m4r" | "m4p" | "alac" => ("MPEG-4 Audio", true, true),
        "mp4" => ("MPEG-4 Audio", true, false),
        "aac" | "adts" => ("AAC", true, true),
        "ogg" | "oga" => ("Ogg", true, true),
        "mka" => ("Matroska", true, true),
        "mkv" | "webm" => ("Matroska", true, false),

        // —— 需要 ffmpeg ——
        "opus" => ("Opus", false, true),
        "spx" => ("Speex", false, true),
        "wma" => ("Windows Media Audio", false, true),
        "asf" => ("Windows Media Audio", false, false),
        "ape" => ("Monkey's Audio", false, true),
        "wv" => ("WavPack", false, true),
        "tta" => ("TTA", false, true),
        "tak" => ("TAK", false, true),
        "shn" => ("Shorten", false, true),
        "mpc" => ("Musepack", false, true),
        "dsf" => ("DSD (DSF)", false, true),
        "dff" => ("DSD (DFF)", false, true),
        "amr" | "awb" => ("AMR", false, true),
        "ac3" | "eac3" | "ec3" => ("Dolby AC-3", false, true),
        "dts" | "dtshd" => ("DTS", false, true),
        "ra" => ("RealAudio", false, true),
        "rm" | "rmvb" => ("RealAudio", false, false),
        "au" | "snd" => ("Sun AU", false, true),
        "voc" => ("Creative VOC", false, true),
        "w64" => ("Sony Wave64", false, true),
        "paf" => ("Paris Audio", false, true),
        "svx" => ("8SVX", false, true),
        "ircam" | "sf" => ("IRCAM", false, true),
        "mod" | "xm" | "s3m" | "it" | "mtm" | "umx" => ("Tracker Module", false, true),
        "gme" | "nsf" | "spc" | "vgm" | "vgz" | "ay" | "gbs" | "hes" | "kss" | "sap" => {
            ("Game Music", false, true)
        }
        // 视频容器也常带音轨：能放出声音，只是不画画面。
        "avi" | "mov" | "flv" | "wmv" | "ts" | "m2ts" | "mpg" | "mpeg" | "3gp" | "3g2" | "ogv"
        | "vob" => ("Video Container", false, false),
        _ => return None,
    };
    Some(FormatInfo {
        label,
        builtin,
        shell,
    })
}

/// 是否是可播放的音频文件（按扩展名）。
pub fn is_audio_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(lookup)
        .is_some()
}

/// 显示用的格式名；未知时退回大写的扩展名。
pub fn label_for(path: &std::path::Path) -> String {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return "AUDIO".into();
    };
    match lookup(ext) {
        Some(info) => info.label.into(),
        None => ext.to_ascii_uppercase(),
    }
}

/// 文件对话框的过滤器（rfd 只需要不含点的扩展名）。
///
/// 包含视频容器：它们里面的音轨也能播，用户可能就想这么用。
pub fn dialog_extensions() -> &'static [&'static str] {
    static CACHE: OnceLock<Vec<&'static str>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            ALL_EXTS
                .iter()
                .copied()
                .filter(|ext| lookup(ext).is_some())
                .collect()
        })
        .as_slice()
}

/// 登记到系统里的扩展名：只有音频类，视频容器不抢默认程序。
///
/// `.cue` 不在 [`ALL_EXTS`] 里（它是分轨表，不是音频编码），但它能被本程序打开，
/// 双击一张 cue 就是「加载整张专辑」，所以单独补上。
pub fn shell_extensions() -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = ALL_EXTS
        .iter()
        .copied()
        .filter(|ext| lookup(ext).is_some_and(|info| info.shell))
        .collect();
    exts.push("cue");
    exts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_extension_has_metadata() {
        for ext in ALL_EXTS {
            assert!(
                lookup(ext).is_some(),
                "`{ext}` 在 ALL_EXTS 里但没有 match 分支"
            );
        }
    }

    #[test]
    fn extension_lists_do_not_drift() {
        // 对话框清单是由 ALL_EXTS 派生的；这条断言守住「派生之后仍是一条不少」。
        assert_eq!(dialog_extensions().len(), ALL_EXTS.len());
        let mut unique = ALL_EXTS.to_vec();
        unique.sort_unstable();
        let before = unique.len();
        unique.dedup();
        assert_eq!(before, unique.len(), "ALL_EXTS 里有重复项");
    }

    #[test]
    fn shell_registration_covers_audio_but_not_video() {
        let exts = shell_extensions();
        for wanted in [
            "mp3", "flac", "wav", "m4a", "mka", "opus", "wma", "ra", "ec3", "cue",
        ] {
            assert!(exts.contains(&wanted), "`{wanted}` 应该登记");
        }
        // 视频容器：双击它们不该进入一个只出声的播放器。
        for unwanted in [
            "mp4", "mkv", "webm", "asf", "rm", "rmvb", "avi", "mov", "flv", "wmv", "ts", "m2ts",
            "mpg", "mpeg", "3gp", "3g2", "ogv", "vob",
        ] {
            assert!(!exts.contains(&unwanted), "`{unwanted}` 不该登记");
        }
        assert_eq!(exts.len(), 65, "登记清单数量变了：确认是有意加的格式");
    }

    #[test]
    fn video_containers_still_play() {
        // 不登记 ≠ 不能播：拖进来照样要出声。
        assert!(is_audio_path(std::path::Path::new("movie.mkv")));
        assert!(!lookup("mkv").unwrap().shell);
    }
}
