# fswtch_unicast 使用说明书

## 概述

`fswtch_unicast` 是一个 FreeSWITCH endpoint 模块：把一路通话的媒体桥接到
**单个 UDP 端口上的裸 PCM 流**。呼叫 `fswtch_unicast/<ip>:<port>` 时，模块
创建一条 B-leg，主叫音频以小端 i16 PCM 经 UDP 发往 `<ip>:<port>`，同一
socket 上收到的 PCM 回播给主叫。无帧头、无信令——纯粹的媒体桥，等价于
单声道 UDP 版的 `mod_unicast`。

## 功能域

| 域 | 入口 | 契约 | 说明 |
|----|------|------|------|
| 呼叫接口 | dialstring `fswtch_unicast/<ip>:<port>` | [dialstring](references/api-contract/dialstring.md) | bridge / originate / 拨号计划建立 B-leg |
| 媒体协议 | UDP socket（每呼叫一个） | [media-protocol](references/api-contract/media-protocol.md) | 裸 L16 双向流、源过滤、背压与错误恢复语义 |
| 模块管理 | `fs_cli` / `modules.conf.xml` | 本文"模块管理"一节 | 加载、卸载、日志控制 |

## 快速上手

构建并安装（输出名必须是 `fswtch_unicast.so`，不能带 `mod_` 前缀——
FreeSWITCH 按 `<basename>_module_interface` 推导符号名）：

```bash
cargo build -p fswtch-unicast --release
sudo cp target/release/libfswtch_unicast.so /usr/lib/freeswitch/mod/fswtch_unicast.so
# macOS 上产物是 libfswtch_unicast.dylib，安装时改名为 fswtch_unicast.so
```

加载并验证：

```bash
fs_cli -x "load fswtch_unicast"
fs_cli -x "show modules" | grep fswtch_unicast
```

发起一路到 UDP 回环对端的呼叫，跑端到端自校验（见 `examples/`）：

```bash
python3 examples/udp_peer_verify.py        # 全过则 EXIT=0
```

## 呼叫工作流程

```
主叫腿                 FreeSWITCH core              fswtch_unicast            UDP 对端
  │      bridge/originate      │                         │                      │
  │ ─────────────────────────► │ outgoing_channel        │                      │
  │                            │ ──────────────────────► │ 解析 <ip>:<port>     │
  │                            │                         │ 建 B-leg session     │
  │                            │                         │ 装 L16 8k/20ms 编解码 │
  │                            │                         │ 绑动态 UDP 端口       │
  │                            │                         │ 起收/发两个异步任务   │
  │                            │ ◄── success(session) ── │                      │
  │  ◄══════ 呼叫应答，进入媒体交换 ══════►                  │                      │
  │                            │ write_frame(主叫音频)    │                      │
  │                            │ ──────────────────────► │ 有界队列 → 发送任务 ─► │ PCM →
  │                            │ read_frame              │                      │
  │                            │ ◄────────────────────── │ 接收任务 → 暂存 → 填帧 │ ← PCM
  │                            │      （不足补静音）       │                      │
  │ ─── 挂机 ────────────────► │ kill_channel(SIG_KILL)  │                      │
  │                            │ ──────────────────────► │ 移除呼叫状态          │
  │                            │                         │ 中止任务、关 socket   │
```

三个生命周期阶段的要点：

- **建立**：dialstring 解析失败/无 profile → 直接拒绝；B-leg 建好后 UDP
  初始化失败 → 不拒绝，降级为静音腿（session 交由 FreeSWITCH 正常状态机
  回收），呼叫本身仍建立。
- **媒体交换**：每呼叫一个 UDP socket（绑 `0.0.0.0` 动态端口）；收发两个
  方向互相独立，单方向故障不拆呼叫。
- **挂断**：`SIG_KILL` 触发状态移除与资源回收；`BREAK`/`XFER` 等媒体控制
  信号不动状态。若 session 被绕过 `kill_channel` 销毁，后台回收器
  （10 s 周期）兜底清理孤儿呼叫状态。

## 关键概念

- **动态本地端口**：模块每呼叫绑一个临时 UDP 端口；对端通过观察来包的源
  地址获知（建链日志也会打印）。
- **源过滤**：只接受来自协商对端 `<ip>:<port>` 的包，其余直接丢弃——裸
  UDP 无鉴权，防止第三方注入音频。对端必须以监听端口为源端口回包。
- **非阻塞读**：`read_frame` 永不阻塞，数据不足补静音。因此"自驱动"应用
  （如 `echo`）会全速空转，属预期行为；真实 bridge 中由远端腿限速。
- **背压丢帧**：FreeSWITCH 媒体线程到异步发送任务之间是有界队列
  （256 帧 ≈ 5 s）。对端消费不过来时按帧丢弃并打节流警告，不阻塞媒体
  线程、不拆呼叫。
- **错误恢复**：UDP 收发遇错（如对端未监听导致的 ICMP/ECONNREFUSED）只
  记录节流日志，任务不死；对端恢复后媒体自行恢复。
- **呼叫非幂等**：每次 originate 都是新呼叫、新 socket、新动态端口。

## 模块管理

加载/卸载与日志：

```bash
fs_cli -x "load fswtch_unicast"      # 或写进 autoload_configs/modules.conf.xml: <load module="fswtch_unicast"/>
fs_cli -x "unload fswtch_unicast"    # 有存活呼叫或残留引用时会被拒: "Module in use."
RUST_LOG=fswtch_unicast=debug freeswitch -nc   # 默认 fswtch_unicast=info
```

日志经 tracing 桥接进 `freeswitch.log`。tracing subscriber 是进程级先到先
得：若其他 Rust 模块先加载，本模块默认 filter 不生效，用 `RUST_LOG` 显式
控制。卸载前请确认无存活呼叫。

**macOS 开发注意**：dyld 不会在 `unload` 后真正移除已加载镜像，同路径再
`load` 会拿到旧代码。开发迭代时要么重启 FreeSWITCH，要么从新路径加载
（如 `load /tmp/fswtch_unicast.so`）。

## 错误与恢复

| 现象 | 含义 | 恢复动作 |
|------|------|----------|
| originate `-ERR CHAN_NOT_IMPLEMENTED` | 模块未加载 | `load fswtch_unicast` |
| originate `-ERR REQUESTED_CHAN_UNAVAIL` | dialstring 地址非法、profile 缺失或模块内部 panic | 检查 dialstring 格式；查日志 |
| 日志 `CallState::new failed ... degraded (silent) media` | UDP 初始化失败（端口耗尽等极端情况），呼叫降级为静音腿 | 查系统 socket 资源；重呼 |
| 日志 `send channel full ... dropping frame`（节流） | 对端消费慢于 50 Hz，背压丢帧 | 检查对端读取能力；属预期保护 |
| 日志 `UDP recv/send error (consecutive: N)`（节流） | 对端不可达/重启，ICMP 反馈 | 对端恢复后自动恢复；无需操作 |
| `unload` 报 `Module in use.` | 仍有呼叫存活或引用未释放 | `show channels` 确认挂光后重试 |

## 交付物索引

- 呼叫接口契约：[references/api-contract/dialstring.md](references/api-contract/dialstring.md)
- 媒体协议契约：[references/api-contract/media-protocol.md](references/api-contract/media-protocol.md)
- 端到端自校验脚本：`examples/udp_peer_verify.py`
- 安装与拨号计划示例：`README.md`
