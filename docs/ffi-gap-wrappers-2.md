# 补齐 FFI 缺口封装(第二批):批量挂断 / ACL / DB / NAT / callstate

> 续 [ffi-gap-wrappers.md](ffi-gap-wrappers.md)。第一批补了 `core` 运行时计数 + `switch_api_execute`。
> 本文档覆盖审查中确认的另外 5 个"模块开发者高频用到、但缺高层封装"的子系统。

## 背景与方法

对 `fswtch-sys` 生成的全部 1791 个 `switch_*` 公开函数做了覆盖统计:fswtch 已封约 897 个
(~50%),未封 1357 个。其中绝大多数不该封(FreeSWITCH 内部机制、`_perform_*` 带 file:line
的内部别名、有 Rust 原生替代的底层原语)。本文档只收录经核查确认"模块开发者高频需要、
且当前缺高层 safe 封装"的 5 个子系统。

每个 patch 的事实(函数签名、类型定义、现有代码落点)均已对照
`target/debug/build/fswtch-sys-*/out/bindings.rs` 与现有 `crates/fswtch/src/` 源码逐项
核对。所有签名引用为 bindgen 生成结果,一字不差。

---

## 缺口总览

| Patch | 子系统 | 价值 | 现状 | 必要性 |
|---|---|---|---|---|
| **C** | 批量挂断(`switch_core_session_hupall*`) | 运维批量操作刚需 | 无 | **必加** |
| **D** | IP/ACL 白名单(`switch_network_list_*`) | SIP 模块几乎必用 | 无 | **必加** |
| **E** | core_db 便利函数 | 日常 DB 操作便捷层 | 部分封(`CoreDb` 只封底层 prepared stmt) | **必加** |
| **F** | NAT 穿透(`switch_nat_*`) | 网关/中继模块 | 无 | **可选**(仅 NAT 部署需要) |
| **G** | callstate 字符串往返 | 配合现有 `CallState` | `CallState` enum 已封,转换函数漏 | **必加** |

> `switch_limit_*` 经核查**已全部封装**(`limit.rs`),非缺口,不列入。
> `switch_event_*` 漏的全是内部调度机制,非模块开发者用,不列入。

---

## Patch C(必加)— 批量挂断会话

### 背景

`switch_core_session_hupall` 家族按"endpoint / channel variable"匹配并批量挂断 session。
voice-call 类业务做"挂断某网关的所有会话""挂断某 var 匹配的所有会话"时必用。

### 关键事实(已核查)

- **没有 `switch_hupall_flag_t`、没有回调**。这批函数纯靠内置匹配(endpoint / var),无用户回调。
  之前预期会有的 `switch_hupall_callback_t` 在本仓库 vendoring 的 FreeSWITCH 头里不存在。
- 实际用到的选择类型是 `switch_hup_type_t`(**位掩码**,非互斥枚举):
  - `SHT_NONE = 0`、`SHT_UNANSWERED = 1` (`1<<0`)、`SHT_ANSWERED = 2` (`1<<1`)
  - C 头里两个不带 `_ans` 后缀的版本是 `#define` 宏,内部传 `SHT_UNANSWERED | SHT_ANSWERED`
    (= 3)。**bindgen 不生成 `#define`,Rust 侧只能直接调 `_ans` 版本**。
- `_ans` 后缀的含义 = "带 answered 选择参数的版本",**不是**"只挂已应答的"。
- `_matching_var`(单数 var,传一对 name/value)vs `_matching_vars`(复数,传一个
  `switch_event_t` 当 key/value 容器)。
- 线程安全:core 内部遍历全局 session 表并持 session manager 锁,外部调用无需加锁。
- `Cause` 已在 `status.rs` 封好(`crate::Cause`,`#[repr(transparent)]` newtype,有
  `.raw()` / `from_raw()`)。`Event`(`.as_ptr()`)、`EndpointInterface`(`.as_ptr()`)也都在。
- **`switch_hup_type_t` 当前无任何高层封装** —— 需新建 `HupType`。

### 签名(bindings.rs 原文)

```rust
pub fn switch_core_session_hupall(cause: switch_call_cause_t);
pub fn switch_core_session_hupall_endpoint(
    endpoint_interface: *const switch_endpoint_interface_t,
    cause: switch_call_cause_t,
);
pub fn switch_core_session_hupall_matching_var_ans(
    var_name: *const c_char,
    var_val: *const c_char,
    cause: switch_call_cause_t,
    type_: switch_hup_type_t,
) -> u32;                                            // 返回挂断的 session 数
pub fn switch_core_session_hupall_matching_vars_ans(
    vars: *mut switch_event_t,
    cause: switch_call_cause_t,
    type_: switch_hup_type_t,
) -> u32;
```

### 落点:`core.rs`

`session.rs` 全是 `impl Session`/`impl SessionGuard`,无顶层 `pub fn`;而 `core.rs` 已有
`session_count`/`sessions_per_second` 这种"前缀是 `switch_core_session_*`、语义却是全局态"
的先例,且 re-export 通路就绪。hupall 是全局操作 session 集合,放 `core.rs` 风格一致。

接在现有 `sessions_per_second()` 之后(`core.rs` 末尾的 `// NOTE:` 之前)。

### 代码

#### C.1 `HupType` 放 `status.rs`

`switch_hup_type_t` 是位掩码,按现有约定(status.rs 模块文档把 bitmask 归己管,如
`OriginateFlag`)放 `status.rs` 里 `OriginateFlag` 旁边,实现 `bits()`/`contains()`/`BitOr`。
**先读 `status.rs` 里 `OriginateFlag` 的实现照抄其模式。**

```rust
/// Which legs a batch hangup applies to — a bitmask over `switch_hup_type_t`.
///
/// Used by [`crate::hupall_matching_var`] / [`crate::hupall_matching_vars`]. Combine with `|`:
/// `HupType::ANSWERED | HupType::UNANSWERED` matches both legs (the default of the upstream
/// `switch_core_session_hupall_matching_var` macro, which Rust cannot call since bindgen drops
/// the `#define`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct HupType(pub sys::switch_hup_type_t);

impl HupType {
    pub const NONE: Self = Self(sys::switch_hup_type_t_SHT_NONE);
    pub const UNANSWERED: Self = Self(sys::switch_hup_type_t_SHT_UNANSWERED);
    pub const ANSWERED: Self = Self(sys::switch_hup_type_t_SHT_ANSWERED);

    /// The raw bitmask value, for FFI.
    #[inline]
    pub const fn bits(self) -> sys::switch_hup_type_t {
        self.0
    }

    /// `true` if `self` contains all of `other`'s bits.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for HupType {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl From<HupType> for sys::switch_hup_type_t {
    fn from(t: HupType) -> Self {
        t.0
    }
}
```

#### C.2 四个 hupall 函数放 `core.rs`

```rust
/// Hangs up **every** active session with `cause`.
///
/// Wraps `switch_core_session_hupall`. Thread-safe — the core holds the session-manager lock while
/// iterating. Prefer the more targeted [`hupall_matching_var`] / [`hupall_endpoint`] when you only
/// need a subset; this is a blunt instrument (typically `SYSTEM_SHUTDOWN`).
pub fn hupall(cause: crate::Cause) {
    // SAFETY: `cause.raw()` is a valid `switch_call_cause_t`; no pointers. The core takes the
    // session-manager lock internally.
    unsafe { sys::switch_core_session_hupall(cause.raw()) };
}

/// Hangs up every session belonging to `endpoint` with `cause`.
///
/// Wraps `switch_core_session_hupall_endpoint`. Useful for shutting down a custom endpoint's legs
/// on module unload.
pub fn hupall_endpoint(endpoint: &crate::EndpointInterface, cause: crate::Cause) {
    // SAFETY: `endpoint.as_ptr()` is a live endpoint interface; `cause.raw()` is valid. `*mut`
    // coerces to the FFI's `*const`. Core takes the session-manager lock internally.
    unsafe { sys::switch_core_session_hupall_endpoint(endpoint.as_ptr(), cause.raw()) };
}

/// Hangs up every session whose channel variable `var_name == var_val`, selected by `hup_type`.
///
/// Wraps `switch_core_session_hupall_matching_var_ans`. Returns the number of sessions hung up.
/// Pass [`HupType::ANSWERED`]\(|[`HupType::UNANSWERED`]|) to match both legs — this reproduces the
/// upstream `switch_core_session_hupall_matching_var` macro (a `#define` bindgen drops). Interior
/// NUL in either string is rejected.
pub fn hupall_matching_var(
    var_name: impl AsRef<str>,
    var_val: impl AsRef<str>,
    cause: crate::Cause,
    hup_type: crate::HupType,
) -> Result<u32> {
    let var_name = cstring(var_name)?;
    let var_val = cstring(var_val)?;
    // SAFETY: both C strings are valid NUL-terminated values for the call; `cause.raw()` and
    // `hup_type.bits()` are plain integers. Core takes the session-manager lock internally.
    let n = unsafe {
        sys::switch_core_session_hupall_matching_var_ans(
            var_name.as_ptr(),
            var_val.as_ptr(),
            cause.raw(),
            hup_type.bits(),
        )
    };
    Ok(n)
}

/// Hangs up every session matching **all** key/value pairs in `vars` (a FreeSWITCH event used as a
/// header bag), selected by `hup_type`.
///
/// Wraps `switch_core_session_hupall_matching_vars_ans`. Returns the number of sessions hung up.
/// `vars` is borrowed for the call; pass [`HupType::ANSWERED`]\(|[`HupType::UNANSWERED`]|) to match
/// both legs.
pub fn hupall_matching_vars(
    vars: &crate::Event,
    cause: crate::Cause,
    hup_type: crate::HupType,
) -> u32 {
    // SAFETY: `vars.as_ptr()` is a live event handle for the call; `cause.raw()` and
    // `hup_type.bits()` are plain integers. Core takes the session-manager lock internally.
    unsafe {
        sys::switch_core_session_hupall_matching_vars_ans(
            vars.as_ptr(),
            cause.raw(),
            hup_type.bits(),
        )
    }
}
```

### re-export 更新(`lib.rs`)

`core` 那行加上 4 个函数名(注意 `core` import 行需加 `Result`):

```rust
pub use core::{
    get_domain, get_hostname, get_switchname, get_uuid, get_variable, hupall, hupall_endpoint,
    hupall_matching_var, hupall_matching_vars, session_count, sessions_per_second, set_variable,
    uptime,
};
```

`status` 那行加上 `HupType`(放在 `OriginateFlag` 附近,按字母序)。

### 备注

- `hupall` / `hupall_endpoint` 返回 `void`,无计数;两个 `_matching` 返回 `u32` 计数。
  四个函数都**不**返回 `switch_status_t`,故不走 `status_to_result`,无失败路径。
- `EndpointInterface::as_ptr(&self) -> *mut sys::switch_endpoint_interface_t`,而 FFI 形参是
  `*const` —— `*mut` 到 `*const` 是隐式转换,Rust 允许。
- `Event::as_ptr(&self) -> *mut sys::switch_event_t`,正好匹配。

---

## Patch D(必加)— IP/ACL 白名单(`switch_network_list_*`)

### 背景

FreeSWITCH 的 `network_list` 是 `acl.conf.xml` 背后的程序化接口:一组 CIDR/host 规则,做
"某 IP 是否落在可信网段内"的判定。写 SIP/网关模块时几乎必用。

### 关键事实(已核查)

- **`switch_network_list_t` 是 opaque**(零长度占位),真实字段在 `.c` 里,Rust 侧不可见。
- **内存所有权:无 destroy 函数。** list 本体 + 所有 node + 所有 token 字符串都挂在 create
  时传入的 pool 上,pool 销毁时一次性回收。所以**不做独立 RAII Drop**。
- **`create` 的 `pool` 参数可传 NULL**(内部会 `switch_core_new_memory_pool` 自建一个),但
  这样 pool 对外不可见、生命周期失控 —— **封装时强制必填 `&Pool`**,借用关系交给 borrow
  checker 管。
- `default_type`(`switch_bool_t`):list 默认放行(`SWITCH_TRUE`)还是拒绝(`SWITCH_FALSE`)。
- `add_cidr_token` 的 `token`:给规则贴的标签(可 NULL),validate 命中时通过 out param 回传,
  典型用途是 ACL 命名。token 被 `strdup` 进 pool,调用方传入的串调用后可释放。
- validate 返回 **`switch_bool_t`**(0/1,匹配且允许),不是 `switch_status_t`。token 是
  `*mut *const c_char`(二级指针 out param),指向 pool 内存储(借用 `&self`,不可释放)。
- validate 语义 = 最长前缀匹配 + default 回退:遍历 node_head,找 `bits` 最大且 IP 落网段内的
  node,用其 `ok` 覆盖 default;一个都没中则返回 `default_type`。
- IPv4 validate 入参 `u32`(网络序);IPv6 入参 `ip_t`(union `{v4: u32, v6: in6_addr}`)。
  高层 API 应接受 `std::net::Ipv4Addr`/`Ipv6Addr`。
- `switch_network_port_range_t`(`port` 参数,可 `null_mut()` 表示不限端口)。

### 签名(bindings.rs 原文)

```rust
pub fn switch_network_list_create(
    list: *mut *mut switch_network_list_t,
    name: *const c_char,
    default_type: switch_bool_t,
    pool: *mut switch_memory_pool_t,
) -> switch_status_t;

pub fn switch_network_list_add_cidr_token(
    list: *mut switch_network_list_t,
    cidr_str: *const c_char,
    ok: switch_bool_t,
    token: *const c_char,
) -> switch_status_t;
pub fn switch_network_list_add_cidr_port_token(
    list: *mut switch_network_list_t, cidr_str: *const c_char, ok: switch_bool_t,
    token: *const c_char, port: switch_network_port_range_p,   // *mut, 可 null
) -> switch_status_t;

pub fn switch_network_list_add_host_mask(
    list: *mut switch_network_list_t, host: *const c_char, mask_str: *const c_char, ok: switch_bool_t,
) -> switch_status_t;
pub fn switch_network_list_add_host_port_mask(
    list: *mut switch_network_list_t, host: *const c_char, mask_str: *const c_char,
    ok: switch_bool_t, port: switch_network_port_range_p,
) -> switch_status_t;

// validate 返回 switch_bool_t(不是 status);token 是 out param
pub fn switch_network_list_validate_ip_token(
    list: *mut switch_network_list_t, ip: u32, token: *mut *const c_char,
) -> switch_bool_t;
pub fn switch_network_list_validate_ip6_token(
    list: *mut switch_network_list_t, ip: ip_t, token: *mut *const c_char,
) -> switch_bool_t;
pub fn switch_network_list_validate_ip_port_token(
    list: *mut switch_network_list_t, ip: u32, port: c_int, token: *mut *const c_char,
) -> switch_bool_t;
pub fn switch_network_list_validate_ip6_port_token(
    list: *mut switch_network_list_t, ip: ip_t, port: c_int, token: *mut *const c_char,
) -> switch_bool_t;
```

### 落点:新建 `crates/fswtch/src/network_list.rs`

fswtch 现有无任何 `switch_network` 引用,全新模块。在 `lib.rs` 加 `mod network_list;`。

### 代码

```rust
//! FreeSWITCH IP/ACL network lists — the programmatic face of `acl.conf.xml`.
//!
//! A [`NetworkList`] holds an ordered set of CIDR / host-mask rules with a default allow/deny,
//! and answers "is this IP permitted, and under which token?". The list borrows a [`Pool`] for
//! all storage (rules, tokens, the list itself): there is no destroy call, so the list's lifetime
//! is bounded by the pool and enforced by the borrow checker (`'pool`).

use std::ffi::CStr;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ptr::NonNull;

use crate::{Pool, Result, SwitchError, GENERR, cstring, status_to_result, sys};

/// The result of an ACL lookup: allowed or denied, plus the token of the matching rule when one
/// was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclVerdict<'a> {
    allowed: bool,
    /// The token label of the rule that decided this verdict, if any. Borrows list storage.
    token: Option<&'a CStr>,
}

impl<'a> AclVerdict<'a> {
    /// `true` when the IP matched an allow rule (or the list's default is allow and no deny hit).
    pub fn allowed(&self) -> bool {
        self.allowed
    }
    /// The token of the deciding rule, when the IP matched a labeled rule.
    pub fn token(&self) -> Option<&'a CStr> {
        self.token
    }
}

/// An IP/ACL network list, borrowing storage from a [`Pool`].
///
/// Rules added via [`add_cidr`](Self::add_cidr) / [`add_host`](Self::add_host) are matched in
/// longest-prefix order by [`validate`](Self::validate). The list lives as long as `'pool`; there
/// is no `destroy` — the pool reclaims everything on drop.
pub struct NetworkList<'pool> {
    raw: NonNull<sys::switch_network_list_t>,
    _pool: &'pool Pool,
}

impl<'pool> NetworkList<'pool> {
    /// Creates a new list named `name` with the given `default_allow` policy.
    ///
    /// `default_allow = true` means IPs matching no rule are permitted; `false` means denied.
    /// The list borrows `pool` for all subsequent storage.
    pub fn new(name: impl AsRef<str>, default_allow: bool, pool: &'pool Pool) -> Result<Self> {
        let name = cstring(name)?;
        let default_type = if default_allow {
            sys::switch_bool_t_SWITCH_TRUE
        } else {
            sys::switch_bool_t_SWITCH_FALSE
        };
        let mut raw: *mut sys::switch_network_list_t = std::ptr::null_mut();
        // SAFETY: `&mut raw` is a valid out-param; `name` is a valid C string; `default_type` is a
        // valid switch_bool_t; `pool.as_ptr()` is a live APR pool the list will bind to.
        let status =
            unsafe { sys::switch_network_list_create(&mut raw, name.as_ptr(), default_type, pool.as_ptr()) };
        status_to_result(status)?;
        let raw = NonNull::new(raw).ok_or(SwitchError(GENERR))?;
        Ok(Self { raw, _pool: pool })
    }

    /// Adds a CIDR rule (e.g. `"10.0.0.0/8"`, `"::1/128"`). `allow = true` permits matching IPs.
    /// `token` labels the rule for later retrieval during [`validate`](Self::validate); pass `None`
    /// for an unlabeled rule.
    pub fn add_cidr(
        &self,
        cidr: impl AsRef<str>,
        allow: bool,
        token: Option<&str>,
    ) -> Result<()> {
        let cidr = cstring(cidr)?;
        let token = match token {
            Some(t) => Some(cstring(t)?),
            None => None,
        };
        let ok = bool_to_switch(allow);
        let token_ptr = token.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: `self.raw` is a live list; `cidr`/`token` are valid C strings (or null) for the
        // call; `ok` is a valid switch_bool_t.
        let status = unsafe {
            sys::switch_network_list_add_cidr_token(self.raw.as_ptr(), cidr.as_ptr(), ok, token_ptr)
        };
        status_to_result(status)
    }

    /// Adds a host/mask rule (e.g. host `"10.0.0.1"`, mask `"255.0.0.0"`). `port_range` of `None`
    /// means any port.
    pub fn add_host(
        &self,
        host: impl AsRef<str>,
        mask: impl AsRef<str>,
        allow: bool,
    ) -> Result<()> {
        let host = cstring(host)?;
        let mask = cstring(mask)?;
        let ok = bool_to_switch(allow);
        // SAFETY: `self.raw` is a live list; `host`/`mask` are valid C strings; `ok` valid.
        let status = unsafe {
            sys::switch_network_list_add_host_mask(
                self.raw.as_ptr(),
                host.as_ptr(),
                mask.as_ptr(),
                ok,
            )
        };
        status_to_result(status)
    }

    /// Looks up `ip` against the list (longest-prefix match, default fallback). The returned
    /// [`AclVerdict`] borrows the matching rule's token for `self`'s lifetime.
    pub fn validate_ipv4(&self, ip: Ipv4Addr) -> AclVerdict<'_> {
        let ip_u32 = u32::from_be_bytes(ip.octets());
        let mut token_ptr: *const std::ffi::c_char = std::ptr::null();
        // SAFETY: `self.raw` is a live list; `ip_u32` is a plain u32; `&mut token_ptr` is a valid
        // out-param (null when no rule matches). The returned switch_bool_t is 0/1.
        let allowed = unsafe {
            sys::switch_network_list_validate_ip_token(self.raw.as_ptr(), ip_u32, &mut token_ptr)
        } != sys::switch_bool_t_SWITCH_FALSE;
        AclVerdict {
            allowed,
            token: cstr_ptr_to_option(token_ptr),
        }
    }

    /// IPv6 variant of [`validate_ipv4`](Self::validate_ipv4).
    pub fn validate_ipv6(&self, ip: Ipv6Addr) -> AclVerdict<'_> {
        let mut ip_t = sys::ip_t::default();
        // SAFETY: `ip_t` is a Default-initialized union; writing the `v6` variant via its 16-byte
        // representation is sound and matches the union layout.
        unsafe {
            ip_t.__bindgen_anon_1.__bindgen_anon_1.__u6_addr8 = ip.octets();
        }
        // ↑ NOTE: the exact field path depends on bindgen's nested-anonymous-field handling.
        //   If bindgen names the inner union differently, use `ip_t.v6 = ...` after constructing
        //   an `in6_addr`. Verify against bindings.rs at implementation time (see "验证点" below).
        let mut token_ptr: *const std::ffi::c_char = std::ptr::null();
        let allowed = unsafe {
            sys::switch_network_list_validate_ip6_token(self.raw.as_ptr(), ip_t, &mut token_ptr)
        } != sys::switch_bool_t_SWITCH_FALSE;
        AclVerdict {
            allowed,
            token: cstr_ptr_to_option(token_ptr),
        }
    }
}

fn bool_to_switch(b: bool) -> sys::switch_bool_t {
    if b {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

/// Wraps a (possibly null) borrowed C string pointer into `Option<&CStr>` without copying/freeing.
///
/// # Safety
/// When non-null, `ptr` must point at storage owned by FreeSWITCH that outlives the caller's use
/// of the returned reference (typically the list's pool). Null is treated as `None`.
unsafe fn cstr_ptr_to_option<'a>(ptr: *const std::ffi::c_char) -> Option<&'a CStr> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: caller guarantees `ptr` is null or a valid C string with pool-backed lifetime.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}
```

> **验证点(实现时)**:`ip_t` union 的字段路径。bindgen 对匿名嵌套 union 的命名不稳定
> (可能是 `ip_t.v6`、`ip_t.__bindgen_anon_1.__u6_addr8` 等)。实现 IPv6 部分前,先
> `grep -A10 "pub union ip_t" bindings.rs` 确认确切字段名,据此写构造代码。若 union 字段
> 难以稳定访问,可退而只封 IPv4(覆盖绝大多数 ACL 场景),IPv6 留 TODO。

### re-export 更新(`lib.rs`)

```rust
mod network_list;
// ...
pub use network_list::{AclVerdict, NetworkList};
```

### 备注

- `add_cidr_port_token` / `add_host_port_mask`(带端口范围)未封 —— 端口级 ACL 较少见,首版
  可只封无端口的 `add_cidr`/`add_host`,需要时再补 `add_cidr_with_port` 等。
- `validate` 同理只封无端口的 `_token` 变体;`_port_token` 留待需要时加。
- `AclVerdict` 借用 `&self`,token 生命周期绑 list(pool),语义与 C 一致。
- `Pool::as_ptr(&self) -> *mut switch_memory_pool_t` 正好匹配 create 第 4 参(已核实)。

---

## Patch E(必加)— `core_db` 便利函数

### 背景

现有 `core_db.rs` 的 `CoreDb` 只封了底层 prepared statement API(`open`/`open_v2`/`exec`/
`prepare`/`step`/`bind_*`/`column_*`)。FreeSWITCH 还提供一批 one-liner 便利函数,是日常 DB
操作的高频入口,尤其 `test_reactive`("表不存在则建表"模式)。

### 关键事实(已核查)

- `switch_core_db_t` = `sqlite3` 别名(opaque)。现有 `CoreDb` wrap 的是 `NonNull<switch_core_db_t>`。
- `open_file` / `open_in_memory` 直接返回 `*mut switch_core_db_t`(**不是** out-param+status),
  失败返 NULL。打开的 db **必须用 `switch_core_db_close` 关** —— 而 `CoreDb::Drop` 已调 close,
  所以这两个应作 `CoreDb` 的**关联构造函数**,复用现有 Drop。
- `persistant_execute` / `persistant_execute_trans` 返 `switch_status_t`,**无 err 输出参数**。
  语义:反复执行 SQL 直到成功(锁冲突重试),最多 `retries` 次。`_trans` 包事务。
- `test_reactive` 是 **4 个参数**:`(db, test_sql, drop_sql, reactive_sql)`,返 void。语义:先跑
  `test_sql`;若失败(说明 schema 不对)则跑 `drop_sql`(通常 `DROP TABLE IF EXISTS`)再跑
  `reactive_sql`(通常 `CREATE TABLE`)。
- `get_table` 走标准 sqlite3 `char***` 接口,结果用 `switch_core_db_free_table` 释放、errmsg 用
  `switch_core_db_free` 释放。
- `switch_core_dbtype` 是无参全局函数,返 `switch_cache_db_handle_type_t`(0=CORE_DB/1=ODBC/
  2=DATABASE_INTERFACE),反映运行时配置的后端,与具体连接无关。

### 签名(bindings.rs 原文)

```rust
pub fn switch_core_db_open_file(filename: *const c_char) -> *mut switch_core_db_t;
pub fn switch_core_db_open_in_memory(uri: *const c_char) -> *mut switch_core_db_t;

pub fn switch_core_db_persistant_execute(
    db: *mut switch_core_db_t, sql: *mut c_char, retries: u32,
) -> switch_status_t;
pub fn switch_core_db_persistant_execute_trans(
    db: *mut switch_core_db_t, sql: *mut c_char, retries: u32,
) -> switch_status_t;

pub fn switch_core_db_test_reactive(
    db: *mut switch_core_db_t,
    test_sql: *mut c_char,
    drop_sql: *mut c_char,
    reactive_sql: *mut c_char,
);                                              // void

pub fn switch_core_db_get_table(
    db: *mut switch_core_db_t, sql: *const c_char,
    resultp: *mut *mut *mut c_char,             // char***
    nrow: *mut c_int, ncolumn: *mut c_int,
    errmsg: *mut *mut c_char,
) -> c_int;                                     // sqlite3 错误码
pub fn switch_core_db_free_table(result: *mut *mut c_char);
pub fn switch_core_db_free(z: *mut c_char);

pub fn switch_core_dbtype() -> switch_cache_db_handle_type_t;
```

### 落点:`core_db.rs` 的 `impl CoreDb` 块 + 一个自由函数 + 一个枚举

`open_file`/`open_in_memory`/`persistant_execute`/`persistant_execute_trans`/`test_reactive`/
`get_table` 都放 `impl CoreDb`(它们都要 db 句柄)。`switch_core_dbtype` 是无参全局函数,
应作 `core_db.rs` 顶层自由函数 + 配套枚举。先读 `core_db.rs` 确认 `impl CoreDb` 块结尾行号与
现有 `open`/`open_uri` 的写法。

#### E.1 `CoreDb` 新构造方法 + 方法

```rust
impl CoreDb {
    /// Opens a SQLite database **file** via `switch_core_db_open_file`.
    ///
    /// Unlike [`open`](Self::open) (which uses `sqlite3_open`), this is FreeSWITCH's convenience
    /// wrapper. Fails if the file cannot be opened (returns `Err` on a null handle).
    pub fn open_file(filename: impl AsRef<str>) -> Result<Self> {
        let filename = cstring(filename)?;
        // SAFETY: `filename` is a valid C string. The returned pointer is null on failure or a
        // heap-allocated sqlite3 handle that `CoreDb::Drop` will close.
        let raw = unsafe { sys::switch_core_db_open_file(filename.as_ptr()) };
        Self::from_raw(raw).ok_or(SwitchError(GENERR))
    }

    /// Opens an **in-memory** SQLite database via `switch_core_db_open_in_memory`.
    pub fn open_in_memory(uri: impl AsRef<str>) -> Result<Self> {
        let uri = cstring(uri)?;
        // SAFETY: as above.
        let raw = unsafe { sys::switch_core_db_open_in_memory(uri.as_ptr()) };
        Self::from_raw(raw).ok_or(SwitchError(GENERR))
    }

    /// Executes `sql` repeatedly until it succeeds or `retries` is exhausted (handles
    /// `SQLITE_BUSY`/`SQLITE_LOCKED`).
    pub fn persistant_execute(&self, sql: &str, retries: u32) -> Result<()> {
        let mut sql_bytes = cstring(sql)?.into_bytes_with_nul();
        let sql_ptr = sql_bytes.as_mut_ptr();
        // SAFETY: `self.raw` is a live handle; `sql_ptr` is a writable NUL-terminated string
        // (the FFI takes `*mut`); `retries` is a plain u32.
        let status = unsafe {
            sys::switch_core_db_persistant_execute(self.as_ptr(), sql_ptr, retries)
        };
        status_to_result(status)
    }

    /// Like [`persistant_execute`](Self::persistant_execute) but wraps the statement in a
    /// transaction (`BEGIN`/`COMMIT`).
    pub fn persistant_execute_trans(&self, sql: &str, retries: u32) -> Result<()> {
        let mut sql_bytes = cstring(sql)?.into_bytes_with_nul();
        let sql_ptr = sql_bytes.as_mut_ptr();
        let status = unsafe {
            sys::switch_core_db_persistant_execute_trans(self.as_ptr(), sql_ptr, retries)
        };
        status_to_result(status)
    }

    /// Runs `test_sql`; if it errors (schema mismatch), runs `drop_sql` then `reactive_sql`.
    ///
    /// The canonical "create table if it doesn't exist" pattern: `test_sql` = a SELECT against
    /// the table, `drop_sql` = `DROP TABLE IF EXISTS ...`, `reactive_sql` = `CREATE TABLE ...`.
    pub fn test_reactive(
        &self,
        test_sql: &str,
        drop_sql: &str,
        reactive_sql: &str,
    ) -> Result<()> {
        let mut t = cstring(test_sql)?.into_bytes_with_nul();
        let mut d = cstring(drop_sql)?.into_bytes_with_nul();
        let mut r = cstring(reactive_sql)?.into_bytes_with_nul();
        // SAFETY: `self.raw` live; three writable NUL-terminated strings; FFI takes `*mut`.
        unsafe {
            sys::switch_core_db_test_reactive(
                self.as_ptr(),
                t.as_mut_ptr(),
                d.as_mut_ptr(),
                r.as_mut_ptr(),
            );
        }
        Ok(())
    }

    // get_table 见下 E.2 —— 它返回需 RAII 释放的资源,封成独立 guard 更稳妥。
}
```

#### E.2 `get_table` —— RAII guard

`get_table` 的结果数组(`char**`)由 sqlite3 malloc,必须用 `switch_core_db_free_table`
释放。做成 RAII guard `TableRows`,Drop 时释放,避免泄漏。

```rust
/// Rows returned by [`CoreDb::get_table`], freed automatically on drop.
///
/// Mirrors sqlite3's `get_table` flat layout: the first `column_count` entries are column names,
/// followed by `row_count` rows of `column_count` values each (all `String`, lossily decoded).
pub struct TableRows {
    raw: *mut *mut std::ffi::c_char,
    row_count: usize,
    column_count: usize,
}

impl TableRows {
    /// Number of data rows (excluding the header row of column names).
    pub fn row_count(&self) -> usize {
        self.row_count
    }
    /// Number of columns.
    pub fn column_count(&self) -> usize {
        self.column_count
    }
    /// The column name at `idx`, or `None` if out of range.
    pub fn column_name(&self, idx: usize) -> Option<&str> {
        if idx >= self.column_count {
            return None;
        }
        // SAFETY: the raw array has `column_count` name entries before the data rows.
        let ptr = unsafe { *self.raw.add(idx) };
        cstr_to_str_lossy(ptr)
    }
    /// The value at `(row, col)`, or `None` if out of range / null cell.
    pub fn get(&self, row: usize, col: usize) -> Option<&str> {
        if row >= self.row_count || col >= self.column_count {
            return None;
        }
        let idx = self.column_count + row * self.column_count + col;
        // SAFETY: `idx` is within `[0, column_count * (1 + row_count))`.
        let ptr = unsafe { *self.raw.add(idx) };
        if ptr.is_null() {
            return None;
        }
        cstr_to_str_lossy(ptr)
    }
}

impl Drop for TableRows {
    fn drop(&mut self) {
        // SAFETY: `self.raw` was allocated by `switch_core_db_get_table` and is freed exactly once.
        if !self.raw.is_null() {
            unsafe { sys::switch_core_db_free_table(self.raw) };
        }
    }
}
```

`CoreDb::get_table` 方法(返回 `Result<TableRows>`):调 `switch_core_db_get_table` 拿到
`resultp`/`nrow`/`ncolumn`;若返回非 0(`SQLITE_OK`=0)则用 `switch_core_db_free` 释放 errmsg
并返回 `Err`;成功则包成 `TableRows`。**实现前先读现有 `CoreDb::exec`**(它已处理过 errmsg 的
malloc/free 模式),照抄其 errmsg 处理。

#### E.3 `switch_core_dbtype` —— 自由函数 + 枚举

```rust
/// The runtime-configured FreeSWITCH database backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheDbType {
    /// Bundled SQLite (`SCDB_TYPE_CORE_DB = 0`).
    CoreDb,
    /// ODBC (`SCDB_TYPE_ODBC = 1`).
    Odbc,
    /// Pluggable database interface (`SCDB_TYPE_DATABASE_INTERFACE = 2`).
    DatabaseInterface,
    /// Any unrecognized value a future FreeSWITCH may add.
    Unknown(u32),
}

impl From<sys::switch_cache_db_handle_type_t> for CacheDbType {
    fn from(v: sys::switch_cache_db_handle_type_t) -> Self {
        match v {
            sys::switch_cache_db_handle_type_t_SCDB_TYPE_CORE_DB => Self::CoreDb,
            sys::switch_cache_db_handle_type_t_SCDB_TYPE_ODBC => Self::Odbc,
            sys::switch_cache_db_handle_type_t_SCDB_TYPE_DATABASE_INTERFACE => {
                Self::DatabaseInterface
            }
            other => Self::Unknown(other),
        }
    }
}

/// Reports the runtime-configured database backend (independent of any open connection).
///
/// Wraps `switch_core_dbtype`.
pub fn cache_db_type() -> CacheDbType {
    // SAFETY: no arguments; returns a plain enum-discriminant u32.
    CacheDbType::from(unsafe { sys::switch_core_dbtype() })
}
```

### re-export 更新(`lib.rs`)

`core_db` 那行加 `CacheDbType`, `TableRows`:

```rust
pub use core_db::{CacheDbType, CoreDb, Stmt, StmtRows, TableRows, cache_db_type};
```

---

## Patch F(可选)— NAT 穿透(`switch_nat_*`)

> 仅在部署需要 UPnP/NAT-PMP 自动端口映射时有用。多数服务器环境用静态端口转发,本 patch
> 可推迟。

### 关键事实(已核查)

- **没有 `switch_nat_dpmp_t` / `switch_nat_upnp_t`**。proto 枚举是 `switch_nat_ip_proto_t`
  (`SWITCH_NAT_UDP=0` / `SWITCH_NAT_TCP=1`)。
- UPnP vs NAT-PMP **不是 init 参数** —— FreeSWITCH 运行时同时探测两者,`get_type()` 返回当前
  激活的那个的描述字符串(`"upnp"`/`"pmp"`/`"n/a"`)。
- `init(pool, mapping)` 是早期初始化(由 core 调);`late_init()` 在其他模块加载完后补完。
- **init 顺序依赖**:`add_mapping` 等需 NAT 已初始化;用 `is_initialized()` 检查。
- `add_mapping(port, proto, external_port_out, sticky)`:`external_port` 是**输出参数**
  (`*mut switch_port_t`,可 null),返回实际分配的公网端口。
- `switch_nat_status()` 返回 `*mut c_char`(**owned**,caller must free —— 但 free 方式注释未指明,
  需查实现确认用 `free()` 还是 pool)。
- `switch_nat_get_type()` 返回 `*const c_char`(**borrowed** static,不释放)。

### 签名(bindings.rs 原文)

```rust
pub fn switch_nat_init(pool: *mut switch_memory_pool_t, mapping: switch_bool_t);
pub fn switch_nat_late_init();
pub fn switch_nat_is_initialized() -> switch_bool_t;
pub fn switch_nat_add_mapping(
    port: switch_port_t, proto: switch_nat_ip_proto_t,
    external_port: *mut switch_port_t, sticky: switch_bool_t,
) -> switch_status_t;
pub fn switch_nat_del_mapping(port: switch_port_t, proto: switch_nat_ip_proto_t) -> switch_status_t;
pub fn switch_nat_status() -> *mut c_char;      // owned
pub fn switch_nat_get_type() -> *const c_char;  // borrowed static
pub fn switch_nat_set_mapping(mapping: switch_bool_t);
pub fn switch_nat_republish();
pub fn switch_nat_reinit();
pub fn switch_nat_shutdown();
```

### 落点:新建 `crates/fswtch/src/nat.rs` + `IpProto` 枚举

```rust
//! FreeSWITCH NAT traversal (UPnP / NAT-PMP) — port mapping helpers.
//!
//! Init is normally driven by the FreeSWITCH core; these wrappers expose the mapping lifecycle
//! for modules that need to punch additional ports. Call [`is_initialized`] before
//! [`add_mapping`].

use std::ffi::CStr;

use crate::{Pool, Result, cstring, status_to_result, sys};

/// IP protocol for a NAT mapping — `switch_nat_ip_proto_t`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum NatIpProto {
    Udp,
    Tcp,
}

impl NatIpProto {
    fn raw(self) -> sys::switch_nat_ip_proto_t {
        match self {
            Self::Udp => sys::switch_nat_ip_proto_t_SWITCH_NAT_UDP,
            Self::Tcp => sys::switch_nat_ip_proto_t_SWITCH_NAT_TCP,
        }
    }
}

/// Initializes the NAT subsystem. `enable_mapping` toggles automatic port mapping.
///
/// Wraps `switch_nat_init`. Usually called by the core; exposed for modules that bootstrap a
/// standalone NAT context against `pool`.
pub fn init(pool: &Pool, enable_mapping: bool) {
    let mapping = bool_to_switch(enable_mapping);
    // SAFETY: `pool.as_ptr()` is a live APR pool; `mapping` is a valid switch_bool_t.
    unsafe { sys::switch_nat_init(pool.as_ptr(), mapping) };
}

/// Completes NAT subsystem init after other modules have loaded.
pub fn late_init() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_late_init() };
}

/// `true` if the NAT subsystem has been initialized and mapping calls are usable.
pub fn is_initialized() -> bool {
    // SAFETY: no arguments; returns switch_bool_t.
    unsafe { sys::switch_nat_is_initialized() } != sys::switch_bool_t_SWITCH_FALSE
}

/// Maps internal `port` (UDP/TCP) to an external port via UPnP/PMP. Returns the external port
/// actually allocated when `request_external` differs; pass `None` to ignore the returned value.
///
/// `sticky = true` marks the mapping persistent. Requires NAT to be initialized
/// ([`is_initialized`] returns `true`).
pub fn add_mapping(
    port: u16,
    proto: NatIpProto,
    request_external: Option<u16>,
    sticky: bool,
) -> Result<Option<u16>> {
    let mut external: sys::switch_port_t = request_external.unwrap_or(0);
    let external_ptr = if request_external.is_some() {
        &mut external as *mut _
    } else {
        std::ptr::null_mut()
    };
    let sticky = bool_to_switch(sticky);
    // SAFETY: `port`/`proto.raw()` are plain values; `external_ptr` is null or a valid out-param;
    // `sticky` is a valid switch_bool_t.
    let status = unsafe {
        sys::switch_nat_add_mapping(port, proto.raw(), external_ptr, sticky)
    };
    status_to_result(status)?;
    Ok(if request_external.is_some() { Some(external) } else { None })
}

/// Removes a previously added mapping.
pub fn del_mapping(port: u16, proto: NatIpProto) -> Result<()> {
    // SAFETY: plain value arguments.
    status_to_result(unsafe { sys::switch_nat_del_mapping(port, proto.raw()) })
}

/// A human-readable NAT status string (list of mappings + state), as an owned [`String`].
///
/// Wraps `switch_nat_status`, which returns a malloc'd string the caller must free. The wrapper
/// copies it out and frees the original via the C `free` paired with the allocation.
///
/// > **NOTE**: the upstream header documents "caller must free the string" but does not name the
/// > free function. Verify against the FreeSWITCH `.c` implementation that plain libc `free()` is
/// > correct before relying on this (the bundled `fswtch-src` ships headers only). If unsure,
/// > prefer [`type_str`](fn.type_str.html) (borrowed, no free).
pub fn status() -> Result<String> {
    // SAFETY: returns null or a malloc'd C string.
    let ptr = unsafe { sys::switch_nat_status() };
    if ptr.is_null() {
        return Ok(String::new());
    }
    // SAFETY: `ptr` is a non-null malloc'd C string; copy out then free with libc free.
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    let s = String::from_utf8_lossy(bytes).into_owned();
    unsafe { libc_free(ptr as *mut _) };
    Ok(s)
}

/// The current active NAT mechanism (`"upnp"` / `"pmp"` / `"n/a"`), as a borrowed static C string.
///
/// Wraps `switch_nat_get_type`, which returns a const string that must not be freed.
pub fn type_str() -> Option<&'static std::ffi::CStr> {
    // SAFETY: returns null or a static const C string.
    let ptr = unsafe { sys::switch_nat_get_type() };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: `ptr` is null or a static const C string.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}

/// Re-publishes all existing mappings (e.g. after a NAT gateway change).
pub fn republish() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_republish() };
}

/// Re-initializes the NAT subsystem (full teardown + init).
pub fn reinit() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_reinit() };
}

/// Shuts the NAT subsystem down.
pub fn shutdown() {
    // SAFETY: no arguments.
    unsafe { sys::switch_nat_shutdown() };
}

/// Toggles whether automatic port mapping is performed.
pub fn set_mapping(enable: bool) {
    // SAFETY: `enable` is a valid switch_bool_t.
    unsafe { sys::switch_nat_set_mapping(bool_to_switch(enable)) };
}

fn bool_to_switch(b: bool) -> sys::switch_bool_t {
    if b {
        sys::switch_bool_t_SWITCH_TRUE
    } else {
        sys::switch_bool_t_SWITCH_FALSE
    }
}

// NAT status strings are malloc'd by FreeSWITCH; free with the matching libc allocator. Declared
// locally (same pattern as `console.rs`) to avoid a hard `libc` crate dependency.
unsafe extern "C" {
    fn free(ptr: *mut std::ffi::c_void);
}
#[allow(non_snake_case)]
unsafe fn libc_free(ptr: *mut std::ffi::c_void) {
    unsafe { free(ptr) };
}
```

### re-export 更新(`lib.rs`)

```rust
mod nat;
// ...
pub use nat::{NatIpProto, add_mapping, del_mapping, init, is_initialized, late_init, republish,
    reinit, set_mapping, shutdown, status, type_str};
```

### 风险点

- **`status()` 的 free 方式未证实**。注释已标注,默认实现假设 libc `free()`(与 FreeSWITCH
  大量 `switch_core_alloc` + 后续手动 `free` 的模式一致)。若实测发现 double-free/crash,需查
  `.c` 改用正确的释放函数。**保守起见,首版可不封 `status()`**,只封 `type_str()`(borrowed,
  零风险)。

---

## Patch G(必加)— callstate 字符串往返

### 背景

`CallState` enum 已封(`channel.rs`,`CCS_*`,`#[repr(transparent)]` newtype),但缺少与现有
`cause_to_str`/`str_to_cause` 配套的字符串往返。同时 `Channel::set_callstate` 方法(驱动
callstate 标记)也缺。`perform_presence`(触发 presence 事件)是 SIP presence 模块需要。

### 关键事实(已核查)

- **`CallState` 名字已被占用** —— 它就是 `switch_channel_callstate_t` 的封装(channel.rs:86),
  **不存在命名冲突**,新函数直接复用。
- `callstate2str`(参 callstate,返 `*const c_char` static)/`str2callstate`(参 str,返 callstate)。
- **`perform_set_callstate` 参数顺序是 `(channel, callstate, file, func, line)`** —— callstate 在
  file/func/line **之前**,与现有 `perform_set_state` 的 `(channel, file, func, line, state)`
  顺序**相反**。封 `Channel::set_callstate` 时务必按 bindings 实际顺序。
- `perform_set_callstate` 返回 **`void`**(不像 `perform_set_state` 返 state)。
- `perform_presence(channel, rpid, status, id, file, func, line)`:rpid/status/id 是 `*const c_char`,
  触发 presence 事件(SIP PUBLISH/NOTIFY)。返回 void。
- `callstate` vs `channel state`:callstate 是通话级 call-progress(CCS_*);channel state 是
  通道级状态机(CS_*)。`set_callstate` 只更新标记 + 触发事件/日志,**不驱动 CS_* 状态机**。

### 落点:`channel.rs`,接在 `cause_to_str` 之后、`bind_device_state_handler` 之前

现有 `cause_to_str` 结束于 channel.rs:1317,`bind_device_state_handler` 起于 1319,中间 1318
是空行。新函数插在此,与 cause 的 str 往返聚在一起。

### 代码

#### G.1 字符串往返(自由函数,对齐 `cause_to_str`/`str_to_cause`)

```rust
/// Translates a call-state name (e.g. `"ringing"`) into a [`CallState`].
pub fn str_to_callstate(name: impl AsRef<str>) -> Result<CallState> {
    let name = cstring(name)?;
    // SAFETY: `name` is a valid C string for the call.
    Ok(CallState::from_raw(unsafe {
        sys::switch_channel_str2callstate(name.as_ptr())
    }))
}

/// Translates a [`CallState`] into its canonical name. The returned string borrows static storage.
pub fn callstate_to_str(state: CallState) -> Option<&'static str> {
    // SAFETY: `switch_channel_callstate2str` returns a static string literal.
    let ptr = unsafe { sys::switch_channel_callstate2str(state.raw()) };
    // SAFETY: `ptr` is null or a static null-terminated string.
    unsafe { borrowed_cstr_to_str(ptr) }
}
```

(`borrowed_cstr_to_str` 和 `cstring` 已在 channel.rs 顶部 import,无需新增。)

#### G.2 `Channel::set_callstate` 方法

加进 `impl Channel` 块(紧挨现有 `set_state` 方法之后,channel.rs:251 附近):

```rust
/// Sets the channel's call-state (`CCS_*`) and fires the associated event/log.
///
/// This updates the call-progress marker only; it does **not** drive the channel state machine
/// (`CS_*`) — that is [`set_state`](Self::set_state)'s job.
pub fn set_callstate(self, state: CallState) {
    // SAFETY: `self.raw` is a live channel; `state.raw()` is a valid callstate; the source
    // strings are static C strings. Note arg order: (channel, callstate, file, func, line) —
    // callstate precedes the locator, unlike `switch_channel_perform_set_state`.
    unsafe {
        sys::switch_channel_perform_set_callstate(
            self.raw.as_ptr(),
            state.raw(),
            c"fswtch-rs".as_ptr(),
            c"Channel::set_callstate".as_ptr(),
            line!() as _,
        )
    };
}
```

#### G.3 `Channel::presence` 方法(可选,触发 presence 事件)

```rust
/// Fires a presence event for the channel (SIP PUBLISH/NOTIFY).
///
/// `rpid` is the RFC 3863 RPID icon hint, `status` the human-readable status text, and `id` the
/// presence identity. Any may be `None` (passed as null).
pub fn presence(
    self,
    rpid: Option<&str>,
    status: Option<&str>,
    id: Option<&str>,
) -> Result<()> {
    let rpid = match rpid {
        Some(s) => Some(cstring(s)?),
        None => None,
    };
    let status = match status {
        Some(s) => Some(cstring(s)?),
        None => None,
    };
    let id = match id {
        Some(s) => Some(cstring(s)?),
        None => None,
    };
    // SAFETY: `self.raw` is a live channel; the three string args are valid C strings or null
    // (null is permitted by the ABI for "unspecified"); locator strings are static.
    unsafe {
        sys::switch_channel_perform_presence(
            self.raw.as_ptr(),
            rpid.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            status.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            id.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            c"fswtch-rs".as_ptr(),
            c"Channel::presence".as_ptr(),
            line!() as _,
        );
    }
    Ok(())
}
```

### re-export 更新(`lib.rs`)

`channel` 那行加 `callstate_to_str`, `str_to_callstate`:

```rust
pub use channel::{
    CallState, Channel, ChannelFlag, bind_device_state_handler, callstate_to_str, cause_to_str,
    str_to_callstate, str_to_cause, unbind_device_state_handler,
};
```

(`Channel::set_callstate` / `Channel::presence` 是 `impl Channel` 方法,随 `Channel` 类型自动导出,
无需单独列。)

---

## 验收标准

对每个 patch:

1. **`cargo check -p fswtch --lib`** 无新增 warning。
2. **`cargo clippy -p fswtch --lib`** 无新增告警。
3. **`cargo fmt -p fswtch --check`** 对新文件/改动文件通过。
4. (Patch D/G)落点行号在实现时可能漂移,以"接在 X 之后、Y 之前"为准,不照搬本文档行号。

对 IPv6 validate(Patch D)与 NAT `status()`(Patch F)的两个未证实点,实现时必须先核查
bindings.rs 真实字段路径 / `.c` 的 free 方式,确认后再写;若无法确认,按文档"退路"处理
(只封 IPv4 / 不封 `status()`)。

---

## 实施顺序建议

按依赖与风险递增:

1. **Patch G**(callstate)—— 最小、最安全、纯复刻现有 `cause_to_str` 模式,先做暖手。
2. **Patch C**(hupall)—— 引入一个 `HupType` bitmask,但逻辑直白,依赖的 `Cause`/`Event`/
   `EndpointInterface` 都已就绪。
3. **Patch E**(core_db)—— 中等,`get_table` 的 RAII guard 稍复杂但模式清晰。
4. **Patch D**(network_list)—— 最复杂,新模块 + 生命周期参数 `'pool` + union 字段验证。
5. **Patch F**(NAT)—— 可选,且有 `status()` free 方式的未证实风险,最后做或只做子集。

每个 patch 独立可提交(Patch C 的 `HupType` 与 hupall 函数在同一 patch 内,不拆)。
