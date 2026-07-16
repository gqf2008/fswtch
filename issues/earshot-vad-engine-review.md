# 审查报告:earshot VAD 引擎集成 + `VadEngine` 枚举选择

- **分支**:`feat/earshot-vad-engine`
- **提交**:`5eef9c3` — `feat(fswtch): 集成 earshot VAD 引擎 + VadEngine 枚举选择`
- **审查日期**:2026-07-16
- **审查范围**:`crates/fswtch/src/vad.rs`(主体)、`status.rs`、`lib.rs`、`Cargo.toml`、`Cargo.lock`
- **结论**:**可以合并**。实现扎实、文档详尽、safety 注释到位;提交信息声称的验证全部复现无误。下列发现均非阻断。

---

## 验证结果(全新 worktree 检出后实跑)

| 检查 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all --check` | ✅ 通过 |
| 编译(默认 bundled,无 FS 链接) | `cargo check -p fswtch` | ✅ 通过(仅 pkg-config 找不到 FS 的预期 warning) |
| Lint | `cargo clippy -p fswtch --all-targets` | ✅ exit 0;`vad.rs` **0 warning** |
| 单测 | `cargo test --lib -p fswtch` | ✅ **65 passed; 0 failed** |
| earshot crate API 用法 | 对照 `earshot 1.1.0` 源码 | ✅ `predict_i16(&mut self, &[i16]) -> f32` 需 256 样本、返回 `[0,1]`、0.5 默认阈值;vad.rs 恒以 256 帧喂入 |
| `&self`→`&mut self` 破坏性变更 | `grep` 全仓库调用方 | ✅ 仓库内无调用方,pre-1.0,安全 |
| `EarshotInner` 纯 Rust 可单测 | 默认 build 跑通 | ✅ `earshot_hysteresis_start_and_stop` / `earshot_silence_is_none` 在无 FS 时通过 |

剩余 clippy warning(`items_after_test_module` ×8)全在 `speech.rs` / `rtp.rs`,与本分支无关,属既有问题。

`status.rs` 把 `cause_as_str_known` 改为 `live_fs` 门控是正确的——它确调 `switch_channel_cause2str`,默认 build 不该链它。

---

## 发现(按重要性排序,均非阻断)

### 1.(设计,最实质)共享方法体把 earshot 路径耦合到 FS 符号链接

`with_engine` / `process` / `reset` / `state` / `set_mode` / `set_param` / `Drop` 每个函数都把两个 engine 的 arm 写在**同一函数体**内,因此 FreeSwitch arm 里的 `sys::switch_vad_init` / `switch_vad_process` 等 extern 符号,会随该方法被引用而进入链接。

**后果**:earshot 路径自身不碰 FS,但只要走 `Vad` 这层 API,就会拖入 FS 符号依赖。这正是 `earshot_vad_speech_segments`(16 kHz,本身零 FS 符号)也不得不挂在 `live_fs` 后的原因——不是 earshot 需要 FS,而是 `with_engine` 引用了 `switch_vad_init`。

提交信息里“EarshotInner 纯 Rust、默认可单测”准确,但那只对**直接测 `EarshotInner`** 成立(当前 `step` / `process_16k` 两个单测确实这么做);走 `Vad` 这一层的 earshot 分派在默认 build 里永远测不到。

**建议**:若想让该路径在无 FS 时也能覆盖,把每个 FreeSwitch FFI 调用拆成独立 `fn`(或 `#[cfg]` 该 arm),让 earshot 路径不引用 `switch_*` 符号即可。非合并必需。

### 2.(覆盖缺口,承接 #1)`Vad` 层 earshot 分派在默认 build 无覆盖

当前默认 build 跑的 earshot 测试只有直调 `EarshotInner` 的两个。`Vad::process` 的 earshot arm、`channels > 1` 的 downmix、以及 earshot 下 `coarse_segments` → `snap_segments` 的整条管线,都只在 `live_fs` 下测。神经网络 `predict_i16` 倒是间接被 `earshot_silence_is_none` 覆盖了(喂静音、断言 NONE)。

### 3.(行为,建议在真语音上验证)onset 累加器在任一非语音帧完全清零

`step` 在 onset 阶段遇到一个 `score < threshold` 的帧就把 `onset_accum = 0`。真实神经 VAD 分数在语中常瞬时跌破阈值,`voice_ms` 窗口内一个 16 ms 的下探就会丢掉全部累加,使 `START_TALKING` 比原生 FS 的 hangover 更敏感/更易迟触发。注释已写明“simple, predictable hysteresis”且可配,但**用于 barge-in / 轮次检测前建议拿真实语音片段验证**。

### 4.(未验证,需作者在有 FS 的环境确认)earshot 语音检测测试用 220 Hz 正弦纯音

`earshot_vad_speech_segments` / `earshot_vad_resampled_8k`(均在 `live_fs` 门控)用 `synthetic(.., 220 Hz, amp 12000)` 纯音,只断言 `!segs.is_empty()` / 有非 NONE 状态。earshot 是按真实语音训练的神经网络,对纯音是否响应不确定。**本地无 FS 跑不了这两个,请作者在真实 FS build 上确认它们确实通过**;若不稳,换成多谐波 + 噪声调制的类语音夹具更可靠。

### 5.(小)threshold 双控制、两种量纲

`set_mode` 直接设 `0.50 / 0.55 / 0.60 / 0.65`,而 `set_param("threshold", v)` 按“千分位”解释(`500` → `0.5`)。同一字段两套刻度,混用易踩坑(如 `set_param("threshold", 2)` → `0.002`)。文档已分别说明,建议补一句二者关系或统一刻度。

### 6.(小)`earshot = "1"` 为无条件依赖

纯 Rust、无 C/链接依赖,不影响 bundled(headers-only)build——提交信息属实。但任何只用 `VadEngine::FreeSwitch` 的消费者也会编译 earshot。鉴于 earshot 轻量,影响有限;若在意发布时的默认依赖面,可考虑 `vad-earshot` feature 门控。

### 7.(小)earshot 路径下 `process` 对入参 buffer 的写行为不对称

仅非 16 kHz(走 resampler)路径会就地改 `pcm`;16 kHz/mono 路径实际只读。`&mut [i16]` 签名已覆盖,但 per-engine 文档可提一句这种不对称。纯文档层面。

### 8.(Nit,MSRV)`usize::is_multiple_of`(vad.rs:630)需 Rust ≥ 1.87

workspace 未声明 `rust-version`,当前 1.96.1 无碍;若要对 crates.io 宣称更宽 MSRV,要么写 `rust-version = "1.87"`,要么改回 `len % frame == 0`。

---

## 值得肯定的点

- `EarshotInner` 用 `Box` 装入 enum 变体以避开 `clippy::large_enum_variant`,并在注释里写明原因——细节到位。
- `earshot_hysteresis_start_and_stop` 直调 `step(score)` 用固定 0.9 / 0.1 分数,绕开神经网络非确定性来测滞回逻辑——测试设计很聪明。
- `Vad` 加 `PhantomData<*const ()>` 标 `!Send / !Sync` 并文档化,符合 FS 媒体线程模型。
- safety 注释逐处给出 `raw` 存活、缓冲区有效性的合约理由,符合 `AGENTS.md` 的 unsafe 规约(`unsafe_op_in_unsafe_fn = deny`、`missing_safety_doc = deny`)。

---

## 附:复现命令

```sh
git worktree add /tmp/fswtch-review feat/earshot-vad-engine
cd /tmp/fswtch-review
cargo fmt --all --check
cargo check -p fswtch
cargo clippy -p fswtch --all-targets
cargo test --lib -p fswtch        # 65 passed; 0 failed
```
