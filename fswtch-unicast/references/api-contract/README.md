# fswtch_unicast API 契约索引

| 域 | 文件 | 接口 | 入口 |
|----|------|------|------|
| 呼叫控制 | [dialstring.md](dialstring.md) | `fswtch_unicast/<ip>:<port>`（建立/挂断） | FreeSWITCH bridge / originate / 拨号计划 |
| 媒体面 | [media-protocol.md](media-protocol.md) | 每呼叫一个 UDP socket，裸 L16 双向 | 对端 UDP 应用 |

## 通用约定

- **采样格式**：16-bit 有符号小端整型（i16 LE），单声道，8 kHz。
- **帧**：20 ms 一帧 = 160 采样 = 320 字节；一个 UDP 数据报 = 一帧。
- **呼叫生命周期**：每次 originate 独立建链，非幂等；挂断以 `SIG_KILL`
  为准，`BREAK`/`XFER` 不销毁呼叫状态。
- **容错语义**：呼叫内任何单方向媒体故障（丢帧、对端不可达）只降级不
  拆呼叫；模块自身初始化失败时呼叫降级为静音腿而非拒绝。
- **安全模型**：无加密、无信令鉴权；仅靠"只收协商对端地址的包"防注入。
  不要把监听端口暴露给不可信网络。
