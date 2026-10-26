# 随包内置 LGPL 构建的 ffmpeg

内置解码器（Symphonia）只覆盖 12 种格式，Opus、WMA、APE、WavPack、TTA、DSD、
AC-3、DTS、AMR 全部依赖系统 ffmpeg。项目要公开分发，承诺装完即可播放，
这与「让用户自己去装 ffmpeg」直接冲突，所以三平台发布包内置一份 ffmpeg。

内置的是 **LGPL 构建**（不带 `--enable-gpl`），只保留音频 demuxer / decoder，
体积与授权都压到最小；调用方式仍是**子进程**（`-i <path> -f f32le pipe:1`），
不链接 libav*。找不到内置或系统 ffmpeg 时降级而不是拒绝启动：内置解码器照常工作，
「部分格式不可用」从顶部横幅降级成单曲级错误提示。

## 考虑过的选项

- **让用户自己装**：把「通用」缩水成「装了 ffmpeg 才通用」，等于放弃卖点。
- **用纯 Rust 解码器补齐**：Opus 需要 C 库，APE / WavPack / Musepack 没有成熟的
  纯 Rust crate，实际做不到。

## 后果

- 每个平台产物增大（音频子集约 20–40MB）；必须随包附 LGPL 文本与源码获取方式。
- macOS 上 ffmpeg 必须与应用一起签名，否则 hardened runtime 下起不了子进程；
  这也让「只给 macOS 买签名」从可选项变成必需项。
- 子进程的异常退出成为「播对」的一部分：现有 seek 就是杀进程重启，
  内置化之后 ffmpeg 崩溃会变成用户可见的 bug，需要按 bug 而不是按边缘情况对待。
- 用户自带的 ffmpeg 仍然优先（环境变量 `NGY_AUDIO_FFMPEG`、PATH），内置那份只是兜底。
