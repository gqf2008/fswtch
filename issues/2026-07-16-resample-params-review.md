# 代码审查报告 — feat/resample-params

| 项 | 值 |
|---|---|
| 分支 | `feat/resample-params` |
| 审查提交 | `80a5e44` |
| 基线 | `master` (`e10af72`) |
| 审查日期 | 2026-07-16 |
| 改动范围 | `crates/fswtch/src/resample.rs`、`crates/fswtch/src/speex.rs` |
| 改动规模 | +84 / −2 |
| 性质 | 纯增补性 API，无破坏性变更 |
| 结论 | ✅ 建议合入（合并前有一个可选小修建议） |

## 一、改动摘要

SpeexResampler 此前仅存 opaque 句柄，创建后 `in_rate/out_rate/quality/channels` 全部丢失；Resample 同样丢失 `quality`（`switch_audio_resampler_t` 无该字段）与原始 `to_size`（C 结构体 `to_size` 会被 `switch_resample_process` 增长）。

- **SpeexResampler**：用 `Cell` 缓存 `channels/in_rate/out_rate/quality`，保持 `&self` setter 语义；新增 `channels()`/`in_rate()`/`out_rate()`/`quality()` 与 `set_quality()`（`speex_resampler_set_quality` 此前已绑定未用）；`set_rate`/`set_quality` 仅在返回 `0` 时同步缓存。
- **Resample**：缓存 `quality` 与创建时 `to_size`；新增 `quality()`/`to_size()`/`to_capacity()`，其中 `to_capacity()` 实时读 `(*raw).to_size`（当前容量，区别于原始请求）。

## 二、验证方法

| 检查 | 结果 |
|---|---|
| `cargo check -p fswtch` | 通过（仅环境噪音） |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy -p fswtch --lib` | 通过；残留告警均在 `session.rs`，非本 PR 引入 |
| `cargo doc -p fswtch --no-deps` | 通过；新增 `Self::` 链接均有效 |

对照上游 C 源码/头文件逐条核实提交论断：

1. **`switch_audio_resampler_t` 确无 `quality` 字段** — `fswtch-src/freeswitch/src/include/switch_resample.h:56-74` 结构体仅含 `from_rate/to_rate/factor/rfactor/to/to_len/to_size/channels`。缓存动机成立。
2. **`to_size` 会被 process 增长** — 该字段语义为 "the total size of the to buffer"，`switch_resample_process` 分配增长之。`to_size()`（创建快照）与 `to_capacity()`（实时容量）区分有意义。
3. **speexdsp 错误码为正值** — `/opt/homebrew/include/speex/speex_resampler.h:104-108` 定义 `RESAMPLER_ERR_SUCCESS=0`，错误为 `1..=5`（非负）。抓取上游 `resample.c` 确认 process 路径无任何 `return -` 负值语句。故本 PR 的 `status == 0` 判定成功**完全正确**。
4. **`speex_resampler_set_quality` "已绑定未用"** — `build.rs` 的 `allowlist_function("speex_.*")` 使其在 `bindings.rs:17777` 生成，但 master `speex.rs` 无 `set_quality` 包装。现已补齐。
5. **线程安全未退化** — `SpeexResampler` 新增 `Cell<u32/i32>`，叠加既有 `PhantomData<*const ()>`，仍为 `!Send + !Sync`，与模块文档声明一致；`Resample` 的纯 `i32/u32` 字段不改变其 `!Send + !Sync`。

## 三、正确性分析

- **`set_rate`/`set_quality`**：仅在 `status == 0`（成功）时回写缓存，与 speexdsp `0=success` 约定吻合。失败时不污染缓存，避免缓存与底层态发散。
- **`SpeexResampler` 缓存不会发散**：speex 状态 opaque，无 `speex_resampler_get_*`/`ctl` 包装泄出；rates/quality 的唯一变更路径即 `set_rate`/`set_quality`，均同步回写。
- **`Resample::quality()/to_size()`**：FreeSWITCH 无 set_quality、本类型也无对应 setter，构造后不可变，用普通字段而非 `Cell` 正确。
- **`Resample::to_capacity()`**：实时读 `(*raw).to_size`，与既有 `from_rate()/to_rate()/channels()` 实时读模式一致，普通 uint 读安全；`!Sync` 保证无跨线程并发读。
- **命名差异是有意且正确的**：`Resample` 用 `from_rate/to_rate`（贴合 FreeSWITCH 术语），`SpeexResampler` 用 `in_rate/out_rate`（贴合 speex 术语）。

## 四、发现的问题

### 🟡 [既有 / 紧邻改动 / 建议] `process_int` 文档将 speexdsp 错误码误述为"负值"

- 位置：`crates/fswtch/src/speex.rs:255-256`（**非本 PR 引入**）
- 现状：文档写 "the speexdsp status code (0 = success, **negative = error**)"
- 事实：speexdsp 错误码为**正值**（`1..=5`），上游 `resample.c` 的 process 路径无任何负值返回。
- 影响：不构成实际 bug（`process_int` 仅原样返回 status 元组，不做 `< 0` 判定）；但若调用方照文档写 `if status < 0`，将永远检测不到错误。
- 建议：顺手改为 "0 = success, non-zero = error"，与本 PR 新增的 `set_quality`/`set_rate` 文档（已正确写 "Returns 0 on success"）保持一致。约 2 词改动。
- 阻塞性：否。

## 五、可选优化（非阻塞）

- **范围校验**：`set_quality`/`set_rate` 不做范围校验直接透传给 speex，依赖其返回 `RESAMPLER_ERR_INVALID_ARG`——与既有 `new()`/`set_rate` 一致，缓存亦不会在失败时更新。可接受；若求稳妥可加 `debug_assert!(quality >= 0 && quality <= 10)`。
- **测试覆盖**：缓存同步逻辑（成功才回写）无单测。既有 resample 测试门控 `#[cfg(all(test, feature = "live_fs"))]`、speex 无测试，受 live_fs 限制难以单测。逻辑简单且已核对正确，可接受。

## 六、建议

直接合入即可。若愿意顺手修掉第四节那条 `process_int` 文档（`negative` → `non-zero`），该区域的状态码语义将更干净。
