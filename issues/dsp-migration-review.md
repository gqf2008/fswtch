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
