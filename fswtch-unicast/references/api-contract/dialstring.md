# 呼叫接口契约：`fswtch_unicast/<ip>:<port>`

## outgoing — 建立一路到 UDP 对端的 B-leg

**Behavior**: 解析 destination_number 中的 `<ip>:<port>` → 创建 B-leg
session → 标记应答并安装 L16 8 kHz/20 ms 单声道编解码 → 状态机推入媒体
交换态 → 绑动态本地 UDP 端口并起收/发任务 → 呼叫进入双向媒体。

**Side effects**: 新 session、新 UDP socket（`0.0.0.0` 动态端口）、两个
后台异步任务、一条全局呼叫状态；挂断时全部回收。

**Idempotent**: 否。每次 originate 都产生新呼叫与新动态端口。

### 调用示例

拨号计划：

```xml
<action application="bridge" data="fswtch_unicast/192.168.1.10:5000"/>
```

fs_cli 直接发起（`&echo` 换成你的下游应用）：

```bash
fs_cli -x "originate fswtch_unicast/192.168.1.10:5000 &echo"
# → +OK <uuid>            （成功，返回 B-leg 的 channel uuid）
# → -ERR <cause>          （失败，见错误表）
```

sendmsg：

```
sendmsg
execute
bridge
fswtch_unicast/192.168.1.10:5000
```

### dialstring 字段

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| 前缀 | `fswtch_unicast/` | 建议带 | 缺失时按裸地址容忍解析 |
| ip | IPv4 地址 | ✅ | 当前只支持 IPv4 对端（本地 socket 绑 `0.0.0.0`） |
| port | u16 | ✅ | 对端 UDP 监听端口 |

整个 dialstring 必须能解析为 `SocketAddr`（如
`fswtch_unicast/10.0.0.2:9000`）；解析失败直接拒绝呼叫。

### 呼叫结果

| 结果 | 含义 |
|------|------|
| `+OK <uuid>` | B-leg 建立并应答，媒体通道就绪 |
| `-ERR CHAN_NOT_IMPLEMENTED` | endpoint 不存在：模块未加载 |
| `-ERR REQUESTED_CHAN_UNAVAIL` | dialstring 解析失败、profile 缺失或模块内部异常 |
| `-ERR NORMAL_CLEARING` 等 | 对端/下游正常拒绝或挂机 |

特殊情形：dialstring 合法、session 建立成功，但 UDP 初始化失败（如
socket 耗尽）时，模块**不拒绝**——呼叫照常建立并降级为静音腿（收静音、
发丢弃），日志有 `degraded (silent) media` 错误。这是刻意设计：拒绝会让
未启动的 session 泄漏。

### 挂断语义

| 信号 | 行为 |
|------|------|
| `SIG_KILL`(1) | 移除呼叫状态：中止收/发任务、关闭 socket |
| `XFER`(2) / `BREAK`(3) | 媒体控制信号，保留呼叫状态，不清理 |
| 其他 teardown（session 被销毁但未发 SIG_KILL） | 后台回收器 10 s 周期兜底清理 |

### 错误与恢复

| 错误 | 触发 | 恢复 |
|------|------|------|
| `CHAN_NOT_IMPLEMENTED` | 模块未 load | `fs_cli -x "load fswtch_unicast"` |
| `REQUESTED_CHAN_UNAVAIL` | 地址串不是合法 `SocketAddr` | 修正 dialstring 后重呼 |
| 日志 `UDP bind failed` / 静音腿 | 系统 UDP socket 耗尽 | 查 `ulimit -n` / 端口范围；重呼 |

### 注意事项

- 对端必须能路由到本机，且**从同一 `<ip>:<port>` 回包**（源过滤，见
  media-protocol.md）。
- 动态本地端口：对端通过观察来包源地址获知，或看建链日志
  `created session <uuid> remote=<addr>`。
- 呼叫期间模块拒绝 `unload`（`Module in use.`），挂光后重试。
