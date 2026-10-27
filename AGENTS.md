# AGENTS.md

本文件是 AI / 自动化 agent 在本仓库工作时的约定。用户当次指令优先于本文件，
本文件优先于通用习惯。

## 项目概览

「声波」是一个跨平台桌面音频播放器，Rust 2024 edition。

- 界面：`gpui-kit`（Zed 的 GPUI + gpui-component）。
- 音频内核：独立 crate `ngy-audio-engine`（`crates/audio`），**不依赖 GPUI**。
- 解码：Symphonia 内置解码优先，系统 `ffmpeg` 子进程兜底；选择后端靠实际试解，
  不看扩展名。
- 输出：cpal，统一成 2 声道交错 `f32`，必要时做 FFT 重采样。

## 目录结构

```
src/                    界面层：app、列表 / 封面 / 进度 / 传输控制 / 频谱
crates/audio/           音频内核：decode、resample、output、state、engine、meta
crates/audio/tests/     真实格式解码测试、真实设备播放测试
testdata/               测试样本，音频文件不入库（见 testdata/README.md）
scripts/shot.py         Windows 窗口截图 / 点击辅助脚本
docs/                   截图等文档资源
```

## 常用命令

```bash
cargo run --release                  # 空界面，拖文件进去
cargo run --release -- a.mp3 b.flac  # 直接播放
cargo build --release
cargo test --workspace
cargo test -p ngy-audio-engine
cargo test -p ngy-audio-engine --test formats -- --nocapture
cargo test -p ngy-audio-engine --test playback -- --nocapture
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

- Windows 需要 MSVC 工具链；Linux 需要 `libasound2-dev`。
- 格式测试依赖 `testdata/` 样本，样本生成命令见 `testdata/README.md`；
  样本缺失时会跳过。播放测试需要真实音频设备，无设备时自动跳过。
- 改音频内核后至少跑对应 crate 的测试；改格式支持后跑 `--test formats`
  并检查每个样本实际走的后端。

## 架构与实现约束

1. `crates/audio` 不得依赖 GPUI，必须能在无界面环境下测试和复用。
2. 解码结果统一为 2 声道交错 `f32`；采样率与设备不一致时在引擎内重采样。
3. 不按扩展名决定解码后端：先试 Symphonia，失败再落到 ffmpeg；错误的返回信息
   应同时包含两条路径的原因。
4. 音频回调里不加锁、不分配、不做 IO。UI 与音频线程之间通过 `state.rs` 的
   原子量和无锁环形缓冲通信；改动线程模型时保持这一点。
5. UI 只通过 `Player` 的公开接口操作播放，不直接碰线程、缓冲或解码器。
6. ffmpeg 是可选依赖：未安装时内置格式仍要能播，界面要明确提示。
7. 不要为了小功能引入新的原生依赖；优先使用现有库（Symphonia、cpal、rubato、
   lofty、ringbuf、rustfft、image、rfd）。

## 文件规模与拆分

单个源码文件不得超过约 **700 行**。大文件既难读也难改，工具读一次就吃掉上下文。

- 超过就按职责拆成子模块，不要继续往上堆：新增代码时如果目标文件已经接近这个上限，
  先拆再改；拆分提交和功能提交要分开 `git commit`。
- 现状提醒：`src/ui/player_view.rs`（约 3800 行）、`src/shell.rs`（约 800 行）已超限，
  下次改动就先拆。
- 拆分沿用现有布局（参考 `src/ui/mod.rs`、`crates/audio/src/decode/mod.rs`）：
  - `foo.rs` 拆成 `foo/mod.rs` 加若干 `foo/*.rs`；子模块文件名按职责命名，
    不叫 `utils.rs` / `helpers.rs` 这类含糊名字（如 `decode/symphonia_backend.rs`）。
  - `mod.rs` 只放模块文档、`mod` 声明和对外重新导出（`pub use`），不放实现；
    多处共用的类型单独成文件（`types.rs`、`config.rs`），避免模块间互相引用类型。
  - 一个职责一个模块，模块名 = 对外概念（`decode/`、`output/`、`ui/`）。
    子模块只依赖下层与共享基础设施，不反向 import 上层：`decode/*` 不得引用 `engine`；
    需要回调时用 `impl Fn` / trait 对象从上往下注入。
  - 同一职责的实现可以分散在多个文件，但对外只经 `mod.rs` 暴露；
    子模块默认私有（`mod x;`，不是 `pub mod x;`），需要对外时用 `pub use` 精确导出。
- 测试同样拆分，并与源码布局对应：
  - 单元测试就近放在被测模块内，用 `foo/tests.rs` 加 `#[cfg(test)] mod tests;`，
    不要把所有单测堆到一个巨型测试文件里。
  - 集成测试放 `crates/<crate>/tests/<主题>.rs`，一个文件一个主题，文件名与被测模块同名
    （现有 `tests/formats.rs`、`tests/playback.rs`）；每个文件是独立测试二进制，
    跨模块的端到端场景才留在 `tests/` 根目录。
- 自查超长文件：

  ```bash
  rg --files -g '*.rs' -g '!target' | xargs wc -l | sort -n | tail -20
  ```

## 代码风格

- 使用 rustfmt 默认格式；提交前跑 `cargo fmt --all`。
- 注释和文档用中文，重点解释「为什么」而不是复述代码；保持简短。
- `main` / `app` 入口用 `anyhow::Result`；底层错误添加上下文，面向用户的错误
  信息用中文。
- 生产路径避免 `unwrap` / `expect`；测试里可以用。
- 跟随现有模块划分与命名，不重构无关代码。

## Git 工作流（强制）

**每次 agent 处理完任务后，必须自动执行 `git commit`，不需要再次询问用户。**

具体要求：

1. 提交前检查 `git status --short` 和 `git diff`，确认改动范围。
2. 只暂存本次任务实际修改的文件，使用明确的路径（`git add path/to/file`）。
   禁止 `git add -A` / `git add .`，禁止顺手提交无关的未跟踪文件
   （例如 `.workbuddy/`、个人配置、临时文件）。
3. 不提交 `target/`、`testdata/` 里的音频样本、密钥或任何敏感信息。
4. 提交信息格式：`type: 中文简述`，一行说清改了什么。
   常用 type：`feat`、`fix`、`refactor`、`perf`、`test`、`docs`、`chore`。
   示例：`fix: 修复暂停后进度条继续前进`
5. 不要 amend、rebase、force push 或 push，除非用户明确要求。
6. 没有可提交的改动时，说明情况并跳过提交。
7. 只提交已验证的改动；测试无法运行时，在最终回复里说明原因，不要假装已验证。
8. 不要为了「提交干净」而回滚或覆盖用户的未提交改动。

## 交付说明

任务完成后向用户简要报告：改了哪些文件、跑了哪些检查、提交哈希。
