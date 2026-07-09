# 补齐 FFI 缺口封装:core 运行时计数 + `switch_api_execute`

## 背景与动机

`voice-call` 仓库的 FreeSWITCH 原生模块为调用若干 `switch_core_*` / `switch_api_execute`
函数,手写了整块 `unsafe extern "C"` FFI 声明,并手搓了 `SWITCH_STANDARD_STREAM` 宏。
这些做法在 fswtch 层面是不必要的重复,且其中一处存在 soundness 隐患。

关键事实(已核查):

- 这些函数**早就存在于 `fswtch-sys` 生成的 bindings 里**了 —— `crates/fswtch-sys/build.rs`
  的 `.allowlist_function("switch_.*")` 已自动收入:
  - `switch_api_execute`、`switch_console_stream_write`、`switch_console_stream_raw_write`
  - `switch_core_session_count`、`switch_core_uptime`、`switch_core_sessions_per_second`
- 它们只是缺一层 **fswtch 的高层 safe 封装**,导致业务侧不得不自己 `unsafe`。
- `fswtch::console::execute` **已经存在**,且完整正确地复刻了 `SWITCH_STANDARD_STREAM`
  (设了 `end` / `data_size` / `alloc_len` / `alloc_chunk` / `raw_write_function`)。
  业务侧(`mod_cc_ai::fs_api`)却没用它,自己手搓了一个**漏初始化 `end` 与 `alloc_len`**
  的版本,构成 voice-call 那边认定的 soundness 隐患(依赖"目标 FS 版本的 write 路径不碰 `end`",
  换版本即可能 NULL 解引用)。

目标:补两个小封装,让业务侧删掉手写 FFI 块 + 手搓 stream,把 unsafe 收回 fswtch 内部。

---

## 缺口清单

| 缺口 | 现状 | 必要性 |
|---|---|---|
| `switch_core_session_count` / `uptime` / `sessions_per_second` 的高层封装 | `core.rs` 未封(同类 `get_uuid` / `get_hostname` 已封) | **必加** —— 业务侧无替代品 |
| `switch_api_execute` 的高层封装 | `console::execute` 已覆盖"无 session / 单命令行"场景;仅当需要 cmd/arg 分离 + session 上下文时才缺 | **可选** |

> 说明:`switch_console_execute`(现 `console::execute` 用的)接受整条命令行并内部
> tokenize;`switch_api_execute` 接受分离的 `cmd` + `arg` + 一个 session。多数场景
> (如 `mod_cc_ai` 现传 `null_mut()` session)可直接迁到 `console::execute`,不需要
> Patch B。Patch B 仅为需要 session 上下文的 API 命令而设。

---

## Patch A(必加)— `core.rs` 三个运行时 introspection 函数

### 落点

`crates/fswtch/src/core.rs`,接在 `get_switchname()` 之后、文件末尾 `NOTE` 注释之前
(即当前第 102 行 `}` 与第 104 行 `// NOTE:` 之间)。

### 代码

```rust
/// The number of currently active channels in the core.
///
/// Wraps `switch_core_session_count`. Thread-safe — it reads an atomic core counter.
pub fn session_count() -> u32 {
    // SAFETY: No arguments; reads a process-global counter.
    unsafe { sys::switch_core_session_count() }
}

/// FreeSWITCH process uptime. The unit follows the upstream `switch_time_t` semantics of
/// `switch_core_uptime` (seconds on current FreeSWITCH releases).
pub fn uptime() -> i64 {
    // SAFETY: No arguments; returns a `switch_time_t` (a 64-bit integer).
    unsafe { sys::switch_core_uptime() as i64 }
}

/// Reads or sets the sessions-per-second limit. Pass `0` to read the current value without
/// modifying it; pass a nonzero limit to apply it, returning the previous value.
///
/// Wraps `switch_core_sessions_per_second`. Thread-safe.
pub fn sessions_per_second(limit: u32) -> u32 {
    // SAFETY: `limit` is a plain `u32`; the function reads/updates a core counter.
    unsafe { sys::switch_core_sessions_per_second(limit) }
}
```

### `lib.rs` re-export 更新

`crates/fswtch/src/lib.rs` 中(约第 53 行):

```rust
// 现状:
pub use core::{get_domain, get_hostname, get_switchname, get_uuid, get_variable, set_variable};
// 改为:
pub use core::{
    get_domain, get_hostname, get_switchname, get_uuid, get_variable, session_count,
    sessions_per_second, set_variable, uptime,
};
```

### 备注

- 这三个是 process-global、无参、读原子计数,与 `get_uuid` / `get_hostname` 完全同类,
  放 `core` 模块最贴切。
- `switch_core_uptime()` 在 sys 里返回 `switch_time_t`(不是 `c_long`)。业务侧
  (`mod_fs_metrics`)手写声明写成 `-> c_long`,64 位上恰好同 size 没出事,但类型错误。
  本封装用 `as i64` 忠实返回,文档注明单位由上游 `switch_time_t` 语义决定。

---

## Patch B(可选)— `console.rs` 加 `execute_api`,覆盖 `switch_api_execute`

### 落点

`crates/fswtch/src/console.rs`,接在 `execute()` 之后(即当前第 101 行 `}` 之后、
`expand_alias` 之前)。

### 代码

```rust
/// Runs a FreeSWITCH API command via `switch_api_execute`, with the command name and argument
/// passed separately and an optional session for command context.
///
/// Unlike [`execute`] (which drives `switch_console_execute` over a single combined command
/// line), this mirrors the `fs_cli` `cmd arg` split: some API commands behave differently when
/// the name and argument are separated, and a subset rely on a live session being attached.
/// Pass `None` for `session` when no session context is needed.
///
/// A private `switch_stream_handle_t` is constructed inline (mirroring `SWITCH_STANDARD_STREAM`),
/// the command is executed against it, and the accumulated text is copied out before the stream
/// buffer is freed. Neither `cmd` nor `arg` may contain an interior NUL.
///
/// Returns the command's textual output, which may be empty. Failure (stream setup failure or a
/// non-success status from `switch_api_execute`) is reported via `Err`.
pub fn execute_api(
    cmd: impl AsRef<str>,
    arg: impl AsRef<str>,
    session: Option<&crate::Session>,
) -> Result<String> {
    let cmd = cstring(cmd)?;
    let arg = cstring(arg)?;
    // A borrowed `Session` is a non-owning handle valid for the call duration; null when absent.
    let session_ptr = session.map_or(std::ptr::null_mut(), |s| s.as_ptr());

    let buffer = alloc_chunk()?;

    // Build a `switch_stream_handle_t` by hand, replicating `SWITCH_STANDARD_STREAM` (same
    // construction as `execute`: zeroed struct, a malloc'd 1KiB buffer, both console writers).
    let mut stream = sys::switch_stream_handle {
        data: buffer.cast(),
        end: buffer.cast(),
        data_size: STREAM_CHUNK_LEN,
        write_function: Some(sys::switch_console_stream_write),
        raw_write_function: Some(sys::switch_console_stream_raw_write),
        alloc_len: STREAM_CHUNK_LEN,
        alloc_chunk: STREAM_CHUNK_LEN,
        ..Default::default()
    };

    // SAFETY: `stream` is a fully initialized handle with a valid buffer and the console writers
    // installed; `cmd`/`arg` are valid, NUL-terminated C strings for the call; `session_ptr` is
    // null or a live session handle.
    let status = unsafe {
        sys::switch_api_execute(cmd.as_ptr(), arg.as_ptr(), session_ptr, &mut stream)
    };

    // Read the accumulated output before tearing the stream down. `data` may have been realloc'd
    // by the writers, so always free the final `data` pointer rather than the original `buffer`.
    let data_ptr = stream.data.cast::<u8>();
    let len = stream.data_len;
    let output = if !data_ptr.is_null() {
        // SAFETY: `data_ptr` is null or points at the buffer the writers maintain; `data_len` is
        // the number of bytes written.
        let bytes = unsafe { std::slice::from_raw_parts(data_ptr, len) };
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::new()
    };

    // SAFETY: `stream.data` is the current buffer (possibly realloc'd from `buffer`) allocated by
    // the libc allocator and now no longer referenced.
    if !data_ptr.is_null() {
        unsafe { free(data_ptr.cast()) };
    }

    status_to_result(status)?;
    Ok(output)
}
```

### `lib.rs` re-export 更新

`crates/fswtch/src/lib.rs` 中(约第 50-52 行):

```rust
// 现状:
pub use console::{CompletionFunc, CompletionMatches, complete, execute, expand_alias, free_matches};
// 改为:
pub use console::{
    CompletionFunc, CompletionMatches, complete, execute, execute_api, expand_alias, free_matches,
};
```

### 可选重构(同时改)

`execute` 与 `execute_api` 的 stream 构造逻辑重复。可提取私有 helper 减少重复:

```rust
/// Builds a `switch_stream_handle_t` replicating `SWITCH_STANDARD_STREAM`: zeroed struct, a
/// malloc'd `STREAM_CHUNK_LEN` buffer, and both console writers. Returns the handle and the
/// original buffer pointer (the writers may `realloc` `stream.data` away from it).
fn standard_stream() -> Result<(sys::switch_stream_handle, *mut u8)> {
    let buffer = alloc_chunk()?;
    let stream = sys::switch_stream_handle {
        data: buffer.cast(),
        end: buffer.cast(),
        data_size: STREAM_CHUNK_LEN,
        write_function: Some(sys::switch_console_stream_write),
        raw_write_function: Some(sys::switch_console_stream_raw_write),
        alloc_len: STREAM_CHUNK_LEN,
        alloc_chunk: STREAM_CHUNK_LEN,
        ..Default::default()
    };
    Ok((stream, buffer))
}
```

随后 `execute` 与 `execute_api` 各自 `let (mut stream, _buffer) = standard_stream()?;`。
注意:输出读取与 free 仍需基于**最终** `stream.data`(可能被 realloc),不是 `_buffer`
——这一不变式在两处都已在注释里写明,重构时务必保留。

---

## 验收标准

1. **编译通过**:`cargo build -p fswtch` 无 warning(注意 `switch_time_t`/`u32` 类型对齐)。
2. **不破坏现有 `console::execute` 行为**(若采纳重构,跑一遍 console 既有测试)。
3. **业务侧迁移后 unsafe 收敛**:见下表。

### 加完之后,voice-call 业务侧可删的 unsafe(对照表)

| 业务侧现状的 unsafe | 替换为 | 还剩 unsafe? |
|---|---|---|
| `mod_fs_metrics` 手写 `extern "C"` 块 + `unsafe { switch_core_uptime() }` 等三处调用 | `fswtch::core::{uptime, session_count, sessions_per_second}` | **全删** |
| `mod_fs_metrics` `event_type(23)` transmute | `fswtch::sys::switch_event_types_t::SWITCH_EVENT_API`(变体已在 sys 中,无需 transmute) | **全删** |
| `mod_fs_metrics` 手写 `switch_event_bind` + 11 个 `unsafe extern "C" fn on_*` 回调 | `fswtch::EventBinder` + `fswtch::event_callback!` 宏(unsafe 收进宏) | **全删** |
| `mod_cc_ai` 的 `fs_api`(`zeroed`/`malloc`/`from_raw_parts`/`free` + 手写 FFI 块) | 直接用 `fswtch::console::execute`(mod_cc_ai 传 `null_mut()` session,不需要 Patch B) | **全删** |
| `mod_voice_seat` worktree 两份 `control.rs` 的 `fs_api` | 同上;需 cmd/arg 分离时用 Patch B 的 `execute_api` | **全删** |

剩余删不掉的只有 `unsafe impl Send/Sync`(`SendBinder` / `CallState`),那属调用方
类型系统约束,本就不该由 fswtch 封装。

---

## 附:业务侧过期注释提醒(非本任务范围,但相关)

voice-call 的 `freeswitch/mod_fs_metrics/src/lib.rs` 有两条过期且错误的注释,迁到本
封装后应一并删除:

- `lib.rs:27-31`:称 `switch_event_bind` / `switch_core_session_count` / `switch_core_uptime`
  / `switch_core_sessions_per_second` "not in fswtch-sys fallback bindings" —— 实际早就在
  sys 里(`allowlist_function("switch_.*")` 已覆盖)。
- `lib.rs:77`:`event_type()` 的"beyond fswtch-sys fallback enum range"注释 —— 实际
  `SWITCH_EVENT_API = 23` 是 `switch_event_types_t` 枚举里的合法判别值(已核生成绑定),
  transmute 不产生 UB,且该变体可直接按名引用,无需 transmute。
