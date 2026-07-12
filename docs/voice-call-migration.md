# voice-call 迁移到 fswtch fork

将 `mod_voice_seat` 从手写 FFI（`ffi.rs` + `aec_vad/events.rs`）迁移到 fswtch fork 的 safe wrapper。迁移后下游源码**零 `unsafe`、零 `sys::`、零手写 `extern`**。

## 背景

voice-call 当前依赖 upstream `RustedBytes/fswtch`，它的 `fallback.rs` 缺 `switch_event_bind`、`switch_channel_*`、`switch_core_session_locate` 等符号，逼出 219 行手写 `ffi.rs` + `events.rs` 里的 `unsafe extern "C"`。

fswtch fork（本仓库 `crates/fswtch`）用真 bindgen（allowlist `switch_.*`）替换 fallback，并加了完整 safe wrapper 层。voice-call 需要的全部 `switch_` 符号都有 safe wrapper，无缺口。

## 前置条件

### 1. 依赖切换

`freeswitch/mod_voice_seat/Cargo.toml`：

```toml
# 改前
fswtch = { git = "https://github.com/RustedBytes/fswtch", branch = "master" }

# 改后（指向本 fork；建议 fork 出 voice-call 自己的 vendor 分支并 pin tag）
fswtch = { path = "../../crates/fswtch" }
# 或 git: fswtch = { git = "https://github.com/<your>/fswtch", tag = "voice-seat-v1" }
```

### 2. 构建环境

fswtch-sys 需要 FreeSWITCH 配置好的头文件。本机已满足：

```bash
export FREESWITCH_INCLUDE_DIR=/Volumes/Workspace/GitHub/freeswitch/src/include
# 验证：该目录下 switch_am_config.h 必须存在（说明 ./configure 跑过）
```

CI / 部署机要确认这个环境变量传入构建。`deploy/` 脚本里设进去。

不需要 `bundled` feature，不需要 `fallback.rs`。

## 迁移清单

### A. 删除 `ffi.rs`（整文件，219 行）

voice-call 的 `ffi.rs` 定义了 `SessionHandle` + 手写 `extern`。逐项替换：

| ffi.rs 手写 | fswtch safe wrapper | 备注 |
|---|---|---|
| `SessionHandle::locate(uuid)` + `rwunlock` | `Session::locate(uuid)` → `SessionGuard` | RAII：Drop 自动 unlock |
| `SessionHandle::hangup(cause)` | `Session::hangup(Cause::NORMAL_CLEARING)` | cause 用 `fswtch::Cause` |
| `SessionHandle::answer()` | `Session::answer()` | |
| `SessionHandle::send_dtmf(digits)` | `Session::send_dtmf(digits)` | 直接走 `switch_core_session_send_dtmf_string` |
| `SessionHandle::set_variable(name, val)` | `Session::channel()?.set_variable(name, val)` | channel 上设变量 |
| `SessionHandle::get_variable(name)` | `Session::channel()?.get_variable(name)` | TOCTOU-safe（strdup） |
| `switch_call_cause_t` / `SWITCH_CAUSE_*` | `fswtch::Cause` | 删手写常量 |
| `switch_channel_t` 手写 `#[repr(C)]` | `fswtch::Channel` / `sys::switch_channel_t` | 删 |

**核心模式变化**：`SessionHandle` 是手写的 RAII（locate + Drop unlock），fswtch 的 `SessionGuard` 已经做了这件事，且 `Session` 是 `Copy`（轻量句柄，不管理资源），用起来更直接。

#### 调用点示例

`control.rs` 改前：
```rust
use crate::ffi::{SessionHandle, SWITCH_CAUSE_NORMAL_CLEARING};

pub fn hangup(uuid: &str) -> Result<()> {
    if let Some(handle) = SessionHandle::locate(uuid) {
        handle.hangup(SWITCH_CAUSE_NORMAL_CLEARING);
    }
    Ok(())
}
```

改后：
```rust
use fswtch::{Session, Cause};

pub fn hangup(uuid: &str) -> Result<()> {
    if let Some(guard) = Session::locate(uuid)? {
        guard.session().hangup(Cause::NORMAL_CLEARING);
    }
    Ok(())
}
```

注意：`guard.session()` 返回 `&Session`（借 guard），`Session` 是 `Copy` 所以方法签名为 `fn xxx(self, ...)` 时直接传值即可。

### B. 替换 `aec_vad/events.rs`（fire 事件路径）

voice-call 的 `events.rs` 手写 `switch_event_create_subclass_detailed` + `add_header_string` + `fire_detailed` + `destroy` 来**发**事件（不是订阅）。fswtch 的 `Event` 已封装全套，且 RAII Drop 自动 `destroy`。

改前（`fire_voiced_start` 等）：
```rust
unsafe extern "C" {
    fn switch_event_create_subclass_detailed(...);
    fn switch_event_add_header_string(...);
    fn switch_event_fire_detailed(...);
    fn switch_event_destroy(...);
}

pub fn fire_voiced_start(unique_id: &str, channel_name: &str) {
    let mut event_ptr: *mut switch_event_t = std::ptr::null_mut();
    unsafe { switch_event_create_subclass_detailed(&mut event_ptr, ...); }
    // ... add_header_string ...
    unsafe { switch_event_fire_detailed(...); }
    unsafe { switch_event_destroy(&mut event_ptr); }
}
```

改后：
```rust
use fswtch::{Event, EventType};

const SWITCH_EVENT_DETECTED_SPEECH: EventType = EventType::from_raw(47); // 或用枚举

pub fn fire_voiced_start(unique_id: &str, channel_name: &str) -> Result<()> {
    let mut event = Event::create_subclass(SWITCH_EVENT_DETECTED_SPEECH)?;
    event.add_header("Unique-ID", unique_id)?;
    event.add_header("Channel-Name", channel_name)?;
    event.fire()?;
    Ok(())  // Drop 自动 destroy，无需手动 cleanup
}
```

要点：
- `Event::fire(self)` 消耗 ownership，fire 后不可再用
- `Drop` 自动 `switch_event_destroy`，消除手动 destroy 的调用顺序 bug
- EventType 如果 fork 没有对应的枚举变体（如 `DETECTED_SPEECH`），用 `EventType::from_raw(47)` 构造

### C. 订阅事件（如果有）

voice-call 若有订阅 FS 事件的代码（`switch_event_bind`），用 `EventBinder`：

```rust
use fswtch::EventBinder;

let mut binder = EventBinder::bind(MyEventType, |event| {
    // event 是 EventRef，add_header 读取等
})?;
// binder Drop 时自动 unbind，无需手动调用顺序
```

RAII Drop auto-unbind 消除"忘记 unbind"和"unbind 顺序错误"两类 bug。

### D. `Cause` 替换

```rust
// 改前
use crate::ffi::{switch_call_cause_t, SWITCH_CAUSE_NORMAL_CLEARING};
fn hangup(cause: switch_call_cause_t) { ... }

// 改后
use fswtch::Cause;
fn hangup(cause: Cause) { ... }
// 调用: hangup(Cause::NORMAL_CLEARING);
```

### E. Resampler 替换（`aec_vad/resample.rs`）

voice-call 手写了 `FsResampler`（`switch_resample_perform_create` / `switch_resample_process` / `switch_resample_destroy` + `(*handle).to` 字段访问）。fork 的 `fswtch::Resample` 已封装全套：

```rust
// 改后
use fswtch::Resample;

let mut resampler = Resample::new(from_rate, to_rate, 1, Resample::DEFAULT_QUALITY)?;
let mut buf: Vec<i16> = samples.to_vec();
let out: &[i16] = resampler.process(&mut buf);   // 借用内部缓冲，无需手动 to 字段
// Drop 自动 switch_resample_destroy
```

删掉 `aec_vad/resample.rs` 整个文件（`FsResampler` + 手写 sys 调用）。

### F. Session/Channel 读取方法

voice-call 在 `aec_vad/mod.rs` 直接 `sys::switch_core_session_get_uuid` / `switch_channel_get_name` / `switch_core_session_get_read_codec`。fork 全有 safe 方法：

| voice-call 手写 (sys::) | fork safe 方法 | 说明 |
|---|---|---|
| `switch_core_session_get_uuid(session.as_ptr())` | `Session::uuid()` → `Option<String>` | 本次新增 |
| `switch_channel_get_name(channel)` | `Channel::name()` → `Option<String>` | 已有 |
| `switch_core_session_get_read_codec` + 解 `implementation` | `Session::read_sample_rate()` → `u32` | 已有（封装 codec 字段解引用） |
| `switch_core_session_get_channel(session)` | `Session::channel()` → `Option<Channel>` | 已有 |
| `switch_channel_event_set_data(channel, event)` | `Channel::event_set_data(&Event)` | 已有 |

### G. 完整 C API → fork wrapper 对照表

voice-call 手写的全部 `switch_` 符号，fork 对应 wrapper（**无缺口**）：

**Session (`fswtch::Session`)：**
- `switch_core_session_perform_locate` + `_rwunlock` → `Session::locate()` + `SessionGuard`（RAII）
- `switch_core_session_get_channel` → `Session::channel()`
- `switch_core_session_get_uuid` → `Session::uuid()` ← **本次新增**
- `switch_core_session_get_read_codec` + 字段解引用 → `Session::read_sample_rate()`
- `switch_core_session_send_dtmf_string` → `Session::send_dtmf()`
- `switch_channel_perform_answer` → `Session::answer()`
- `switch_channel_perform_hangup` → `Session::hangup()` / `Channel::hangup()`

**Channel (`fswtch::Channel`)：**
- `switch_channel_get_name` → `Channel::name()`
- `switch_channel_get_uuid` → `Channel::uuid()`
- `switch_channel_set_variable_var_check` → `Channel::set_variable()`
- `switch_channel_get_variable_dup` → `Channel::get_variable()`（TOCTOU-safe strdup）
- `switch_channel_event_set_data` → `Channel::event_set_data(&Event)`

**Event (`fswtch::Event` / `EventRef` / `EventBinder`)：**
- `switch_event_create_subclass_detailed` → `Event::create_subclass()`
- `switch_event_add_header_string` → `Event::add_header()`
- `switch_event_add_body` → `Event::add_body()`
- `switch_event_fire_detailed` → `Event::fire()`
- `switch_event_destroy` → `Event::Drop`（RAII 自动）
- `switch_event_bind` + `switch_event_unbind` → `EventBinder`（RAII Drop auto-unbind）
- 事件回调 `on_command` → `event_callback!` 宏

**Resampler (`fswtch::Resample`)：**
- `switch_resample_perform_create` → `Resample::new()`
- `switch_resample_process` + `(*handle).to` → `Resample::process()` 返回借用切片
- `switch_resample_destroy` → `Resample::Drop`

**类型：**
- `switch_call_cause_t` / `SWITCH_CAUSE_*` → `fswtch::Cause`
- `switch_channel_t`（手写 opaque）→ `fswtch::Channel`
- `switch_status_t` → `fswtch::Status` / `Result`
- `switch_event_types_t` / `SWITCH_EVENT_CUSTOM` → `fswtch::EventType`
- `SWITCH_EVENT_DETECTED_SPEECH = 47` → `EventType` 枚举或 `EventType::from_raw(47)`

## 验证迁移完整性

迁移后跑这个检查，确认零残留：

```bash
# 1. 不应有 unsafe FFI 调用（unsafe impl Send/Sync 标记 trait 除外）
grep -rn "unsafe " freeswitch/mod_voice_seat/src/ | grep -v "impl Send\|impl Sync"
# 应为空

# 2. 不应有 sys:: 直接引用
grep -rn "sys::\|fswtch::sys" freeswitch/mod_voice_seat/src/
# 应为空

# 3. 不应有手写 extern
grep -rn 'extern "C"' freeswitch/mod_voice_seat/src/
# 应为空

# 4. ffi.rs 应已删除
test ! -f freeswitch/mod_voice_seat/src/ffi.rs && echo "✓ ffi.rs deleted"
```

## 残留说明

### 1. media bug 回调拿 session —— **零 unsafe**（fork 已封装）

voice-call 在 `on_read_replace`/`on_write_replace` 等 `MediaBugHandler` 回调里需要 session（读 sample rate、UUID、channel）。**fork 的 `MediaBugContext::session()` 现在返回 `Option<Session>`（safe）**，回调里直接用：

```rust
fn on_read_replace(&mut self, ctx: &mut MediaBugContext<'_>, frame: &mut MediaFrameMut<'_>) {
    let session = ctx.session().expect("session live in callback");
    let rate = session.read_sample_rate();   // safe
    let uuid = session.uuid();               // safe
    let channel = session.channel();         // safe
}
```

soundness 依据：FS 在 session 读锁下回调 media bug，session 在回调期间必然有效。`Session` 是非拥有句柄（`Copy`），从借用指针构造 sound。**voice-call 不需要 `unsafe { Session::from_raw }`**。

### 2. `extern "C" fn switch_module_shutdown`（**可消除 `unsafe`**）

voice-call 现状写 `unsafe extern "C" fn switch_module_shutdown() -> fswtch::Status`。**`unsafe` 是多余的** —— fork 的 `module_exports!` 宏内部已生成 `extern "C"` trampoline（exports.rs），用户提供的 shutdown 类型是 `Option<extern "C" fn() -> fswtch::Status>`（**无 `unsafe`**）。

```rust
// 改前 (voice-call 现状, 多余的 unsafe)
unsafe extern "C" fn switch_module_shutdown() -> fswtch::Status { ... }

// 改后 (fork 宏要求的就是 extern "C", 无 unsafe)
extern "C" fn switch_module_shutdown() -> fswtch::Status { ... }
```

`extern "C"` 本身保留（FS module ABI 必需），但 `unsafe` 修饰符删掉。

### 3. `unsafe impl Send/Sync`（如有，可保留）

voice-call 若有跨线程传递 `!Send` 资源（resampler、event binder 句柄）会留下 `unsafe impl Send for X`。这是**标记 trait**（标记类型可跨线程），不是 FFI 调用，不引入 FFI 风险。参考 mod-vad-bot 的 `SendResample`/`SendBinder`。

## API 稳定性

- fork pin 到最新 commit（`5e6ad3e` 或之后；或 fork 出 voice-call vendor 分支打 tag `voice-seat-v1`）
- 两个项目共享 fork 时建议 pin tag 而非 commit hash，避免漂移
- voice-call 实际用到的 API 面（Session/Channel/Event/Cause/EventBinder）小且稳定，pin tag + 必要时 vendor patch 可行

## 风险

- **构建环境**：`FREESWITCH_INCLUDE_DIR` 必须传入 CI / 部署构建，否则 fswtch-sys 编译失败
- **fork 维护**：~20k 行相对 upstream 的重写，不会与 upstream RustedBytes 合并。接受本项目 fork 为上游
- **media bug 路径**：若 voice-call 的 `mod_voice_seat` 用 `MediaBugContext`/`MediaFrame`（fork 的 media bug 生命周期 API），需单独确认回调签名与 fork 对齐 —— 这是 mod-vad-bot（endpoint 模式）没用到的部分，迁移前先验证

## 参考

- fswtch wrapper 源码：`crates/fswtch/src/{session,channel,event,cause}.rs`
- 已迁移的参考实现：`mod-vad-bot/src/control.rs`（CallControl trait + Session locate/hangup/send_dtmf）
- mod-vad-bot 的 `SendResample`/`SendBinder`（`unsafe impl Send` 范例）：`mod-vad-bot/src/audio_dsp.rs`、`event_sub.rs`
