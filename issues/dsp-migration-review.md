# 代码审查报告：`feat/dsp-migration`

- **分支**: `feat/dsp-migration`
- **审查提交**: `da0ef38` — `feat(fswtch): 迁移 audio-dsp 纯 Rust DSP 模块（resample/rms/agc/denoise）`
- **审查日期**: 2026-07-16
- **变更规模**: +843 行（8 文件：Cargo.lock / Cargo.toml / dsp/* / lib.rs）
- **结论**: 建议合入（approve with nits）

从 `vox-seat/crates/audio-dsp` 迁移 4 个纯 Rust DSP 源文件为 `fswtch::dsp` 子模块：`params` / `util` / `agc` / `denoise`。适配 fswtch 惯用法（`crate::log_error` 替代 `tracing::error!`、`Result<_, String>` 替代 `anyhow::Result`、丢弃 tokio/parking_lot/crossbeam/ringbuf 死依赖）。

## 验证结果

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | ✅ 通过 |
| `cargo check -p fswtch` | ✅ 通过（仅 pkg-config / 硬链接 噪声警告） |
| `cargo check -p fswtch --tests` | ✅ 通过（16 个 dsp 测试类型正确） |
| `cargo clippy`（dsp 模块本身） | ✅ 干净，无任何 dsp 命中 |
| `cargo clippy --workspace --all-targets` | ⚠️ 见下「既有问题」 |
| `cargo test` | ⚠️ 本机无法链接（无 FreeSWITCH 安装）——见下 |

## 主要发现

### 1. [设计不一致] dsp 自称「无 FFI 耦合」，但 `util.rs:97` 调用 `crate::log_error`

模块文档（`dsp/mod.rs:18`）写「pure-Rust algorithms with no FreeSWITCH FFI coupling」，但 `SampleRateConverter::process` 的 rubato 错误路径调了 `crate::log_error`，这会传递性拉入 `switch_log_printf`（`logging.rs`）。grep 确认这是 dsp **唯一**的 FFI 耦合点。

**后果**：dsp 的 16 个单测在默认 `bundled`（headers-only）构建下**无法链接**——报错 `Undefined symbol _switch_log_printf`。提交信息声称「16 个 inline test 全部通过」只在装了真实 FreeSWITCH（`FREESWITCH_LIB_DIR` 或 pkg-config）的环境下成立，fresh checkout 跑不起来。

> 说明：这**不是**相对 master 的回归——master 的 lib-test 在 bundled 模式下本来就链接不过（`channel.rs`/`codec.rs`/`media.rs` 等都调 FFI，正是 `live_fs` feature 注释描述的场景）。但作为「纯 Rust 无 FFI」模块，这一行 `log_error` 耦合让它和整条 crate 的链接依赖绑死，与其文档定位自相矛盾。若改成 `eprintln!` / `tracing` / log trait，dsp 测试即可脱离 FS 独立运行，「16 测试通过」就能在任何环境复现。
>
> **建议**：把这一处 log 改为非 FFI 通道，或在文档里如实标注「需 live_fs」。

### 2. [文档] `agc.rs:10` 模块文档句子被截断

```text
//! Design: drive each frame's RMS toward `target_rms` via a one-pole-smoothed
                                                                  ↑ 句子到此中断
/// One-pole smoothing coefficient for gain INCREASE (attack). Small = slow.
```

模块级 doc 在 "one-pole-smoothed" 处戛然而止，下一行直接是 item doc。补全后半句即可。

### 3. [测试质量] `util.rs:264` 硬编码性能断言易 flaky

`rms_benchmark` 用 `assert!(per_call_us < 15.0, ...)` 给单测定阈值。性能断言在 CI / 负载波动 / 异构 runner 上天然 flaky——慢机器会因非功能原因挂掉整个 build。建议加 `#[ignore]`（手动 `cargo bench` 式运行）或删断言只留 `println!`。

## 次要 nit

- **[util.rs:71 vs :89]** i16↔f32 标度不对称：输入 `/ 32768.0`，输出 `* 32767.0`。常用、误差 ~0.003%，但两侧统一更干净。
- **[util.rs:73, 80]** `chunk_size = self.inner.input_frames_next()` 在 process 开头取一次后整个循环复用。当前用的是 `FixedAsync::Input`（输入帧数契约固定），没问题；但这是**隐含假设**——日后若改非固定 async 模式，固定 `chunk_size` 会喂错 rubato，落到 `Err(_) => continue`（util.rs:82-83, 96-101）**静默丢采样**。这两条静默丢样路径建议补一句注释说明降级行为。
- **[agc.rs:48 vs util.rs:148]** AGC 自己用 `f64` + `i64` 内联算 RMS（精度更高），未复用 `dsp::rms`（`f32` 累加）。合理（精度需求不同），但有重复——可加一行注释说明为何不复用。

## 既有问题（非本分支引入，但影响工作流）

`cargo clippy --workspace --all-targets` / `cargo test --workspace` 在 example 阶段失败：

```text
error[E0425]: cannot find function `attach_media_bug` in crate `fswtch`
  crates/fswtch-apm/examples/mod_apm.rs:201
  crates/fswtch-apm/examples/mod_aec3.rs:178
```

已核实：**本分支完全没碰 `fswtch-apm/`**（`git diff master..feat/dsp-migration -- crates/fswtch-apm/` 为空，两个 example 文件与 master 逐字节相同）。根因是更早的 `bda318a refactor(fswtch): 自由函数→impl 方法` 把 `attach_media_bug` 改成了 `Session` 方法（`media.rs:769`），但没更新这两个 example 的调用点。建议单独开个 fix 提交把 `fswtch::attach_media_bug(session, …)` 改成 `session.attach_media_bug(…)`——否则 AGENTS.md 规定的 `cargo clippy --workspace --all-targets` 永远红。

## 优点

- **设计决策扎实**：`pub mod dsp;` 不做 glob re-export，刻意避免与已 re-export 到 crate 根的 `resample::Agc`（FSW C 句柄）撞名——commit message 说得很清楚，正确。
- **文档质量高**：`agc.rs` 解释了为什么不用 speex AGC（AEC-Challenge 上增益泵浦把输出压成静默，附 SI-SNR/ERRE/STOI 数值）——这种「为什么」非常宝贵。
- **AGC 算法正确**：慢 attack / 快 release（`ATTACK_COEF=0.02` / `RELEASE_COEF=0.25`）、gain 双向 clamp、`f64` 累加 `sum_sq`；`lifts_quiet_speech`/`attenuates_loud_fast` 测试均验证了收敛与封顶行为，数学上自洽（400 帧 0.02 系数 → gain≈9.997，封顶 10×）。
- **denoise 缓冲正确**：480 样本帧缓冲、`Vec::append` 清空 out_buf、`reset` 仅在 enabled 时重置。feature-gate（`denoise` 默认开，可 `--no-default-features` 关）干净。
- **测试有意义**：rms SIMD-vs-scalar 一致性、flush 尾部不截断、flush 幂等、AGC 三态——都是真行为验证，非空壳测试。
- **依赖最小且全部使用**：仅 `rubato`/`wide`/`nnnoiseless`，Cargo.lock 新增全是预期传递依赖（audioadapter、safe_arch、realfft、anymap3…），无删除、无意外重依赖。

## 结论

这是一次干净、文档良好的迁移，迁移惯例适配（`log_error` / `Result<_, String>` / 去死依赖）到位，算法与测试自洽。

非阻塞建议（按性价比排序）：

1. 补全 `agc.rs:10` 截断的文档句。
2. `rms_benchmark` 改 `#[ignore]` 或删阈值。
3. 重新评估 dsp 里那处 `crate::log_error`——要么改非 FFI 通道让 dsp 测试真能在 bundled 下独立跑，要么文档如实标注「需 live_fs」。
4. （独立提交）修 `fswtch-apm` 两个 example 的 `attach_media_bug` 调用点，让 `cargo clippy --workspace --all-targets` 转绿。

---

## 再审（2026-07-16，分支已前进至 `c69c85f`）

初审报告后，作者落了两个提交：`94aa46c fix(dsp): 审查反馈修复` 与 `c69c85f fix(fswtch-apm): 修复 example attach_media_bug 调用点`。本次复审验证修复正确性，并把上轮当 nit 处理、实测为真实 bug 的两条提级。

### 初审 4 条修复 — 全部正确落地

| # | 修复 | 验证 |
|---|---|---|
| 1 | dsp FFI 解耦：`crate::log_error` → `eprintln!` | ✅ grep 确认 dsp 源码零 `crate::`/`sys::` 代码引用（仅余 markdown 链接文本与注释）。链接报错里 `_switch_log_printf` 已消失，dsp 模块文档「no FreeSWITCH FFI coupling」名副其实。 |
| 2 | `agc.rs:10` 文档句补全 | ✅ 补为「…via a one-pole-smoothed gain coefficient — slow attack …; fast release …」。 |
| 3 | `rms_benchmark` 加 `#[ignore]` | ✅ 带说明字符串，`cargo test` 显示 `ignored`，CI 不再因定时不达标挂。 |
| 4 | fswtch-apm 两个 example 的 `attach_media_bug` 调用点 | ✅ `fswtch::attach_media_bug(session, …)` → `session.attach_media_bug(…)`，与 `mod_media_bug_meter.rs:113` 对齐。 |

附带 nit 注释（i16↔f32 不对称、`FixedAsync::Input` chunk 假设、AGC 用 f64 内联 RMS）也都补了。

### 门禁复跑

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets` ✅ **现在 Finished（examples 编译通过，修复 #4 见效）**；仅余 `ai-agent-seat` 既有 warning。
- 把 dsp 四文件抽到独立 crate（dsp 现已 FFI-free，可独立链接），**16 个内联测试 15 跑 1 ignored 全过**——实证 commit message「16 测试通过」属实（前提是脱离整条 crate 的 FFI 链接）。
- `cargo test -p fswtch --lib` 在 bundled 模式仍链接失败，但残留 undefined symbol 已从「`_switch_log_printf` + `_switch_channel_cause2str`」收敛到只剩 `_switch_channel_cause2str`（来自 `status::Cause::as_str`，由 channel 等其他模块测试拉入）。dsp 自身不再是链接拖累，此为**既有平台限制**（`live_fs` 文档已说明），非本分支回归。

### 上轮当 nit、复审实测为真实 bug 的两条（探针测试实证 confirmed）

探针测试置于 `/tmp/dsp-probe/tests/review_probes.rs`，两条均通过——即确认问题真实存在。

#### A. `dsp::Agc` 静默期增益爬升 → 语音起始爆裂

`agc_gain_creeps_to_max_during_silence_then_saturates_speech` 通过：100 帧静默（≈1s）后 `agc.gain() > 5.0`（向 max_gain=10 爬）；紧接一帧 RMS 5000 的正常语音，`5000 × ~8.8 = 44000` 被 clamp 到 `i16::MAX`，**160 个采样里 >100 个饱和到 32767**——语音起始处削顶失真。

根因：静默时 `rms=0` → `desired = target_rms/EPS → max_gain`，慢 attack 让增益一路爬到天花板；语音一来被顶格放大。原 `unity_on_silence` 测试只跑 1 帧、只验「输出仍为 0」（瞬时确实为 0），**抓不到稳态爬升**——测试盲区。

> 设计语境：`dsp/mod.rs` 写明原 pipeline 在 AGC 上游有「far-field gate」，静默/噪声在到 AGC 前就被门控。但「该 pipeline is now dead」——`dsp::Agc` 作为独立库 API 暴露时上游没有 gate，静默爬升是未文档化的坑，与 speex AGC 被否的同款「gain pumping」同族。
>
> 建议（任一）：① 文档显式标注「需上游 noise gate / hold」；② 给 `Agc` 加 silence-hold（rms < 阈值时冻结增益）；③ 补「长静默→语音」回归测试。

#### B. `DenoiseStage::process` 的 `out` 清空契约文档与代码不符

`denoise_out_is_not_cleared_at_start_of_next_call` 通过：同一个 `out: Vec` 跨调用复用、不 drain，喂两帧 480 → `out.len() = 960`（累加），而非文档声称的 480。

`denoise.rs:56-57` 文档写「The caller should drain `out` after each call — **it is cleared at the start of the next call**」，但代码只 `out.append(&mut self.out_buf)` / `out.extend_from_slice(input)`，**从不 clear 调用方的 `out`**。依赖该契约的调用方会无限增长。

> 建议：要么删掉文档「it is cleared at the start of the next call」、改成「caller must clear/drain `out`」；要么在 `process` 开头 `out.clear()`（契约自洽，但需确认无调用方依赖累加语义）。

### 观察（重申，非阻塞）

- **dsp 全模块在 workspace 内零消费者**：`ai-agent-seat` 用的是它自己的 `crate::audio_dsp`（且 `PIPELINE_SAMPLE_RATE = 8000`）+ FSW C 句柄 `fswtch::Resample`，不是 `fswtch::dsp::*`。对 library crate 可接受，但 commit message「自研 AGC 替代 speex AGC」目前是**愿景不是事实**——线上跑的还是 speex。把 A 修了再接线才稳。

### 再审结论

**Approve。** 四条修复全部正确，门禁转绿，dsp 实证 FFI-free、16 测试在隔离环境全过。

但复审把上轮两条 nit **提级为真实 bug 并已实证**：

1. **AGC 静默爬升 → 语音起始削顶**（算法 + 测试盲区，speex-同款 pumping）；
2. **DenoiseStage `out` 清空契约文档/代码不符**。

二者都小、都好修，建议合入前一并处理——尤其若近期要把 `dsp::Agc`/`DenoiseStage` 接进实活 pipeline（其上游 far-field gate 已被删，A 会直接咬人）。
