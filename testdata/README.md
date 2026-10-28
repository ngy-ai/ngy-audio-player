# 测试样本

这些文件不入库（见 `.gitignore`），用 ffmpeg 现场生成即可：

```bash
mkdir -p testdata && cd testdata
SRC="-f lavfi -i sine=frequency=440:duration=1.2:sample_rate=44100 -ac 2 -y -loglevel error"
ffmpeg $SRC -c:a pcm_s16le                       tone.wav
ffmpeg $SRC -c:a libmp3lame -b:a 128k            tone.mp3
ffmpeg $SRC -c:a flac                            tone.flac
ffmpeg $SRC -c:a libvorbis -q:a 4                tone.ogg
ffmpeg $SRC -c:a libopus -b:a 96k                tone.opus
ffmpeg $SRC -c:a aac -b:a 128k                   tone.m4a
ffmpeg $SRC -c:a alac                            tone_alac.m4a
ffmpeg $SRC -c:a aac -f adts                     tone.aac
ffmpeg $SRC -c:a wmav2 -b:a 128k                 tone.wma
ffmpeg $SRC -c:a wavpack                         tone.wv
ffmpeg $SRC -c:a pcm_s16be                       tone.aiff
ffmpeg $SRC -c:a pcm_s16le -f caf                tone.caf
ffmpeg $SRC -c:a libvorbis -f matroska           tone.mka
ffmpeg $SRC -c:a ac3 -b:a 192k                   tone.ac3
ffmpeg $SRC -c:a libspeex                        tone.spx
ffmpeg $SRC -c:a libopencore_amrnb -ar 8000 -ac 1 tone.amr
ffmpeg $SRC -c:a tta                             tone.tta
ffmpeg $SRC -c:a pcm_s16be -f au                 tone.au
ffmpeg $SRC -c:a pcm_s16le -f w64                tone.w64
ffmpeg $SRC -c:a aac -ar 44100                   tone.3gp
```

然后跑：

```bash
cargo test -p ngy-audio-engine --test formats -- --nocapture
```

它会打印每个文件走的是 `Symphonia` 还是 `FFmpeg`，并检查解出来的时长是否合理。

APE 另走一条路：ffmpeg 只有解码器、没有编码器，生成不出来，用 ffmpeg 自己的测试文件：

```bash
curl -O https://samples.ffmpeg.org/A-codecs/lossless/luckynight.ape
```

60 秒、44.1 kHz 立体声，正好满足 `tone.*` 之外「时长 ≥ 30 秒」那条规则。它同时是
「内置解码器必须让路」的回归样本：APE 的描述区里抄着一份原始 WAV 头，探测器只认
文件头，才不会把这个文件当成 WAV。

本机要是已经有别的 `.ape` 曲目，拷进来也行，只要长于 30 秒。
