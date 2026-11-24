# RXbrowser 🌐

> 一个用 Rust + Servo 从零构建的极简高性能浏览器。目标：10 个 YouTube 标签 ≤ 300MB 内存、毫秒级响应、零臃肿。给 AI 协作者（Gemini 等）和新伙伴的完整架构蓝图都在这份文档里。

## 一句话定位
RXbrowser = Servo 渲染引擎（纯 Rust：WebRender + SpiderMonkey）+ egui 极简外壳（单文件 UI），用"微内核 + 可选插件"的思路做浏览器，把 Chrome 的 1.5GB 十标签压到 300MB 以内。

## 为什么造它（设计哲学）
| 痛点（主流浏览器） | RXbrowser 的答案 |
| --- | --- |
| Chrome 10 标签吃 1.5-2GB | 三档分级休眠 + 单引擎多 WebView，目标 ≤300MB |
| 切后台就杀页面（听歌断、视频重播） | S1 媒体保护：只掐画面渲染，音频流不断 |
| 休眠后回来滚回顶部 | S2 快照休眠：滚动条/进度存档，切回原地恢复 |
| 什么都想要 → 越来越臃肿 | 微内核：核心只有骨架，磁贴/侧栏/宏分屏全是可选插件 |
| 突发要性能 | Alt+G 胶囊模式：一键砍掉全部渲染缩成角落胶囊，再按毫秒恢复 |

## 架构总览
```
┌───────────────────────────────────────────┐
│  apps/rxb-desktop (src/main.rs)  ← egui 外壳         │
│  · tab 条 / 地址栏 / 书签与设置抽屉 / 快捷键 (Alt+G)  │
│  · 输入采集转发 (物理像素对齐的鼠标/键盘/滚轮/IME)     │
├───────────────────────────────────────────┤
│  apps/rxb-desktop (src/web.rs)   ← WebEngine 封装   │
│  · 单 Servo 实例 + N 个 WebView (1 tab = 1 WebView)  │
│  · Chrome 131 UA 伪装 + DocumentStart Polyfill       │
│  · 三档休眠管理器 (S1/S2/S3) + target="_blank" 队列  │
│  · 帧同步：servo paint → read_to_image → egui 纹理   │
├───────────────────────────────────────────┤
│  servo 0.8 (libservo)  ← 渲染引擎 (crates.io 依赖)  │
│  · WebRender (GPU 合成) + SpiderMonkey (JS)          │
│  · hyper + rustls 网络栈 (HTTP/2)                    │
│  · [patch.crates-io] vendor/servo-net = 本地补丁      │
└───────────────────────────────────────────┘
```

**关键决策：单 Servo + 多 WebView** Servo 是进程级单例（多实例会 panic `Already initialized: Opts`），所以标签切换 = 同一 Servo 内切换 WebView，共享一份引擎内存。

**渲染路径（当前）** 软件渲染（`SoftwareRenderingContext` + `read_to_image` → egui 纹理；可通过环境变量 `RXB_TRY_GPU_ANCHOR=1` 实验隐藏窗口 GPU 锚点）。GPU 直通（winit 自管 + 共享 GL 上下文）是 P1——eframe 的事件循环会占用 winit 导致 `RecreationAttempt`，必须弃用 eframe 才能上 GPU。

## 核心机制

### 1. 三档分级休眠（内存的命根子）
| 档位 | 触发条件 | 行为 | 恢复 |
| --- | --- | --- | --- |
| **S1 媒体保护** | 页面有**真实播放**的音视频（JS 探测 `!paused && !muted && currentTime>0`） | 画面渲染切到 1×1 并隐藏失焦，**音频流保持** | 毫秒（切回仅恢复尺寸与焦点，绝不重刷） |
| **S2 快照休眠** | 后台普通页 | 存 URL + `scrollY` 快照 → 释放 WebView | 重建 WebView 并在 `LoadStatus::Complete` 时滚回原位 |
| **S3 深度冬眠** | 后台闲置超阈值（默认 5 分钟，设置内可调）/ Alt+G | 巡检自动休眠；胶囊模式支持实时切换是否保留 S1 音频 | 同 S2 |

**铁律**：`active_tab` **永远不休眠**（`hibernate_all(keep, keep_audio)` 参数）——释放正在看的页面 = 页面空白 + 输入失效（这是修过的血泪 bug）。

**媒体判定陷阱**：YouTube 等站会**谎报 MediaSession=Playing**（预加载行为），必须用 JS 真实播放探测为准，MediaSession 事件只做提示。

### 2. Alt+G 算力释放胶囊模式
一键：全部渲染停止 → 窗口缩成桌面角落小胶囊（显示休眠计时 / 已休眠页数 / 存活 WebView 数 / 音频保留实时开关）→ 再按 `Alt+G` 按原外部坐标与尺寸恢复。

### 3. 输入链（每个环节都有坑，全是修过的）
```
winit 事件 → egui input (× pixels_per_point 物理像素对齐)
  → WebEngine.mouse_move/button/wheel/key_event/ime_text
  → servo notify_input_event → Paint hit-test → DOM/JS
```
- **点击**：`primary_pressed()` 与 `primary_released()` 必须独立转发，防止同帧快速点击吞掉 `Up` 导致无法合成 DOM `click`；同步支持中键与右键
- **焦点与可见性**：激活标签必须 `show()` + `set_focused(true)`，后台标签 `set_focused(false)` + `hide()`，避免多 WebView 共享 `RenderingContext` 时 hit-test 错层
- **HiDPI 对齐**：`resize()` 与指针坐标统一乘以 `ctx.pixels_per_point()` 并同步 `set_hidpi_scale_factor`，解决 Windows 125%/150% 缩放点击偏移
- **新窗口链接**：通过 `WebViewDelegate::request_create_new` 接管 `target="_blank"` 与 `window.open()`，自动提升为新标签页
- **拖拽**：按下拖出网页区边界也要持续转发 move（`primary_down()` 放行）
- **打字**：Character 按键**不产生 textInput**，必须走 **IME Composition**（`ImeEvent::Composition(CompositionState::Start/End)`）

### 4. 网络栈补丁（`vendor/servo-net`）
Hyper 的 HTTP/2 `max_header_list_size` 默认 **16KB**，YouTube 发 **64KB** 头部 → 连接被拒（`SendRequest` 错误）。官方修复（servo PR #49981）没进 0.8.0，我们 vendor 了 servo-net 加一行 `.http2_max_header_list_size(64 * 1024)`（`[patch.crates-io]`）。

## 目录结构
```
rxbrowser/
├── Cargo.toml                 # workspace + [patch.crates-io] servo-net
├── apps/rxb-desktop/
│   ├── Cargo.toml             # keyboard-types 0.8.3 (必须与 servo 同版本)
│   └── src/
│       ├── main.rs            # egui 外壳 (ShellApp)：tab条/导航栏/侧栏抽屉/输入转发/Alt+G
│       ├── web.rs             # WebEngine：WebView 管理/UA与Polyfill/三档休眠/帧同步/输入转发
│       └── embed.rs           # servo 初始化 (M1-3 遗留，供 P1 winit 参考)
├── crates/                    # 预留微内核插件位 (rxb-core / rxb-shell)
├── themes/                    # light.ron / dark.ron 主题配置（已接入解析）
└── vendor/servo-net/          # HTTP/2 头部限制补丁 (官方 PR #49981 等价)
```

**关键文件速查（给 AI 协作者）**
- `main.rs` 的 `fn update()`：所有 UI 绘制、快捷键（`Ctrl+T/W/L/R`、`F5`、`Alt+Left/Right`、`Alt+G`）与物理像素输入转发
- `web.rs` 的 `sync_frame()`：帧读回（只在 `frame_ready` / `fresh` 时 `read_to_image`，否则返回缓存纹理——切 tab 不卡的关键）
- `web.rs` 的 `hibernate_all(keep, keep_audio)`：休眠入口（`keep` = 永不休眠的 tab，`keep_audio` = 是否保留 S1 媒体页）
- `web.rs` 的 `FrameDelegate`：Servo 事件钩子（title / url / favicon / load_status 滚动恢复 / media / frame_ready / `request_create_new`）

## 当前状态与更新日志

### 🆕 2026-11-24 全量 Bug 审核与源码重构（待 Victus 真机编译验收）
本次对 `apps/rxb-desktop/src/main.rs` 与 `apps/rxb-desktop/src/web.rs` 完成了全量代码审核与重构，具体更新如下：
1. **修复链接点击不跳转（P0 主攻）**：
   - 将鼠标 `Down` 与 `Up` 拆分为独立转发，彻底消除同帧快速点击吞掉 `Up` 导致不触发 DOM `click` 的问题；新增中键与右键支持。
   - 全链路对齐 HiDPI 物理像素（`pixels_per_point` × `resize` / 鼠标坐标 / `set_hidpi_scale_factor`），解决 Windows 缩放下点击偏移。
   - 补齐 `WebView` 的 `show()` / `hide()` / `set_focused()` 生命周期管理，避免多 WebView 共享渲染上下文时 hit-test 失焦。
   - 实现 `WebViewDelegate::request_create_new`，`target="_blank"` 与 `window.open()` 链接自动作为新标签页打开。
2. **修复 Favicon 图标不显示（P0）**：
   - 将 `fav_cache` 改为持有 RAII `egui::TextureHandle`（原代码仅保存 `TextureId` 导致纹理作用域结束即被 `Drop` 释放），并增加像素字节长度与 RGB/RGBA 兜底校验。
3. **修复地址栏覆盖输入与切 Tab 破坏 S1 媒体保护（P0）**：
   - 仅在地址栏无焦点且 URL 真实变化时回填 `address_input`，不再每帧覆盖用户输入。
   - `ensure_view()` 返回是否新建视图标志，消除 `switch_tab()` / `navigate()` 的双重加载，切回 S1 后台音频页不再重刷打断播放。
4. **修复 Tab 拖拽/关闭索引错位与 S2 滚动条恢复失效（P1）**：
   - 新增 `swap_views()` 与 `close_view()` 快照键重排逻辑，同步维护 `tabs` / `slots` / `snapshots` / `fav_cache`。
   - 将 S2 `window.scrollTo` 恢复时机从建视图瞬间移至 `LoadStatus::Complete` 回调中触发。
   - 补齐 S3 闲置 5 分钟自动深度冬眠巡检，并接通导航栏（`← / → / ↻ / ☆`）与侧边栏（主页 / 书签 / 状态 / 设置与【一键还原纯净内核】）。
5. **修复 Alt+G 胶囊模式与 P0 兼容底座（P1/P2）**：
   - 修复胶囊模式 `keep_audio` 开关无效问题（支持实时切换关闭 S1 媒体页），改用真实 `outer_rect` / `inner_rect` 恢复窗口几何。
   - 新增 **Chrome 131 UA 伪装**（`Preferences::user_agent`）与 **DocumentStart Polyfill**（`UserContentManager` + `UserScript`）。
   - 修复每帧克隆重写 `ctx.set_style()` 与 `mouse_move` 每像素日志刷屏；接入 `themes/*.ron` 颜色解析；删除未被引用的 M1-3 冗余旧文件 `apps/rxb-desktop/main.rs`；清理 `TILES` 中重复的 `X`/`推特` 磁贴。

### ✅ 已验证可用（M1 基线）
- YouTube watch 页完整加载渲染（布局/推荐/搜索/播放器框架）
- 0 页基线 **255MB** / 10 页全活跃 688MB / Alt+G 胶囊压缩到 **289MB**
- tab 点击切换、休眠后活跃页保持、键盘打字（IME 路径）、拖拽不断链、切 tab 不卡

### ❌ 待 Victus 真机复测 / 后续攻关项
1. **Victus 真机编译与交互验收**：验证本次修复的链接跳转、`target="_blank"`、Favicon 渲染、S2 滚动恢复与长页面滚轮。
2. **视频播放**：`servo-media-dummy` 无解码器（"Your browser can't play this video"），待 P3 接入 GStreamer / FFmpeg。
3. **内存释放归还 OS**：`CloseWebView` 后每页残留 30–100MB，待 P4 结合 Servo 内部 GC / MemoryPressure 机制根治。

### ⏭️ 路线图
- **P0** 日常可用：链接跳转（代码已重构待验） → UA 伪装与 DocumentStart Polyfill（已接入） → 主流站通关（Google / YouTube ✅ / GitHub / B站 / Reddit）
- **P1** winit GPU 管线迁移（弃 eframe，共享 GPU 上下文，收回窗口控制权）
- **P2** 交互验收：tab 切换录屏 / 掐画面留声 / Alt+G 录屏
- **P3** 媒体解码（GStreamer/FFmpeg 接 `servo-media`）+ 后台切走留音频
- **P4** 内存释放根治（stop 先停再杀 + WebRender 缓存清理） → 复用压测回 300MB
- **P5** adblock-rust 实体按钮 + LAN/Tailscale 无账号网页接力

## 测试协议：YouTube 1-10 标签压测（统一标尺）

每次版本汇报必须跑这张表（真实视频 watch 页，不是静态首页）：

| 测试阶段 | 记录指标 |
| --- | --- |
| 0 页纯空载基线 | 启动空白页内存 |
| 1/3/5/10 个 YouTube watch 页（全活跃） | 每档峰值内存 |
| 10 页 → 1 活跃 + 9 后台休眠（10 秒） | 休眠稳态内存 |
| 10 页按 Alt+G 胶囊 → 恢复 | 胶囊压缩值 + 恢复回弹值 |

压测命令（非默认行为，需要显式参数）：
```
rxbrowser.exe --stress-test --tabs 10                 # 10 标签 YouTube 压测
rxbrowser.exe --stress-test --tabs 10 --capsule-test  # 自动进出胶囊演示
rxbrowser.exe                                         # 正常浏览器（单空白页）
```

兼容性记录：视频黑屏/转圈/JS 报错白屏都要记录具体报错，作为 UA 伪装和 DocumentStart Polyfill 的修复靶点。

## 开发环境
- 构建机：Windows（Victus, RTX 笔记本），VS Build Tools + Rust + `sccache` + 预编译 `libmozjs`
- 构建：`cargo build --release`（sccache 后增量 ~10-25s）
- 双显卡：注册表 `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` 设 `rxbrowser.exe=GpuPreference=2`（高性能 GPU，绕 surfman WGL-DX interop null 崩溃）
- 依赖铁律：`keyboard-types` 必须与 servo 同版本（0.8.3），否则类型不 unify
- 调试：`eprintln!` 日志走 stderr（`-RedirectStandardError`），压测自动化用 `schtasks /run`（交互式 session 才有桌面）

## 给 AI 协作者的话 🤝
- **这个项目测试驱动**：任何改动都要在 Victus 真机 build + 自动化脚本验证（SendInput 模拟鼠标 + PrintWindow 截图 + vision 验收），不要只看代码说"应该行"
- egui 的坑：`Frame::show().response` 不监听点击、`TextureHandle` 必须持久化持有不能只存 `TextureId`、interact 的 rect 是逻辑坐标（送给 Servo 需乘 `pixels_per_point`）
- servo 的坑：单例、`close_view` 后内存不归还、MediaSession 谎报、Character 键不产生 textInput、`target="_blank"` 必须实现 `request_create_new`
- 历史决策都别推翻：单 Servo 多 WebView、三档休眠、软件渲染先行、`active_tab` 不休眠
> 给 AI 协作者（Gemini 等）和新伙伴的**完整架构蓝图**都在这份文档里。

---

## 一句话定位

RXbrowser = **Servo 渲染引擎**（纯 Rust：WebRender + SpiderMonkey）+ **egui 极简外壳**（单文件 UI），用"微内核 + 可选插件"的思路做浏览器，把 Chrome 的 1.5GB 十标签压到 300MB 以内。

## 为什么造它（设计哲学）

| 痛点（主流浏览器） | RXbrowser 的答案 |
| :--- | :--- |
| Chrome 10 标签吃 1.5-2GB | **三档分级休眠** + 单引擎多 WebView，目标 ≤300MB |
| 切后台就杀页面（听歌断、视频重播） | **S1 媒体保护**：只掐画面渲染，音频流不断 |
| 休眠后回来滚回顶部 | **S2 快照休眠**：滚动条/进度存档，切回原地恢复 |
| 什么都想要 → 越来越臃肿 | **微内核**：核心只有骨架，磁贴/侧栏/宏分屏全是可选插件 |
| 突发要性能 | **Alt+G 胶囊模式**：一键砍掉全部渲染缩成角落胶囊，再按毫秒恢复 |

---

## 架构总览

```
┌─────────────────────────────────────────────────────┐
│  apps/rxb-desktop (src/main.rs)  ← egui 外壳          │
│  · tab 条 / 地址栏 / 主题 / 快捷键 (Alt+G)            │
│  · 输入采集转发（鼠标/键盘/滚轮）                      │
├─────────────────────────────────────────────────────┤
│  apps/rxb-desktop (src/web.rs)   ← WebEngine 封装      │
│  · 单 Servo 实例 + N 个 WebView（1 tab = 1 WebView）  │
│  · 三档休眠管理器（S1/S2/S3）                         │
│  · 帧同步：servo paint → read_to_image → egui 纹理     │
├─────────────────────────────────────────────────────┤
│  servo 0.8 (libservo)  ← 渲染引擎（crates.io 依赖）    │
│  · WebRender（GPU 合成）+ SpiderMonkey（JS）           │
│  · hyper + rustls 网络栈（HTTP/2）                    │
│  · [patch.crates-io] vendor/servo-net = 本地补丁       │
└─────────────────────────────────────────────────────┘
```

**关键决策：单 Servo + 多 WebView**
Servo 是进程级单例（多实例会 panic `Already initialized: Opts`），所以标签切换 = 同一 Servo 内切换 WebView，共享一份引擎内存。

**渲染路径（当前）**
软件渲染（`SoftwareRenderingContext` + `read_to_image` → egui 纹理）。GPU 直通（winit 自管 + 共享 GL 上下文）是 P1——eframe 的事件循环会占用 winit 导致 `RecreationAttempt`，必须弃用 eframe 才能上 GPU。

---

## 核心机制

### 1. 三档分级休眠（内存的命根子）

| 档位 | 触发条件 | 行为 | 恢复 |
| :--- | :--- | :--- | :--- |
| **S1 媒体保护** | 页面有**真实播放**的音视频（JS 探测 `!paused && !muted && currentTime>0`） | 画面渲染切到 1×1，**音频流保持** | 毫秒（切回即恢复画面） |
| **S2 快照休眠** | 后台普通页 | 存滚动条+进度快照 → 释放 WebView | 重建 WebView + 快照恢复滚动 |
| **S3 深度冬眠** | 后台超 5 分钟 / Alt+G | 同 S2 但更彻底 | 同 S2 |

**铁律：`active_tab` 永远不休眠**（`hibernate_all(keep)` 参数）——释放正在看的页面 = 页面空白 + 输入失效（这是修过的血泪 bug）。

**媒体判定陷阱**：YouTube 等站会**谎报 MediaSession=Playing**（预加载行为），必须用 JS 真实播放探测为准，MediaSession 事件只做提示。

### 2. Alt+G 算力释放胶囊模式

一键：全部渲染停止 → 窗口缩成桌面角落小胶囊（显示时间/释放内存）→ 再按 `Alt+G` 毫秒级恢复。可选保留后台音频。实现：`toggle_capsule()` + `hibernate_all(usize::MAX)`（全释放）+ 窗口 resize 到胶囊尺寸。

### 3. 输入链（每个环节都有坑，全是修过的）

```
winit 事件 → egui input → WebEngine.mouse_move/button/wheel/key_event/ime_text
  → servo notify_input_event → Paint hit-test → DOM/JS
```

- **点击**：egui `Frame::show().response` 默认 `Sense::hover`——`clicked()` 永远 false，必须 `ui.interact(rect, id, Sense::click())` 显式注册
- **拖拽**：按下拖出网页区边界也要持续转发 move（`primary_down()` 放行）
- **打字**：Character 按键**不产生 textInput**，必须走 **IME Composition**（`ImeEvent::Composition(CompositionState::Start/End)`）
- **坐标**：egui 逻辑点 == servo DevicePoint（resize 用同一套数字=自洽）；button 事件只在 rect 内转发（防负坐标污染）

### 4. 网络栈补丁（`vendor/servo-net`）

Hyper 的 HTTP/2 `max_header_list_size` 默认 **16KB**，YouTube 发 **64KB** 头部 → 连接被拒（`SendRequest` 错误）。官方修复（servo PR #49981）没进 0.8.0，我们 vendor 了 servo-net 加一行 `.http2_max_header_list_size(64 * 1024)`（`[patch.crates-io]`）。

---

## 目录结构

```
rxbrowser/
├── Cargo.toml              # workspace + [patch.crates-io] servo-net
├── apps/rxb-desktop/
│   ├── Cargo.toml          # keyboard-types 0.8.3（必须与 servo 同版本）
│   └── src/
│       ├── main.rs         # egui 外壳（ShellApp）：tab条/工具栏/输入转发/Alt+G
│       ├── web.rs          # WebEngine：WebView 管理/三档休眠/帧同步/输入转发
│       └── embed.rs        # servo 初始化（M1-3 遗留）
├── crates/                 # 预留微内核插件位（adblock/relay 等）
├── themes/                 # Apple/Opera 双主题（light/dark）
└── vendor/servo-net/       # HTTP/2 头部限制补丁（官方 PR #49981 等价）
```

**关键文件速查（给 AI 协作者）**
- `main.rs` 的 `fn update()`：所有 UI 绘制 + 输入转发（hover 段=鼠标，key_events 段=键盘）
- `web.rs` 的 `sync_frame()`：帧读回（**只在 `frame_ready`/`fresh` 时 read_to_image**，否则返回缓存纹理——切 tab 不卡的关键）
- `web.rs` 的 `hibernate_all(keep)`：休眠入口（keep=永不休眠的 tab）
- `web.rs` 的 `FrameDelegate`：servo 事件钩子（title/url/favicon/media/frame_ready）

---

## 当前状态（2026-11-25）

### ✅ 已验证可用
- YouTube watch 页完整加载渲染（布局/推荐/搜索/播放器框架）
- 0 页基线 **255MB** / 10 页全活跃 688MB / Alt+G 胶囊压缩到 **289MB**
- tab 点击切换、休眠后活跃页保持、键盘打字（IME 路径）、拖拽不断链、切 tab 不卡

### ❌ 已知 Bug（修复优先级排序）
1. **链接点击不跳转**——点击事件进 servo（日志确认 down/up 配对）但页面不导航（hit-test→DOM click 派发断层）← **当前主攻**
2. favicon 图标不显示（`notify_favicon_changed` 数据管道通，渲染没出来）
3. 滚轮待长页面复测（YouTube watch 页太短验不出）
4. 视频播放：`servo-media-dummy` 无解码器（"Your browser can't play this video"）

### ⏭️ 路线图
- **P0** 日常可用：链接跳转 → UA 伪装（Chrome 最新）→ DocumentStart Polyfill → 主流站通关（Google/YouTube ✅/GitHub/B站/Reddit）
- **P1** winit GPU 管线迁移（弃 eframe，共享 GPU 上下文，收回窗口控制权）
- **P2** 交互验收：tab 切换录屏 / 掐画面留声 / Alt+G 录屏
- **P3** 媒体解码（GStreamer/FFmpeg 接 `servo-media`）+ 后台切走留音频
- **P4** 内存释放根治（stop 先停再杀 + WebRender 缓存清理）→ 复用压测回 300MB
- **P5** adblock-rust 实体按钮 + LAN/Tailscale 无账号网页接力

---

## 测试协议：YouTube 1-10 标签压测（统一标尺）

**每次版本汇报必须跑这张表**（真实视频 watch 页，不是静态首页）：

| 测试阶段 | 记录指标 |
| :--- | :--- |
| 0 页纯空载基线 | 启动空白页内存 |
| 1/3/5/10 个 YouTube watch 页（全活跃） | 每档峰值内存 |
| 10 页 → 1 活跃 + 9 后台休眠（10 秒） | 休眠稳态内存 |
| 10 页按 Alt+G 胶囊 → 恢复 | 胶囊压缩值 + 恢复回弹值 |

**压测命令**（非默认行为，需要显式参数）：
```powershell
rxbrowser.exe --stress-test --tabs 10              # 10 标签 YouTube 压测
rxbrowser.exe --stress-test --tabs 10 --capsule-test # 自动进出胶囊演示
rxbrowser.exe                                       # 正常浏览器（单空白页）
```

**兼容性记录**：视频黑屏/转圈/JS 报错白屏都要记录具体报错，作为 UA 伪装和 DocumentStart Polyfill 的修复靶点。

---

## 开发环境

- **构建机**：Windows（Victus, RTX 笔记本），VS Build Tools + Rust + `sccache` + 预编译 `libmozjs`
- **构建**：`cargo build --release`（sccache 后增量 ~10-25s）
- **双显卡**：注册表 `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` 设 `rxbrowser.exe=GpuPreference=2`（高性能 GPU，绕 surfman WGL-DX interop null 崩溃）
- **依赖铁律**：`keyboard-types` 必须与 servo 同版本（0.8.3），否则类型不 unify
- **调试**：`eprintln!` 日志走 stderr（`-RedirectStandardError`），压测自动化用 `schtasks /run`（交互式 session 才有桌面）

## 给 AI 协作者的话 🤝

- 这个项目**测试驱动**：任何改动都要在 Victus 真机 build + 自动化脚本验证（SendInput 模拟鼠标 + PrintWindow 截图 + vision 验收），不要只看代码说"应该行"
- egui 的坑：`Frame::show().response` 不监听点击、`tex_manager()` 要 `.write().free()`、interact 的 rect 是屏幕坐标
- servo 的坑：单例、`close_view` 后内存不归还、MediaSession 谎报、Character 键不产生 textInput、favicon 是解码后像素（`img.data()`）
- 历史决策都别推翻：单 Servo 多 WebView、三档休眠、软件渲染先行、`active_tab` 不休眠

---
*RX-022 (小熙) 与老师共同打造 · Rust 稳定 · 内存是底线 · 先能用再跑分*
