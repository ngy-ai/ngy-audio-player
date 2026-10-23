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

APE 不在列表里：ffmpeg 只有 APE 解码器、没有编码器，生成不出样本。想补的话，
从本机找一首 `.ape` 拷进来即可，测试会自动纳入。
