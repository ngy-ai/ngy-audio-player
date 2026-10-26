//! 格式表：文件过滤器、显示标签、以及「内置解码器能不能吃下」的预期。
//!
//! 这里的 `builtin` 只是**预期**，不是判据 —— 实际能否解码永远以「试一次」为准。
//! 例如 `.ogg` 既可能是 Vorbis 也可能是 Opus，前者 Symphonia 能解、后者不能，
//! 只看扩展名会判错。所以 `decode::open` 的策略是：先试 Symphonia，失败再走 ffmpeg。

/// 一个扩展名的元信息。
#[derive(Clone, Copy, Debug)]
pub struct FormatInfo {
    /// 显示用的格式名，例如 `FLAC`。
    pub label: &'static str,
    /// Symphonia 是否**通常**能解。
    pub builtin: bool,
}

/// 查扩展名（不含点，大小写不敏感）。未知扩展名返回 `None`。
pub fn lookup(ext: &str) -> Option<FormatInfo> {
    let ext = ext.to_ascii_lowercase();
    let (label, builtin) = match ext.as_str() {
        // —— 内置解码器覆盖的容器 / 编码 ——
        "mp3" | "mp2" | "mp1" | "mpa" => ("MPEG Audio", true),
        "flac" => ("FLAC", true),
        "wav" | "wave" => ("WAV", true),
        "aiff" | "aif" | "aifc" => ("AIFF", true),
        "caf" => ("CAF", true),
        "m4a" | "m4b" | "m4r" | "m4p" | "mp4" | "alac" => ("MPEG-4 Audio", true),
        "aac" | "adts" => ("AAC", true),
        "ogg" | "oga" => ("Ogg", true),
        "mka" | "mkv" | "webm" => ("Matroska", true),

        // —— 需要 ffmpeg ——
        "opus" => ("Opus", false),
        "spx" => ("Speex", false),
        "wma" | "asf" => ("Windows Media Audio", false),
        "ape" => ("Monkey's Audio", false),
        "wv" => ("WavPack", false),
        "tta" => ("TTA", false),
        "tak" => ("TAK", false),
        "shn" => ("Shorten", false),
        "mpc" => ("Musepack", false),
        "dsf" => ("DSD (DSF)", false),
        "dff" => ("DSD (DFF)", false),
        "amr" | "awb" => ("AMR", false),
        "ac3" | "eac3" | "ec3" => ("Dolby AC-3", false),
        "dts" | "dtshd" => ("DTS", false),
        "ra" | "rm" | "rmvb" => ("RealAudio", false),
        "au" | "snd" => ("Sun AU", false),
        "voc" => ("Creative VOC", false),
        "w64" => ("Sony Wave64", false),
        "paf" => ("Paris Audio", false),
        "svx" => ("8SVX", false),
        "ircam" | "sf" => ("IRCAM", false),
        "mod" | "xm" | "s3m" | "it" | "mtm" | "umx" => ("Tracker Module", false),
        "gme" | "nsf" | "spc" | "vgm" | "vgz" | "ay" | "gbs" | "hes" | "kss" | "sap" => {
            ("Game Music", false)
        }
        // 视频容器也常带音轨：能放出声音，只是不画画面。
        "avi" | "mov" | "flv" | "wmv" | "ts" | "m2ts" | "mpg" | "mpeg" | "3gp" | "3g2" | "ogv"
        | "vob" => ("Video Container", false),
        _ => return None,
    };
    Some(FormatInfo { label, builtin })
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
pub fn dialog_extensions() -> &'static [&'static str] {
    &[
        "mp3", "mp2", "mp1", "mpa", "flac", "wav", "wave", "aiff", "aif", "aifc", "caf", "m4a",
        "m4b", "m4r", "mp4", "alac", "aac", "adts", "ogg", "oga", "mka", "mkv", "webm", "opus",
        "spx", "wma", "asf", "ape", "wv", "tta", "tak", "shn", "mpc", "dsf", "dff", "amr", "awb",
        "ac3", "eac3", "dts", "dtshd", "ra", "rm", "au", "snd", "voc", "w64", "paf", "svx", "mod",
        "xm", "s3m", "it", "umx", "nsf", "spc", "vgm", "vgz", "gbs", "avi", "mov", "flv", "wmv",
        "ts", "m2ts", "mpg", "mpeg", "3gp", "3g2", "ogv",
    ]
}
