# AGENTS.md · AutoTappo 开发者与智能体架构指南

> 本文件为后续在此项目工作的 AI Agent 及人类开发者提供**核心上下文、逆向发现、技术规范、已实现功能清单与新功能路线图**。

---

## 1. 项目背景与定位

**AutoTappo** 是一款专为库洛游戏《鸣潮》（Wuthering Waves）官方桌面宠物挂件 **WuwaTappo**（鸣潮桌宠）打造的**超轻量、纯静默、高拟真后台连点与数据看板助手**。

### 核心体验准则
1. **绝对 0 鼠标干涉**：严禁任何物理鼠标指针抖动、位移或拖拽；
2. **完全后台无感**：用户在正常打字聊天、写代码、玩游戏、看视频时，桌宠在后台全速累计有效点击数；
3. **零快捷键与输入法冲突**：不抢占文本输入、不破坏 Shift 切换中英文状态、不打断鼠标划词；
4. **原生桌面极低内存占用**：Rust + Tauri v2 技术栈，窗口关闭时彻底销毁 WebView2 归还 200MB+ 内存，仅留轻量 Rust 原生线程在托盘常驻守护（参考 `CampusFlow` 托盘模型）；
5. **国内直连秒级更新检测**：优先直连 Gitee 国内开放 API，GitHub API 自动兜底。

---

## 2. 官方桌宠（WuwaTappo）逆向核心发现

通过对官方二进制文件（`WuwaTappo.exe`）及解压后的前端 JavaScript 逆向分析确认：

### 2.1 输入监听机制
- 官方使用 Rust `rdev` 库安装了全局底层输入钩子（`WH_KEYBOARD_LL` 与 `WH_MOUSE_LL`）；
- 当捕获到键鼠动作时，后端向 WebView 抛发 `key-press`、`key-release` 等事件；
- **计数有效性防刷规则**：
  - **长按单个按键不重复计数**（内部 `Set.has(e)` 查重），必须有明确的按键释放（`key-release`）或交替按键；
  - 官方底栏已自带总点击计数器，AutoTappo 无需重复跨进程解密其本地 `.dat` 文件，只需专注于安全发送与本次会话计数。

### 2.2 官方硬编码快捷键（严格避坑指南）
桌宠前端源码中写死了以下系统全局热键，**AutoTappo 严禁占用**：
- **`F7`**：关闭游戏模式 / 恢复鼠标交互；
- **`F8`**：**切换显示器（多屏幕跨屏调度）**（`switchDisplayMonitor.js: It = new Set(["F8", "f8"])`）；
- **`F9`**：**老板键（隐藏/呼出桌宠）**；
- **`Ctrl+Alt+P`**：鼠标穿透开关；
- **`Ctrl+Alt+1~5`**：播放已装备表情包。

> **规范**：AutoTappo 全局热键默认选用 **`F6`**，且提供全键盘自定义录制，但系统内部硬编码拦截 `F7`、`F8`、`F9` 防止冲突。

### 2.3 宝箱机制与窗口结构深度逆向（关键突破）
通过 Brotli 逆向解包 `WuwaTappo.exe` 内嵌前端资源（`index-D8GJxFG7.js`、`index-DmC0wUEw.js`、`index-BjTCKBy6.css`），全面查明宝箱触发、DOM 布局与窗口渲染机理：
- **宝箱解锁与出现阈值**：
  - 累计点击 `< 1000`（`As = 1e3`）：宝箱挂件完全隐藏；
  - `1000 <= 累计点击 < 2000`（`by = 2e3`）：单宝箱解锁（左侧优先，左侧在 CD 倒计时中则显示右侧）；
  - `累计点击 >= 2000`：双宝箱全部解锁。
- **状态维护与判定**：
  - 倒计时时间戳：`left_treasure_chest`、`right_treasure_chest`（Unix 毫秒）；
  - 冷却就绪条件：`qn = jt <= 0`（左就绪），`Un = $t <= 0`（右就绪）；
  - 冷却期挂载倒计时组件：`<mu :progress="gp" :text="mp" />`；
  - 冷却就绪后生成按钮：`<button class="reward-btn" onClick="zl('left'|'right')"></button>`。
- **DOM 挂载与坐标相对偏移**：
  - 左宝箱：`.reward-anchor--left`（CSS：`right: -36px; bottom: 24px;`）；
  - 右宝箱：`.reward-anchor--right`（CSS：`right: -46px; bottom: 72px;`）。
- **全屏透明图层与 GetWindowRgn 穿透机制（重大发现）**：
  - 官方桌宠的 `Tauri Window`（类名 `Tauri Window`，标题 `Wuwa Tappo`）实际上是一个**覆盖整个显示器全屏的透明窗口**（如 `(0, 0, 1769, 1179)`）；
  - 严禁使用普通的 `GetWindowRect` 来定位桌宠，否则只能得到全屏尺寸；
  - **动态定位机理**：官方通过 IPC `set_main_hit_region` 调用 Windows GDI `SetWindowRgn` 对窗口实施多边形裁切，仅桌宠身体及可交互按钮区域具有物理区域；
  - **AutoTappo 定位方案**：直接调用 Windows API `GetWindowRgn` / `GetRegionData` 读取桌宠的非穿透区域包围盒 `rcBound` 与各子矩形。无论用户如何拖拽桌宠、按 `F8` 切屏、调整桌宠缩放比例或系统 DPI 缩放，均可毫秒级 100% 自动精确锁定桌宠与宝箱的物理位置，无需任何人工死板坐标！
- **开箱行为安全红线（严禁伪造发包）**：
  - 官方开箱调用链为：`zl(place)` -> `openChest({ place, click_num })` -> `jsonpClient.post("api/frontend/chests/open", ...)`；
  - **绝对安全准则**：严禁在 AutoTappo 外部模拟 HTTP/jsonp 请求发包，避免 Cookie、Session Token 或设备指纹异常导致官方反作弊风控封号；
  - **安全落地准则**：必须通过 Windows 原生 UI/消息流在桌宠真实的 Hit Region 坐标区域触发轻量交互，由官方 WebView 自身执行正规鉴权请求。

### 2.4 自动开箱机制（实机验证结论 · 以此为准）

> 以下结论来自真机实测（100% / 150% / 175% 三种 DPI、单屏与双屏、桌宠左移/右下/右上多位置），优先级高于任何推测。

**① 真实点击接收窗口不是主窗口**

- 视觉层 `FindWindowW("Tauri Window", "Wuwa Tappo")` 带 `WS_EX_LAYERED | WS_EX_TRANSPARENT`，**永远点击穿透**，只负责渲染；
- 真正接收点击的是独立顶层窗口，类名 **`DesktopCatInput`**（owner 为 Tauri 主窗）；
- 必须用 `EnumWindows` 按类名匹配，`FindWindowW` 找不到它；主窗口是全屏尺寸，`GetWindowRect` 无法定位桌宠。

**② 坐标体系（三个必须遵守的换算）**

| 量 | 坐标系 | 说明 |
| :-- | :-- | :-- |
| `GetWindowRgn` / `GetRegionData` 矩形 | **窗口相对** | 必须加窗口原点才是屏幕坐标 |
| 窗口原点 | `GetWindowRect(DesktopCatInput)` | 多屏时可为负数（副屏实测 `(1920, −261)`） |
| `PostMessage` 的 lParam | **客户区坐标** | 无边框全屏窗口，客户区原点 == 窗口原点 |

**③ 区域是拼图式矩形并集，不是“一整块可点区”**

- 原始矩形是扫描线分解结果：锚点与桌宠本体相邻时会被**合并成一个矩形**；
- 因此不能靠“某个矩形更靠右”识别锚点，必须用**逐行右边界剖面 + 平台 + 与锚点同一批行的本体右缘（取众数）**；
- 房间里可能同时有多只猫（自己的 + 好友的），**必须先按 x 区间重叠聚类**再逐簇分析。

**④ 就绪状态无法从窗口区域判断（重要纠正）**

- 就绪（礼物按钮）与冷却（倒计时气泡）两种状态下**区域矩形完全相同**（都是 62×44 的锚点框），`WindowFromPoint` 也都返回 `DesktopCatInput`；
- 所以「用 `GetWindowRgn` 检测 `reward-btn` 碰撞体是否存在」**是错的**，早期版本文档的该说法已废弃；
- 正确做法是对锚点框做**像素采样判定**（见 ⑥）。

**⑤ 锚点几何由 CSS 比例常数唯一确定（跨分辨率/DPI 的根基）**

- 反解公式：`(锚点右缘 − 本体右缘) / 46`（右锚点）或 `/ 36`（左锚点）**必须等于** `锚点框高度 / 44`，三者解出同一个缩放系数 `s`；
- 实测 `s` 精确等于 `DPI / 96`（100%→1.0，150%→1.5，175%→1.773）；
- 自校验不通过就**拒绝点击**（官方改版时的安全失败路径）；
- 官方夹取桌宠位置时**为锚点预留了 `46×s` 的外伸余量**，所以贴屏幕右下角极限位置也不会裁切（实测贴边后几何不变）。

**⑥ 就绪判定的像素判据**

- 锚点框内**内缩 12%** 排除圆角与阴影，统计两个比例：
  - `warm`：`r>180 && (r-b)>45 && (r-g)>10`（礼物气泡为暖橙实心块）；
  - `white`：`r>235 && g>230 && b>215`（倒计时气泡为奶白色）；
- 实测分离度：就绪 `warm≈0.65 / white≈0.00`，冷却 `warm≈0.12 / white≈0.47~0.72`；
- 判据：`warm ≥ 0.30 && white < 0.15` → 就绪；`white ≥ 0.15` → 冷却；其余 → 未知（**未知一律不点**）。

**⑦ 权限：开箱必须与官方同等完整性**

- 官方 exe 清单写死 `requestedExecutionLevel level="requireAdministrator"`，**永远以管理员运行**；
- 挂机连点走 `keybd_event` 全局键盘事件（官方用 `WH_KEYBOARD_LL` 全局钩子接收），**不受 UIPI 拦截**，普通权限即可；
- 开箱必须把消息**定向发送到具体 HWND**（`PostMessage`），跨完整性等级会被 UIPI 拒绝（`GetLastError = 5`），**所以必须提权**；
- 开箱没有键盘后路（官方未提供开箱热键）；
- **本项目自 v0.2.0 起在可执行文件中嵌入 `requireAdministrator` manifest**（`src-tauri/AutoTappo.exe.manifest` + `build.rs` 的 `WindowsAttributes::app_manifest`），双击即提权，**每次启动会弹一次 UAC 确认框**——这是强制提权的固有代价，无法绕过；
- 自定义 manifest 时**必须自行声明 Common-Controls v6 依赖**（Tauri 官方文档明确警告），否则 dialog 等 API 会失效；本项目已在 manifest 中补齐；
- NSIS 安装模式相应改为 `perMachine`（与管理员权限运行匹配）；`relaunch_as_admin` 命令保留作为兜底，正常情况下不会被触发。

**⑧ 遮挡与不可用状态**

- 桌宠视觉层是 `TOPMOST`，普通窗口不会遮挡它；**只有 z 序中排在桌宠之前的窗口**才算遮挡；
- 必须排除两类假阳性：**桌宠自己进程的窗口**（输入层/Toast/面板）、**`WS_EX_TRANSPARENT` 点击穿透全屏覆盖层**（远控、手写画布等）；
- **模块面板打开时 `GetWindowRgn` 返回 ERROR（窗口无区域）** → 定位为空 → 自动跳过。

**⑨ 跨屏后的短暂不一致（靠“点后验证”兜底）**

- `F8` 跨屏（尤其两屏 DPI 不同）后可能**区域与绘制短暂不一致**，像素判定会变“未知”；
- 应对：**每轮重新读区域（绝不缓存）** + **点击后 350 ms 复查**（区域哈希变化且不再就绪才算成功），失败降频重试。

**⑩ 资源占用（常驻场景实测）**

| 操作 | 单次耗时 |
| :-- | :-- |
| `GetWindowRgn` + `GetRegionData` | **21 µs** |
| 区域解析找锚点 | < 50 µs（Rust） |
| z 序遮挡检查 | < 30 µs（Rust） |
| 原生 `BitBlt` 抓 62×44 小块 | 1~3 ms（仅在决策时刻） |

- 稳态每 30 分钟仅唤醒约 40 次，CPU 占用 ~0.005%；**绝不可每轮截图**（PIL 全屏抓取实测 ~100 ms，会显著吃 CPU）。

**⑪ 不透明度与桌宠大小（50%~200% / 0%~100%）**

从官方前端逆出的实现（`wuwa_assets/index-BjTCKBy6.css` + `index-D8GJxFG7.js`）：

```css
:root { --pet-base-size: 200px; --pet-scale: 1 }
/* 桌宠容器 */
opacity: calc(var(--window-opacity, 1) * var(--hover-auto-hide-opacity));
transform: scale(var(--pet-scale, 1));
transform-origin: 124px 331px;
```

```js
applyWindowOpacity(e): --window-opacity = clamp(0,100,e) / 100   // 不透明度 0~100%，默认 100
applyPetScale(e):      --pet-scale      = clampPetScale(e) / 100 // 大小 50~200%，默认 100
PET_SCALE_MIN = 50, PET_SCALE_MAX = 200
```

- **大小是纯几何变化**：命中区域会随 `transform: scale()` 一起缩放，而我们的定位是「测量式」的
  （`scale = 实测平台高度 / 44`），因此天然自适应；
- **但原 `scale` 合法区间 `0.5..=3.5` 不够**：`scale ≈ (DPI/96) × 大小比例`，200% 大小 + 200% DPI = 4.0
  会被误判非法而整簇跳过，已放宽为 `0.35..=4.6`；
- **不透明度会破坏绝对像素阈值**：屏幕像素是 alpha 合成 `screen = a·C + (1−a)·B`
  （`a`=不透明度，`B`=按钮背后的壁纸）。设 a=0.5、深色壁纸 `(30,30,35)`、就绪橙 `(255,138,61)`，
  合成得 `(143,84,48)` —— `(r−b)=95`、`(r−g)=59` 都还成立，但 `r > 180` 这条**绝对亮度门槛不成立**，
  于是判为 `Unknown` → **永不点击**。这就是「调了透明度就开不了箱」的根因，且失效与否取决于壁纸明暗，属偶发；
- **解决办法是「背景差分 + 色度方向」**：锚点框在桌宠本体**右外侧**（CSS `right: -36px / -46px`），
  框外一圈就是纯壁纸，可直接采到 `B`；框内相减得 `d = screen − B = a·(C − B)`，
  **`d` 的方向与 `a`、`B` 都无关**，只被 `a` 缩放长度，因此按方向判色相即可跨不透明度工作；
- **两级结构**：阶段 1 仍先跑原绝对阈值（100% 不透明度下行为与旧版完全一致，不引入回归），
  只有判不出时才走阶段 2 背景差分；所有判不了的路径一律收敛到 `Unknown`，而 `Unknown` **从不点击**（失败安全）；
- 采样时向右/上/下各外扩 5~20 px 取壁纸样本（**左侧不扩**，左侧紧邻本体，会把本体像素混入背景估计）。

> **实机进展（2026-10-27）**：在多种「不透明度 × 桌宠大小」组合下实测，开关反复关闭再打开，
> 均能正确显示「冷却中」，不再退化为「无法判定」——说明阶段 2 的背景差分判据已真实生效。
> **仍待确认**：尚未观察到「就绪」态（warm 路径）在低不透明度下的表现，需等宝箱真正就绪时再验一次；
> 阶段 2 的比例阈值（`0.42 / 0.22 / 0.28 / 0.18`）仍属估算值，未用诊断日志取数校准；
> `hoverAutoHide` 打开时鼠标悬停会让桌宠淡出，现有「60 ms 区域稳定性校验」只校验区域、不校验像素，
> 后续可考虑补一次像素一致性校验。详见 `research/auto-chest-opacity-scale.md`（该目录不进版本库）。
>
> **校准通道**：本程序是 GUI 子系统（无控制台），诊断日志**落盘到
> `%TEMP%\autotappo-chest-debug.log`**。设置环境变量 `AUTOTAPPO_CHEST_DEBUG=1` 后启动，日志里会出现
> `[chest] stage1 warm=… white=… box=…x…`、`[chest] stage2 bg=(r,g,b) warm=… white=…` 与
> `[chest] 判定 = …`（每轮一行，可看出循环是否在跑）。超过 1 MB 自动清空，不会写满磁盘；
> 不设该变量时仅一次 `AtomicU8` 判断，开销可忽略。

**⑫ 多猫重叠导致区域合并 —— UI Automation 才是正解（2026-10-27 实测）**

- 用户把自己桌宠拖到别人桌宠上之后，`GetWindowRgn` 返回的区域会**合并成一整块**：
  实测 `rects=1`、`bound=(1308,821)-(1704,1032)`（396×211，而单体本应只有约 200 宽）；
- 锚点的凸起信息被彻底吞掉 → 逐行剖面反解出 `scale = 211/44 ≈ 4.8` 越界 → 一个锚点都定位不到
  → 面板显示「暂无宝箱」。**与不透明度、桌宠大小完全无关**；把桌宠拖开立刻恢复（已实机确认）；
- **根本解法：走 UI Automation 直接读官方 WebView 的 DOM**（`src-tauri/src/uia.rs`）：
  - `Get_CurrentClassName` 直接给出状态：`reward-countdown`＝冷却，`reward-btn`＝就绪；
  - 冷却剩余时间就是元素 `Name`（来自官方 `<div role="img" aria-label="13:34">`）；
  - `Get_CurrentBoundingRectangle` 给出精确屏幕矩形，点击坐标 = 矩形中心 − 窗口原点；
  - `pet-dock-content` 是本体的 DOM 容器（实测 200×211），**只属于自己那只猫**，
    用它反推缩放 `s = h/211`，再按 CSS 偏移预测左右锚点中心取最近者判左右；
  - 实测每次遍历 **25~37 ms**；桌宠内存无变化（DOM 只有十几个节点）；
- **前提**：UIA 受 UIPI 限制，要求与官方桌宠**同等完整性**（本项目已强制 `requireAdministrator`）；
- **降级策略**：UIA 初始化/调用失败时自动回落到下面的区域 + 像素老路径，失败安全不变。

---

## 3. 防干扰与输入拟真技术规范

### 3.1 幻影按键 (Phantom Keys)
为了确保发送的模拟击键**绝对不在任何编辑器、终端或聊天框打出乱码字符**，且不破坏现有软件快捷键：
- 采用 Windows 虚拟功能扩展键：**`VK_F21` ~ `VK_F24`**（`0x84 ~ 0x87`）；
- 底层工作线程随机交替发射，满足桌宠交替按键去重计数需求。

### 3.2 全场景礼貌避让 (Polite Pausing)
利用 Windows `GetAsyncKeyState` 在发包前毫秒级侦测用户物理操作：
1. **输入法中英文切换保护**：检测到物理 `Shift`、`Ctrl`、`Alt`、`Win` 时立即暂停，松开后附带 180ms 礼貌冷却，确保微软拼音等输入法的 Shift 切换绝不失效；
2. **日常打字避让**：检测到物理 A-Z、0-9、空格、退格、回车时立刻避让；
3. **鼠标划词与拖拽避让**：检测到物理鼠标左键、右键、中键（`0x01, 0x02, 0x04`）处于按下状态时绝不发送按键，不打断用户选中文本。

### 3.3 鼠标指针防消失防护
- 启动时通过 `SystemParametersInfoW(SPI_SETMOUSEVANISH, ...)` 暂时关闭 Windows “键入时隐藏鼠标指针”，程序完全退出时自动恢复；
- 不执行任何改变光标坐标的位移指令，保证 100% 物理手感。

### 3.4 图标多分辨率标准与 WebView2 隔离规范
1. **Windows Multi-frame ICO 规范**：
   - Windows 资源管理器及任务栏要求 `.ico` 必须包含 16x16, 24x24, 32x32, 48x48, 64x64, 128x128, 256x256 全套标准帧，否则易被系统缓存回退或模糊拉伸；
   - `build.rs` 中显式配置 `cargo:rerun-if-changed`，确保 CI 资源更新时 Cargo 绝不沿用旧编译中间件。
2. **WebView2 前端 IIFE 严格隔离规范**：
   - 前端 `web/app.js` 必须使用自执行函数 `(function() { 'use strict'; ... })();` 进行完全闭包隔离；
   - 避免在 WebView2 内部执行上下文切换或热重载时触发 `SyntaxError: Identifier ... has already been declared` 导致整段 JS 解析中断。

---

## 4. 软件架构与工程结构

```
autotappo/
├── .github/workflows/
│   └── build.yml          # GitHub Actions 自动化 CI 流水线 (推送/打Tag自动编译产出便携版与NSIS安装包)
├── src-tauri/
│   ├── Cargo.toml         # 原生轻量依赖 (tauri v2, serde, rand)
│   ├── tauri.conf.json    # 配置 (ID: com.autotappo.desktop, bundle/nsis perMachine, 无窗口尺寸——尺寸在 window.rs)
│   ├── AutoTappo.exe.manifest  # 嵌入 exe 的清单：requireAdministrator + PerMonitorV2 + Common-Controls v6
│   ├── build.rs           # 通过 WindowsAttributes::app_manifest 嵌入上述清单
│   ├── capabilities/      # Tauri v2 权限配置
│   └── src/
│       ├── main.rs        # 程序入口、DPI感知初始化、模块装配、窗口拦截、ExitRequested保护
│       ├── tray.rs        # 托盘图标、左键单击呼出、托盘右键上下文菜单
│       ├── window.rs      # 按需创建与销毁主窗口 (452x620，彻底释放 WebView2 渲染内存)
│       ├── clicker.rs     # 后台静默连点工作线程 (F21~F24发射、礼貌避让、动态热键监听)
│       ├── chest.rs       # 自动开箱引擎 (窗口发现/区域读取/锚点定位/遮挡检测/像素判定/点击注入)
│       ├── uia.rs         # UIA 无障碍树探测（读官方 WebView DOM：状态/矩形/剩余时间）
│       ├── updater.rs     # 一键自更新（WinHTTP 下载 → 备份 → 替换 → 重启 → 清理 .bak）
│       └── commands.rs    # IPC 接口 (get_status, toggle_running, set_speed, set_hotkey, reset_count, minimize, quit, get_chest_status, set_auto_chest, relaunch_as_admin, apply_update)
├── web/
│   ├── index.html         # 精致看板 UI (状态卡片、三档速度、自定义热键胶囊、自动开箱控制卡、更新弹窗)
│   ├── style.css          # 现代化玻璃拟态 UI (移除刺眼彩色Emoji，全单色几何矢量SVG，细腻微光动画)
│   ├── app.js             # 前端控制逻辑 (纯浏览器独立双击预览模式、Gitee/GitHub双通道检测、热键录制、开箱状态轮询)
│   └── logo.png           # 高清矢量 Logo
├── README.md              # 纯用户端说明文档 (CampusFlow 风格)
└── AGENTS.md              # 开发者与 Agent 架构知识库 (本文件)
```

---

## 5. 核心功能已实现清单

1. **Rust 原生挂机引擎**：
   - 三档速度拟真调节：休闲（~10次/秒）、快速（~25次/秒）、极速（~50次/秒）；
   - 随机击键时长（10~30ms）与微停顿（3%几率模拟人手停顿 250~400ms）。
2. **动态全局热键录制**：
   - 默认快捷键 `F6`；
   - 界面点击热键徽章进入录制状态，敲击任意功能键（如 F1~F12、Home、PgUp、小键盘等）按完即生效，支持持久化存储到 `localStorage` 并同步至 Rust 原生线程；
   - 严禁并拦截绑定官方保留的 `F7`、`F8`、`F9`。
3. **CampusFlow 级托盘与窗口生命周期**：
   - 点击主界面右上角关闭或“收进系统托盘”，窗口直接 `destroy()`，彻底释放 200MB~300MB 内存；
   - 单击托盘图标毫秒级重建窗口并恢复当前挂机看板状态。
4. **国内高速双通道更新检测系统**：
   - 优先通过 Gitee 国内 API 查询最新 Release；
   - 若 Gitee 404 或网络波动，无缝降级到 GitHub API 兜底；
   - **界面唤醒自动检测**：每次从托盘点出界面时后台静默查验（内置 60 秒防抖），发现新版时右上角版本胶囊自动出现呼吸发光红点；
   - 弹窗内明确展示 **GitHub 仓库下载地址与一键跳转按钮**。
5. **自动开箱引擎（`chest.rs` + `uia.rs`）**：
   - 独立原生线程，默认关闭，由界面开关（`set_auto_chest`）控制；
   - **主线走 UI Automation**：直接读官方 WebView 的 DOM，拿到 `reward-countdown`/`reward-btn`
     状态、精确矩形与冷却剩余时间——因此**多猫重叠、低不透明度、任意缩放**都不影响；
   - UIA 不可用时自动回落：每轮重读 `GetWindowRgn`、x 重叠聚类、逐行剖面定位锚点、CSS 比例自校验；
   - z 序遮挡检测（排除自身进程窗口与 `WS_EX_TRANSPARENT` 覆盖层）；
   - 回落路径用原生 `BitBlt` 抓取锚点框内缩 12% 采样，`warm/white` 双阈值判定就绪（未知一律不点）；
   - 点击后 350~400 ms 效果验证，失败降频重试；提权不足时状态置 `need-admin` 并 60 s 退避；
   - 稳态 30 分钟仅约 40 次唤醒，CPU ~0.005%。
6. **强制管理员权限（v0.2.0 起）**：
   - exe 内嵌 `requireAdministrator` manifest，双击即提权，避免用户手动右键或二次重启；
   - 开关状态由前端 `localStorage` 唯一决定，后端快照只用于展示，**绝不反向覆盖用户选择**（否则刚打开就会被旧快照关掉，且轮询停摆后再也打不开）。
7. **前端无编译本地即开即看**：
   - 直接双击 `web/index.html` 即可在普通浏览器中体验完整 UI 与交互动效；
   - 控制台内置 `testNewVersionUI()` 快速测试钩子。
8. **双 Git 远端协同**：
   - GitHub: `origin` (`https://github.com/To0Ru/AutoTappo.git`)
   - Gitee: `gitee` (`git@gitee.com:To0Ru/AutoTappo.git`)
9. **一键自更新（v0.2.1 起）**：升级弹窗里点一下就自动下载、备份旧 exe、原地替换并重启，
   详见 6.5 节。

---

## 6. 自动开箱已实现设计（`src-tauri/src/chest.rs`）

> 技术依据全部见 **2.4 节**。本节只描述代码结构与状态机，避免与 2.4 重复。

### 6.1 模块职责划分

| 函数 | 职责 |
| :-- | :-- |
| `ensure_dpi_aware()` | 启动早期设置 `PER_MONITOR_AWARE_V2`，保证坐标是物理像素 |
| `find_pet()` | `EnumWindows` 找 `DesktopCatInput`（输入层）+ `Tauri Window/Wuwa Tappo`（视觉层），并取窗口原点与 DPI |
| `read_region(hwnd)` | `GetWindowRgn` + `GetRegionData` 解析出矩形列表（窗口相对） |
| `cluster_by_x()` | 按 x 区间重叠的并查集聚类（多猫场景） |
| `anchors_in_cluster()` | 逐行右边界剖面 → 平台 → 同一批行的本体右缘（众数）→ CSS 比例自校验 |
| `z_order()` / `is_occluded()` | 只检查 z 序中位于桌宠之前的窗口，排除自身进程与 `WS_EX_TRANSPARENT` |
| `capture()` | 原生 `BitBlt` + `GetDIBits` 抓一小块，返回 BGRA 缓冲 |
| `ring_margin()` | 按 scale 决定采样外扩边距（5~20 px），用于取框外壁纸样本 |
| `background_estimate()` | 框外一圈像素逐通道取中位数，估出壁纸底色 `B` |
| `classify()` | 两级判定：绝对阈值（100% 不透明度）+ 背景差分（低不透明度） |
| `post_click()` | `WM_MOUSEMOVE → WM_LBUTTONDOWN → WM_LBUTTONUP`，客户区坐标 |
| `uia::Uia::probe()` | UIA 读官方 WebView DOM（仅当 UIA 可用时才走这条主线） |
| `resolve_chests()` | 把 UIA 报告翻译成「左右宝箱 + 客户区点击坐标」，并换算屏幕→客户区 |
| `predict_place()` | 用 `pet-dock-content` 反推缩放，按 CSS 偏移判定该元素是左还是右 |
| `is_elevated()` / `relaunch_as_admin()` | 检测自身提权状态 / `ShellExecuteW("runas")` 重启 |

### 6.2 状态机与时间预算

| 状态 | 触发条件 | 下一轮间隔 |
| :-- | :-- | :-- |
| `idle` | 开关关闭 | 500 ms |
| `no-pet` / 区域为空 | 找不到桌宠 / 面板打开 / F9 隐藏 | 5 s |
| `moving` | 60 ms 内区域哈希变化（用户拖拽） | 800 ms |
| `no-chest` | 定位不到锚点（累计数不足） | 10 s |
| `no-chest`（UIA 路径） | DOM 里一个宝箱元素都没有（累计 < 1000，或面板打开） | 15 s |
| `cooldown` | 像素判为冷却 | **30 s** |
| `cooldown`（UIA 路径） | DOM 读到 `reward-countdown` | 睡到「剩余 − 8 s」，上限 20 min（30 min 冷却约只需 2 次唤醒） |
| 就绪 | 像素判为就绪 | 立即点击 → 点后验证 |
| `opened` | 点后验证成功 | **29 min 45 s**（然后 2 s 细扫） |
| `opened`（UIA 路径） | 点后验证成功 | **3 s**（开掉的那个会变成 30 min 倒计时，下一轮自然按剩余时间睡；双宝箱同时就绪时另一个能立刻点掉） |
| `retry` | 点了但验证失败 | 5 s |
| `occluded` | z 序被遮挡 | 2→5→10→30 s 退避 |
| `need-admin` | `PostMessage` 返回 ERROR_ACCESS_DENIED | 60 s |

### 6.3 资源红线（不得违反）

1. **每轮必须重读区域**（21 µs），绝不缓存——缓存会踩跨屏不一致的坑；
2. **绝不可每轮截图**：只在判定要点击那一刻抓一小块（100% 时约 62×44，低不透明度适配后外扩到约 69×58），1~3 ms；
3. 循环内**无持续增长的分配**：每轮仅一次约 4 KB 的区域缓冲临时分配；捕获缓冲只在小块截图时申请；
   阶段 2（背景差分）会临时申请 3 个 `Vec<u8>`（约 1~3 KB）做中位数，用后即释放，不累积；
4. 线程全程 `sleep` 驱动，禁止忙等或自旋。

### 6.4 安全红线（沿用 2.3 节）

- 严禁伪造 `api/frontend/chests/open` 等 HTTP/jsonp 请求；
- 只允许驱动官方 WebView 通过原生消息流执行真实点击；
- 绝不移动真实鼠标指针（不使用 `SendInput` / `mouse_event` / `SetCursorPos`）。

---

### 6.5 一键自更新（`src-tauri/src/updater.rs`）

> 目标：用户不必再手动下载、删旧程序、替换文件，点一下就完成升级。

流程（严格按此顺序，任何一步失败都回滚）：

1. 前端从已拿到的 Release 信息里挑出便携版资产地址（`AutoTappo.exe`，排除 `setup` / `install`），
   取不到就按约定拼 `https://<host>/To0Ru/AutoTappo/releases/download/<tag>/AutoTappo.exe`；
2. 调 `apply_update` 命令 → 在阻塞线程里执行 `updater::apply_from_url()`（不卡 IPC 线程）；
3. 下载到当前 exe 同目录的 `AutoTappo.exe.new`。**下载走 WinHTTP**（系统自带）：
   零新增 crate、自动复用系统代理与系统证书库，也不会像子进程那样闪控制台；
4. 校验：体积 ≥ 300 KB、文件头必须是 PE 魔数 `MZ`（防止把 HTML 错误页当成程序）；
5. `AutoTappo.exe` → `AutoTappo.exe.bak`（**运行中的 exe 允许改名，不允许删除**）；
6. `AutoTappo.exe.new` → `AutoTappo.exe`，失败则把 `.bak` 改回去；
7. `spawn` 新 exe 后旧进程 `app.exit(0)`；启动失败同样回滚到旧版本；
8. 新进程启动时 `cleanup_after_update()` 删除 `.bak`（老进程可能还没完全退出，
   故放到**后台线程重试最多 10 秒**），并清掉残留的 `.new`。

安全约束：
- 下载地址走**主机白名单**（`github.com` / `*.githubusercontent.com` / `gitee.com` / `cdn.jsdelivr.net`），
  防止前端参数被篡改成任意 URL 下载；只接受 `https://`；
- 诊断日志落盘 `%TEMP%\autotappo-update.log`（GUI 子系统无控制台）。

已知限制：便携版场景才有效；NSIS 安装版用户仍建议用安装包覆盖安装。

## 7. 开发者与 Agent 工作准则

- **Git 提交与推送的边界**：
  - **本地 `git commit` 无需征求同意**，应经常提交，作为改错回滚的安全网（提交信息写清改了什么、为什么）；
  - **`git push` / 打 tag / 发布 Release 必须先征得用户明确同意**，一步一确认，不得自行把改动推到远端；
- **严禁提交垃圾文件**：严禁提交临时 Python、AHK、.bat 脚本或编译产物；
- **保持 README.md 纯净**：`README.md` 仅包含用户关心的安装、使用和特色，技术与逆向细节统一沉淀在 `AGENTS.md`；
- **多平台同步**：完成功能后，使用 `git push origin main && git push gitee main` 保持两端同步；
- **构建产物主动拉取到本地**：每次 GitHub Actions 编译完成、或发布新 Release 后，**主动下载一份便携版 `AutoTappo.exe` 到 `research/`**（文件名随意，建议带版本号如 `AutoTappo_v0.2.0.exe`），方便用户直接双击测试，省去用户自己去 GitHub 手动下载。`research/` 已被 `.gitignore` 排除，不会误提交；
- **禁止的进程操作**：**严禁** `Get-Process msedge | Stop-Process` 这类按进程名无差别终止，会杀掉用户正在使用的浏览器。需要无头截图时用独立 `--user-data-dir` 启动并用 `-PassThru` 只结束自己启动的 PID；
- **打 Tag 发布正式版**：
  ```bash
  git tag v0.1.0
  git push origin v0.1.0
  ```
  GitHub Actions 会全自动打包 Release 并挂载 NSIS 安装包与 Portable 便携版。
