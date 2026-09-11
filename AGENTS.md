# DeskFence 工作约定（给后续会话的 Agent）

## 沟通与修改纪律（2026-08-31 用户明确要求，长期有效）

1. **注释一致性**：任何代码改动，必须同步核对受影响的注释与代码是否一致，
   不符合就改注释（行为变了、旧注释没跟着变 = 必须修）。首次执行就抓出
   band_attach_anchor 降级循环未排除宿主的真 bug，此纪律有效。
2. **疑虑/冲突主动确认**：**任何一方**——用户新指令、准备执行的操作、遇到的
   现有状况——与 AGENTS.md 既有约定、历史决策或日志事实有冲突、矛盾或含糊
   时，**主动向用户确认清楚再执行**，不要自行猜测静默取舍。实例：AGENTS.md
   写"docs/ 不发布"，但 README 引用 docs/demo.svg（实际 main 上有它）——
   这类矛盾应先问，不要自己推断；组装命令列表漏了 icon.svg 导致 README
   图标 404，同理，发现"现状与文档不符"先停下来问。
3. **操作可回滚可还原（2026-09-01 用户明确要求，长期有效）**：所有会话的
   **每次操作**都必须做到可回滚、可还原——
   - 改文件/改代码：在 git 跟踪内进行，动手前确认现状可从 git（或副本）
     还原；工作树已有未提交改动时不得混入无关修改，避免无法分离回退。
   - 删除/覆盖/移动：先留备份（改名保留或复制），不直接销毁原内容。
   - 改系统/配置状态（注册表、settings.json、自启动项、桌面图标可见性等）：
     先记录原值，或确认存在程序内恢复路径（如 `--restore-desktop`）。
   - 不可逆动作（force-push、重写历史、真删数据、对外发布/推送）：执行前
     **必须先向用户确认**，且留有还原手段。
   - 执行任何有风险的操作时要能说清"如何撤销它"；撤销路径不明的操作先问
     再做（与第 2 条同一纪律）。
   - **git 小步提交是硬纪律（2026-09-01 补充，适用于任何项目任何会话）**：
     动手改代码前工作树必须干净（已有改动先 commit 或 stash）；每完成一个
     可独立验证的改动（用户确认后）立即 `git commit` 形成还原点，绝不把多
     个功能项堆在一个未提交工作区里——回滚一律用 `git revert`/`reset`/
     `checkout` 快速完成。本地私有仓直接提交即可（不推远端）。

## 沟通与修改纪律（续）

4. **先找现成能力/最短路径（2026-09-04 用户要求，长期有效）**：解决问题或
   改代码前，先想"有没有现成可用能力、更快更直接的办法"，不要在受限路径里
   绕圈耗时耗 token。实例：需要原生桌面/原生控件的真实参数时，直接请用户
   切换到原生桌面（或用应用的 desktop_state 切换）现场取数，而不是在栅栏
   模式下反复间接探测；应用已有的 desktop_state 三态、CLI 旗标、既有诊断
   工具优先复用。
5. **能并行就并行（2026-09-04 用户要求，长期有效）**：凡是相互独立、可同时
   进行的子任务，主动并行，不要串行磨蹭——同一响应里并行发起多个独立工具
   调用；多步的独立任务（多路搜索/排查/验证/读多处文件）用 Agent 工具并行
   派发子智能体（Explore/general-purpose 等），一次消息里同时发出。用户观察
   到国外模型默认多智能体并行、GLM 系列偏串行，特此明确要求改。注意边界：
   有依赖关系的步骤仍按顺序；会互相踩踏的操作（改同一文件、有先后次序的
   构建/验证链）不强行并行；给子智能体的任务描述必须自包含（它看不到本会话
   上下文），只取结论不取过程。
6. **代码卫生即做即清（2026-09-09 用户明确要求，长期有效）**：每次修改或
   操作结束后，代码必须停在干净状态，不把尾巴留给下个会话：
   - **调试/手工测试的产物用完即删**：临时探针代码、`#[ignore]` 的
     `debug_*` 手工测试、临时日志输出、桌面/目录里的探针临时文件——验证
     一完成当场删除，绝不入库。随 `cargo test` 常跑的功能性单元测试保留。
   - **零警告水位**：动过代码后 `cargo build --release` 与
     `cargo clippy --all-targets` 必须零警告（CI 已是 `-D warnings` 硬
     门禁，2026-09-09 起），测试全绿再提交。
   - **注释与代码同步**：行为改了注释必须跟着改；搬迁/删除代码时把孤儿
     注释一并处理（归位到真身或删除），不留"描述不存在的东西"的注释。
   - 无死代码、无注释掉的代码块、无 TODO/FIXME 残留；发现即顺手清，
     随当笔提交走，不攒批次。

## 构建/运行流程（用户明确授权）

- **有运行中的 deskfence.exe 实例时：直接 `taskkill //F //IM deskfence.exe` 停掉，
  重新 `cargo build --release` 并启动新版本**，不需要先问用户。
- 启动方式：`cmd //c start "" "<绝对路径>\target\release\deskfence.exe"`（Git Bash 中
  路径要加引号，反斜杠会被吞）。
- 验证存活：`tasklist //FI "IMAGENAME eq deskfence.exe"` + 看
  `%APPDATA%\DeskFence\run.log` 尾部是否出现
  `desktop icons hidden after fence presentation verified`。
- 出问题时自救命令：`deskfence.exe --restore-desktop`（恢复原生桌面图标后退出，
  适合验证 exe 是否能启动——注意 PowerShell 对 GUI 子系统程序不等待，要拿退出码用
  `Start-Process -Wait -PassThru`）。

## 历史教训（不要再踩）

1. **不要静态导入 comctl32 v6 独有导出**（如 `DrawShadowText`）：exe 未嵌入 SxS
   清单时加载器绑到 comctl32 v5.82，导出缺失 → 进程以 0xC0000139
   (STATUS_ENTRYPOINT_NOT_FOUND) 在 main 之前死掉，无任何日志。当前做法：
   `app.manifest`（已嵌入 DeskFence.rc）+ render.rs 里 GetProcAddress 动态加载、
   失败降级两遍 DrawTextW。新增 shell/comctl 导出时先想一遍这个问题。
2. 沙箱 shell 里跑 GUI exe 的退出码不可信（可能报 127/0）；判定启动失败要用
   `Start-Process -Wait -PassThru` 的 `$p.ExitCode`。
3. 本仓库曾有"另一个源码树构建的实例"在本机运行（日志里有 `wallpaper: parsed ok`
   字样即为其产物，本仓库源码无此字符串），排查问题时先确认进程来源。
4. **像素对齐已验证基准（2026-08-24，勿回退）**：
   - 图标路径 `shell.rs get_system_icon_hicon` 主路径（SHGFI_ICON|LARGEICON|
     ADDOVERLAYS，SM_CXICON==目标时）与原生桌面**逐像素一致**（含 .lnk 箭头，
     全屏模板搜索残差≈1.6-2.4/255）。Win11 24H2+ 下 `SHGFI_OVERLAYINDEX` 对
     .lnk 返回 0，imagelist overlay 路径拿不到箭头——别把兜底路径提为主路径。
   - 图标名矩形顶 = **图标底 + 2 逻辑 px**（render.rs draw_item `label_top`，
     150% 下实测 3 物理px）。此前 `y+cs+6.5*scale` 把图标当 y+0 起算导致 gap
     偏小 3px，已修。验证方法：两态截图（栅栏态/`--restore-desktop` 原生态）+
     48px 模板自由搜索定位 + "图标底+gap+文字"条带 NCC，同文件配对 shift 应为 0。
   - **拖拽残影/入场动画的图标名也必须走同一几何+GDI 路径**（draw_guides 返回
     GdiLabelJob → `gdi_draw_labels_transparent`：DrawShadowText + 仅对"有墨水"
     像素置 alpha=255）。曾用 D2D 文字+2 物理px 偏移，拖动时与原生/静态差 3px+
     抗锯齿不同，用户立刻看出不对齐。残影图标本体 0.65 alpha（≈原生拖拽图像）。
   - 残影/入场的**换行状态与静态一致**（原生拖拽图像保持拖前换行）：ui.rs
     refresh_guide 用与 draw_item 相同的 trim_to_lines（同宽 cell_w-2*scale、
     同 2 行上限）先截断再交给 GDI，构造上保证两行拖动中依旧两行。自动化
     鼠标测试前必须 MinimizeAll，否则点击被用户窗口吃掉、拖动根本不触发。
   - 诊断工具：`deskfence.exe --icondump <file> <out-prefix>`（导出 SHGFI /
     imagelist 两条路径的 48px 图标 BMP+BGRA）。注意 dump 进程需先设 DPI 感知
     （已在 icon_dump 内处理），否则 SM_CXICON 被虚拟化成 32。
5. **Git Bash + PowerShell 5.1 联用坑**：
   - bash 双引号里 `\\$var` 会变成字面 `$var`；传 Windows 路径用单引号
     `'C:\path\file'` 拼接变量。SHGetFileInfoW 不接受正斜杠路径。
   - 给 `powershell -File` 用的 .ps1 / Add-Type 的 .cs **必须 UTF-8 带 BOM**
     （无 BOM 按 GBK 读，中文注释会吞换行导致代码错乱）；**纯 ASCII 的脚本可不带
     BOM**。Add-Type 编译器是 C# 5，别用新语法。复杂引号一律写成 .ps1 文件再
     `-File` 调用，别在 `-Command` 里嵌 C#（bash/PS 双层引号必炸）。
   - PowerShell 截图进程默认 DPI 未感知（截出来是 1280x853 虚拟分辨率）；
     必须 P/Invoke `SetProcessDpiAwarenessContext(-4)` 后再 CopyFromScreen
     （tools/uitest.ps1 已封装 shot/click/menuwin/dialog/keys 等动作）。
   - 截图前先 `(New-Object -ComObject Shell.Application).MinimizeAll()`，
     否则截到的可能是浏览器全屏窗而不是桌面。
   - 弹出菜单存在与否用窗口类 `#32768` 枚举判定（uitest.ps1 menuwin），
     比截图 OCR 可靠；注意本机 SPES 安全软件常驻一个离屏 #32770 对话框，
     枚举对话框必须带 pid 区分归属。
6. **菜单前台权与闪屏(2026-08-25 修复，勿回退）**：所有 TrackPopupMenu 调用前必须
   `SetForegroundWindow(owner)` + 菜单后 `PostMessage(WM_NULL)`（shell.rs
   `menu_foreground`，KB135788/MSDN 托盘标准做法）。owner 用 1px 菜单宿主窗口
   （ui.rs init_tray 创建，WS_EX_TOOLWINDOW|WS_EX_TRANSPARENT，屏幕右下角最后
   一像素）。四个坑，全都实测踩过：
   - **隐藏窗口无法前台化**（SetForegroundWindow 静默失败→僵尸菜单/秒退）。
   - **owner 绝不能 WS_EX_LAYERED**：分层 owner 损坏菜单开合动画成两段跳变；
     也**绝不能压在栅栏区域上**（1px 前台窗口失活会波及正下方分层窗口）。
   - 权限被拒时 `AttachThreadInput` 借前台线程输入状态重试（menu_foreground）。
   - **"点桌面关菜单闪屏"是四重根因叠加**，全修于 2026-08-25：
     ① 栅栏悬停卡片(fence_hover)立即点亮→菜单期间残留→关闭后被迟到的
     MOUSELEAVE 熄灭=亮灭闪。修：卡片改 400ms 延迟提交（fence_hover_pending，
     与图标 hover 一致），track() 打开菜单时清全部 hover 状态。
     ② 菜单模态循环会推迟栅栏消息，关闭后补投递的**陈旧坐标 mousemove**
     会幽灵点亮悬停。修：WM_MOUSEMOVE 与 GetCursorPos 真实位置偏差>6px 丢弃。
     ③ ensure_all_attached 每秒对每个栅栏无条件 SetWindowPos"洗牌"（多窗口
     同目标导致就位判定永假），平时是 no-op，前台 band 变化后变成真实 z
     移动=分层窗口跨 band 重合成闪。修：整链就位判定（宿主上方窗口之下
     恰好是全部栅栏则跳过；1px 菜单宿主在容忍集内）。
     **③ 的判定本身 2026-08-27 又挖出致命 bug（"刚开始不闪、菜单操作后
     每秒闪"的最终根因）**：旧实现从"宿主正上方窗口"向 GW_HWNDNEXT（向
     下）遍历，永远撞不到锚点自身；且托盘窗（DeskFenceTray,22x22）会被
     菜单前台化挤进栅栏与宿主之间——只容忍菜单宿主+预算 1 的判定每秒
     失配 → 每秒全栅栏 SetWindowPos=每秒闪。已重写：**从宿主向上
     （GW_HWNDPREV）走**，容忍自有辅助窗口（菜单宿主/托盘窗/#32768 菜单
     弹层），收齐全部栅栏才算就位；外来窗口夹在宿主与栅栏之间才修复
     （一次即收敛）。诊断：run.log 的 "z-chain repair" 正常应极罕见，
     每秒出现即此 bug 复发。
     **④ 2026-08-27 终修（闪屏 + 栅栏浮到别的窗口上方，两个症状同一根源）**：
     真点击实测（SPES 当日已放开合成点击）。**浮窗根因**：栅栏曾被顶到宿主
     之上 200+ 层（入口：desktop_insert_after 在取不到锚点时回退 HWND_TOP、
     以及创建时无锚点的 HWND_TOP 回退），而"相交才遮挡"的判定对"下方窗口
     不与栅栏矩形重叠"的高位栅栏直接放行——判定测的是"有没有被盖"，不是
     "在不在桌面 band"。**闪屏根因**：单趟收集式判定遇任何不可见窗沉底
     （最小化窗/SPES 离屏窗/自动隐藏任务栏转换）就判全列失位→全量
     SetWindowPos=DWM 重合成。**终版不变式（勿回退）**：逐栅栏从宿主向上
     （GW_HWNDPREV）走，只允许跳过：其他自家栅栏、不可见窗（IsIconic/隐藏/
     矩形与虚拟屏幕不相交）、自有辅助窗（菜单宿主/托盘窗/#32768/Shell_
     TrayWnd）；**碰到任何可见且在屏内的外来窗口还没找到该栅栏，或预算
     （320 步）耗尽/到栈顶仍未命中 → 出带，把该栅栏单独移回**。锚点函数
     返回 Option，取不到就不动（绝不 HWND_TOP）。修复日志带 occluder 类名；
     常态应极罕见（boot 首次就位一次 + 真实出带事件）。测试要点：托盘菜
     单从图标**向上**展开，空白点击点必须先量菜单矩形再选（否则点到菜单
     第一项=误触发"隐藏全部栅栏"）。验收：tools/matrix2.ps1（tray/triangle
     × 1s/10s/30s 双路径矩阵，全部"菜单实测开→实测关+栅栏数不变+0 repairs
     +闪事件仅菜单自身像素"）+ tools/elevtest.ps1（人为把栅栏顶到栈顶，
     1s 内自愈拉回底带）+ tools/fenceloc.ps1（5 栅栏必须 aboveHost+1..+5）。
     ④ rescan 无条件 show_all_fences 全量重绘（watcher dirty 在菜单交互时
     被 Explorer 元数据触碰置位）；壁纸 60s 兜底捕获(PrintWindow 强制宿主
     重绘)撞上交互。修：rescan 文件集合无变化直接返回；壁纸比对带每通道
     8 容差（捕获亮度有 ±4% 时序波动，严格比较会误判"变了"引发全量重绘）；
     交互后 2.5s 内推迟捕获（mark_interaction/LAST_INTERACTION_MS）。
   - **残余现象（勿再追）**：菜单遮挡栅栏期间 DWM 丢弃被遮区域的颜色转换
     缓存，菜单移走后重转换有 ~4% 舍入差（检测器可见的 25k 采样事件，
     400ms 内自愈）——并排对比实测**低于人眼感知阈值**，ULW 重呈现也无法
     合并，属分层窗口固有行为。验证工具 tools/flashdet.ps1（~25fps 全屏
     差分探测器）+ tools/uitest.ps1（click/traymenu/menuwin/shot）。
   - probe_desktop_item_spacing（LVM_GETITEMSPACING 跨进程 SendMessage）
     **2026-08-26 已改粘性缓存**（键=注册表 IconSize+系统 DPI，键不变永不
     重发）：旧的 10s TTL 等于每 10 秒唤醒一次宿主，菜单交互后的未稳态里
     这次唤醒表现为"点桌面偶发闪 ~4% 亮度"——就是"刚开始不闪、后面点
     倒三角/托盘再点桌面有时闪"的根因。托盘 tooltip 为纯 "DeskFence"。
   - **⑤ 终修第二轮（2026-08-27 下午，勿回退）**：
     **浮窗最后入口**：handle_mousemove 拖栅栏时曾 `SetWindowPos(HWND_TOP)`
     把被拖栅栏顶到全栈顶且松手无人放回="看到过 1 次浮到 ZCode 上"的根源。
     已改：提升只到最高兄弟栅栏之上（ui.rs `drag_elevate_anchor`，band 内）、
     handle_lbuttonup 立即插回 `desktop_insert_after`、自愈豁免被拖者。
     **菜单后点桌面闪的真根源**：菜单开合瞬间系统瞬态窗插进宿主与栅栏 band
     （实抓三类：SPES epc_pxs 的 ScW 全屏钩子层；EdgeUiInputTopWndClass 输入
     条；cloak=2 的全屏 CoreWindow——SystemSettings/TextInputHost 等 visible
     位有效但 DWM 不合成像素，物理遮不住）→ 每次触发整链 z-chain repair=
     整面 DWM 重合成闪。此前判为"DWM 固有洗色不可修"是误诊。修三件套
     （ensure_all_attached）：① 容忍集扩容——DWMWA_CLOAKED 任意非零跳过
     （window_is_cloaked）+ EdgeUiInputTopWndClass 按 band 原生系统窗跳过；
     ② walk_strikes 防抖——连续 **3** 拍失位才修（tools/bandtest.ps1 实测
     150ms 存活/350ms 间隔的过路者在两拍制下会对齐两个 tick 造成漏网），
     真出带自愈延迟 2-3s（elevtest 验收窗已放宽至 4.5s）；重犯退避＝仅
     第 3、13、23…拍出手，杜绝周期性风暴复活；
     ③ 修复动作纯 z（SWP_NOMOVE|SWP_NOSIZE），位置归交互路径管，同位也
     触发的 DWM 重算降到最小。
     工具与基线：tools/bandtest.ps1（无输入注入：瞬态过路者零修复＋常驻
     外来者标记→单批修复→静默，当前 PASS）；tools/bandwalk.ps1（从宿主
     GW_HWNDPREV 向上走 = band 真值。**EnumWindows 枚举顺序对 TOOLWINDOW
     不可靠**，判断栅栏 z 别再用全局枚举序）；tools/zprobe.ps1（全局属性
     快照）。验收：fenceloc.ps1 五栅栏 aboveHost+1..+5；稳态 80s 日志零
     walk-break 零 repair。菜单开合仍存在的 DWM 颜色缓存洗色（前条"残余
     现象"）与原生同级、低于感知阈，不在修复范围。
7. **启动首帧/壁纸快照(2026-08-25 修复，勿回退）**：
   - `ensure_wallpaper` 的 `wallpaper_ms==0` 是"强制重捕获"哨兵（不能用饱和减法
     判断：进程启动前 15s 内 `now-0<15000` 会把清零操作整个吞掉，快照迟到一整个
     刷新周期——这就是"启动 1-3s 甚至 15s 后文字阴影才出现"的根因）。
   - **快照防污染**：PrintWindow 拍桌面宿主时原生图标可见就会把图标烙进快照
     （启动"乱七八糟"的根源）。捕获期间临时隐藏图标列表再恢复；壁纸快照持久化
     缓存（%APPDATA%\DeskFence\wallpaper.bin，退出时/内容变化时落盘，启动优先
     加载）让首帧免现场捕获。
   - **换壁纸过渡期 DWM 会给宿主刷纯黑**：黑帧是暂态内容，绝不能计入
     wallpaper_fails（会堆积触发"回退透明"误落盘）；真实失败（PrintWindow 报错）
     才计。回退判定需 `wallpapers.is_empty() && fails>=2 && 启动>10s` 双保险。
   - 壁纸变化信号源（按优先级）：WM_SETTINGCHANGE（手动换壁纸，毫秒级）→
     Themes 目录 watcher + IDesktopWallpaper 签名轮询（幻灯片轮换，秒级）→
     60s 兜底轮询（PrintWindow 强制宿主重绘，更短间隔会在交互时看到桌面闪；
     60s 经 60 秒静默连拍验证无可见闪）。**本机（企业定制环境）两个轮换信号
     都不可用**：换壁纸不写 TranscodedWallpaper（mtime 不变），IDesktopWallpaper
     coclass 未注册（REGDB_E_CLASSNOTREG）——只靠 SETTINGCHANGE+60s 兜底。
   - 启动关键路径已并行化：scan_desktop_raw（纯 FS ~5ms）先行，显示名解析
     （SHGFI_DISPLAYNAME，54 文件串行 ~1.3s）与图标提取（.lnk/exe 单个可达
     ~180ms）各 4 线程后台跑，主线程同时做壁纸暖场+渲染器预热（一次性小表面
     烧掉 D2D/GDI/DrawShadowText 首次使用的 300-900ms），join 后首帧全缓存
     命中。热会话 boot done ~0.8-1.5s，首帧即带 ClearType 阴影的最终帧。
   - MutexGuard 经 Deref 不能做字段级分裂借用，先 `let s: &mut UiState = &mut guard;`
     再同时借 renderer(不可变)与 icon_cache(可变)。
   - **CLSID_DesktopWallpaper = {C2CF3110-460E-4FC1-...}**——是 **3**110 不是
     5110，windows 0.52 未导出此常量需手写 GUID；写错 CoCreateInstance 报
     REGDB_E_CLASSNOTREG 静默降级。
   - **自启动首帧加速（2026-08-27，勿回退）**：冷启动慢的主因是"每次启动
     全量现提"——SHGFI 显示名解析 + 图标提取（.lnk/exe 冷盘+杀软单文件可达
     数百 ms）零持久化。已加 **%APPDATA%\DeskFence\iconcache.bin**：键=
     `{path}\0{px}`（与内存缓存一致）、逐条校验 mtime+字节长==4*px*px
     （render::icon_pixels 的 DIB 32bpp 契约），含 DFNM 显示名段；不合规
     条目跳过不中断。启动未命中项照旧后台提取，落盘由 global_tick 监视
     ICON_EXTRACT_COUNT 变化、安静 4s 后异步写（tmp+rename），启动关键
     路径零 IO——运行期懒提取/DPI 切换/新文件自动覆盖。warm_renderer_
     scratch 已并行化（show 前 join）。实测二次启动 bg_names 213→41ms、
     bg_icons 595→176ms（残余 miss≈回收站伪条目属预期，其键无真实文件
     mtime 不入缓存）、首栅栏呈现 ~303ms；冷盘真开机收益更大。注意：
     mtime 以 ms 截断存取同源一致；改名/换目标会更新 .lnk mtime 所以能
     正确失效；改 UI 排序逻辑时记得 finalize_scan_with 的预填值必须等于
     全量解析输出，否则去重/排序在热启与冷启不一致。自启动注册为 HKCU
     CurrentVersion\Run（shell.rs），与 Explorer 构建桌面同窗口启动，没有
     更早的合法时机可抢——进程拉起前的耗时属于系统/杀软范畴。
   - **栅栏原子呈现（2026-08-27，勿回退）**：refresh_fence_impl 原本"先
     ShowWindow 显示（空白）→ 绘制 → ULW 上屏"且串行循环，启动时用户看
     到栅栏从 1 号到 5 号依次点亮（首末相差整个串行绘制时长 ~80ms+）。
     已改批量模式：UiState.defer_show_until_batch 置位期间跳过显示动作，
     所有表面画完并向**隐藏窗**提交 ULW（对隐藏窗同样有效，像素暂存）后，
     show_all_fences 末尾一次 ShowWindow 放行（实测 0ms/5 个），DWM 同帧
     合成=同一帧弹出。已可见窗口的运行期刷新不受该位影响。
   - **菜单后点空白→原生闪现 1-2s（2026-08-27 傍晚修复，勿回退）**：三拍
     防抖的连带回归。ensure_all_attached 每 tick 先 `attached.clear()`，
     旧代码失位当拍就修好并重新入集合；防抖期内失位栅栏整 tick 缺席
     attached，同秒 reconcile_desktop_icons 的 any_fence_presented_on_
     desktop() 因 `attached.contains`  conjunct 变假→走保底
     restore_desktop_now 放出原生图标，下一两拍修好又藏回=完整往返。
     修法：UiState.last_healthy_ms 记录每栅栏最近"全条件就绪"时刻，
     就绪判定对 8s 内健康者放行（宽限 > 最大自愈延迟）。任何把"短暂
     z 失位/瞬态外来窗"放大成桌面级回退的判定，都必须挂健康宽限。
     复现工具 tools/menurepro.ps1（纯 PostMessage，无真实输入、无锁屏
     风险）；注意 taskkill+start 竞态瞬间两代实例并存会让探针/walk 看
     到"另一代实例的真实栅栏"充当拦路者——排查时先确保单实例。
   - **⑥ 走查预算与垃圾层现实（2026-08-28，勿回退）**：企业环境里各软件
     把辅助窗 HWND_BOTTOM 沉底，隐形垃圾一层层垫在宿主与栅栏之间（单日
     实测 ~369 层，只会更多）。后果：walk 预算 320 在 ~330 处耗尽永远摸
     不到栅栏 → `NOT FOUND` 风暴 + strikes 飙升（栅栏其实全程可见无遮挡，
     纯日志/自愈语义问题）。已改：预算 320→1000，报文区分 budget
     exhausted/top reached。**自愈语义不变的关键**：失位判定靠"遇到可见
     外来窗先于栅栏"（out_of_band），隐形垃圾只消耗步数不构成失位；所以
     预算调大不会把真浮窗放行（真浮窗必然先撞见可见 app 窗）。另：栅栏
     沉到垃圾层之下属正常漂移（视觉无差异），不需要每层纠正。
     bandwalk2.ps1 可打印自家窗口在链上的精确步位（bandwalk.ps1 是前 30
     步简版）。repair FAILED 日志（错误码+锚点）保留，防再次出现"修复
     静默无效"无处下手。
   - **⑦ 公开发布流程（GitHub）**：
     **双仓架构（2026-09-11 起，勿回退）**：origin =
     zghehehe/DeskFence-private（**私有仓，永不公开**，单分支 master
     含全部历史/AGENTS.md/tools/docs/website，日常开发推送默认去这里）；
     public = zghehehe/DeskFence（**对外发布仓**，远程只有 main 分支 +
     v0.1.x tag + Releases；2026-09-11 已删除其远程 master，副本在私仓
     与本地）。**防呆教训（2026-09-11 用户点破）：GitHub 仓库转 Public
     后所有分支与全部提交历史任何人可克隆浏览，"主页默认展示 main"
     不提供任何隐私保护——master 绝不能推到对外仓**（含内部细节的
     中文提交史会被一起公开）。对外仓现仍 Private（用户决定暂不转
     公开），转公开时机由用户网页操作；转公开后验证：匿名访问只见
     main、releases/latest/download/deskfence.exe 直链可用。发布动作 =
     组装 main（见下）后 `git push public main` + `git push public <v* tag>`
     （tag 推到 public 触发 Actions build+Release）。
     公开仓库只含必要代码——src/、Cargo.*、
     build.rs、DeskFence.rc、app.manifest、assets/deskfence.ico、
     resources/deskfence.res、README、LICENSE、.gitignore、
     .cargo/config.toml（crt-static 单文件）、.github/workflows/release.yml。
     **不发布**：tools/、docs/、website/、AGENTS.md。构建资源已预编译
     （build.rs 直接链接 res，不再调 windres——改图标后本地手动重生成）。
     发布分支=孤儿分支 main（git plumbing 组装，不动工作树）：
     export GIT_INDEX_FILE=$PWD/.git/pub-idx
     git read-tree --empty
     git add .cargo .github src resources assets/deskfence.ico \
       assets/deskfence-icon.svg docs/demo.svg \
       Cargo.toml Cargo.lock build.rs DeskFence.rc app.manifest \
       README.md LICENSE .gitignore
     T=$(git write-tree); git commit-tree $T -p main -m sync | xargs git branch -f main
     unset GIT_INDEX_FILE && rm -f .git/pub-idx
     **组装后必须 `diff <(git ls-tree -r --name-only v0.1.0) <(git ls-tree -r --name-only main)`
     核对文件清单**——2026-08-31 v0.1.1 按旧列表漏了 deskfence-icon.svg,README
     图标 404,被迫重写 main 历史。新加 README 引用的文件时同步更新此列表。
     打 tag v* 推到 public 后 Actions 自动 build+单文件校验+发 Release。
     crt-static 经 .cargo/config.toml 全局生效（exe 仅依赖系统库）。
     **官网**：website/index.html 是单文件官网（内联 SVG 动画、零依赖），
     含下载(直链 releases/latest/download/deskfence.exe)/隐私/支持三节。
     发布= tools/deploy-site.sh → 推到独立公开仓库 zghehehe.github.io
     （GitHub 用户主页域名 https://zghehehe.github.io）。代码仓保持纯净，
     site 与代码彻底分离。注意：**zghehehe.github.io 尚未创建**
     （2026-09-11 实测 404，deploy-site.sh 保留待用）；对外仓 Private
     期间官网下载/源码链接对外 404——公开推广前先把 zghehehe/DeskFence
     转 Public（转公开前提=双仓架构已落地、远程无 master）。
     GitHub 提交身份一律中性：zghehehe + zghehehe@users.noreply.github.com
     （publish/deploy 脚本内已强制 env，勿用工作身份提交公开内容）。
     **main 历史已于 2026-08-28 强制重写**（旧根含 README 占位符低级错误）：
     远端 main 与 tag v0.1.0 均为全新单根提交 9ddeea5，旧提交在远端不可达。
     此后 publish 脚本 -p main 正常追加即可，勿再重建根；README 用相对路径
     引用 assets/deskfence-icon.svg 与 docs/demo.svg（GitHub README 的 <img>
     对 SVG 渲染/播放 CSS 动画均正常，已实测）。
     **真实桌面截图严禁入库/公开（2026-08-28 已全删 docs/assets/*.png）**：
     含个人文件名、内网工具名与企业定制壁纸文字。README 配图=docs/demo.svg
     （website 首屏卡通动画的自包含版，内联全部 keyframes；GitHub README
     经 <img> 引用可正常播放 CSS 动画；校验 XML 合法性要用 XmlDocument.Load，
     PS5.1 Get-Content 会按 GBK 误读 UTF-8 报假错）。
     **发布说明纪律（2026-09-10 用户要求，长期有效）**：
     - 版本说明必须**先经用户逐字确认**才能提交/发布，不得先斩后奏。
     - 说明只写用户可感知的能力与修复；**严禁个人环境信息**——具体
       软件品牌（会议/输入法/安全软件）、企业/内网字样、内部工程细节
       （测试数/门禁/发布自动化方式），一律脱敏。公开源码注释同标准
       （2026-09-10 已清：输入法品牌注释中性化；输入法窗口类名常量
       属功能必需保留）。
     - 载体分工：公开 main = 每版本**一个孤儿提交**、标题一行短句
       （如 "DeskFence v0.1.2"）；全量说明放 annotated tag 注释，
       workflow 提取为 Release body。**提取坑（2026-09-10 实锤）**：
       actions/checkout 会把 tag 简化成指向提交的轻量引用，
       `git tag -l --format='%(contents)'` 只返回一行 commit message；
       必须显式 `git fetch --force origin refs/tags/<tag>:refs/tags/<tag>`
       重取 tag 对象后 `git cat-file tag <tag> | sed '1,/^$/d'` 剥头。
       不用 commit message 当说明——否则 commits 页与 Release 页双份长文。
     - 公开 main 重建 = 组装孤儿根 + `git push --force public main`
       覆盖（先例 2026-08-28、2026-09-10）；旧版本 v0.1.x tag 锚定
       各自历史链，源码包与 Release 不受影响。
     - 发布后 Releases 页可能有残留 Draft（删 tag 会把旧 Release 转
       Draft；Actions 也可能留草稿）——Draft 永远浮在列表最上方，
       由用户网页删除；本机无 GitHub API 凭证（无 gh/无 token），
       Release 增删改只能网页操作。
8. **ink 常驻渲染（2026-08-26 重构，勿回退）**：精确模式不再"整窗不透明+
   烙壁纸快照"——draw_fence 只铺 1/255 隐形底（ULW 按逐像素 alpha 做命中
   测试，没有它栅栏空白区会点击穿透！），真壁纸从栅栏底下**逐帧透出**
   （DWM 合成），换壁纸背景同帧跟随，"先保留 1s 旧壁纸再切换"结构性消失。
   快照管线（ensure_wallpaper/PrintWindow/wallpaper.bin）原样保留，角色变为
   **标签种子**。文字统一走 `gdi_draw_labels_seeded`（两种渲染模式共用）：
   标签矩形先垫种子（表面覆盖层预乘色 P 合成到快照壁纸色 W：P+W*(1-a)，
   即 hover/选中/边框压在壁纸上的 straight 结果），DrawShadowText ClearType
   对种子烘焙（与旧精确模式同渲染器同参数），RGB 偏离种子的像素=墨水置
   alpha=255，其余像素恢复垫前原状。无快照时黑种子兜底。旧的
   gdi_draw_labels（整面 alpha=255）与 D2D draw_label 已删。
   稳态已验证：与旧精确模式全屏 diff 逐位一致（仅桌面时钟/托盘像素差，
   栅栏区 0 差）。**待人工验证**（被 9 的锁屏阻塞）：栅栏空白区点击命中、
   换壁纸过渡观感、两态 NCC 基线。
   - **Step-3（2026-08-26）阴影墨水化**：seeded 后处理把"三通道全变暗"
     的像素判为阴影，反推覆盖率 c=1-D/S（与种子取值无关！），输出纯黑
     alpha 墨（叠加表面覆盖层贡献 P*(1-c)、alpha=1-(1-a_p)(1-c)）——
     阴影换壁纸瞬间即精确、重烘焙不变色，消除"1s 后阴影跳变"；只有
     1px 字形 ClearType 边缘仍随种子重烘焙（原生同级，不可感知）。
   - **Step-3 懒捕获**：UiState.wallpaper_dirty_since；壁纸失效后的重
     捕获要求 距失效>1.5s（淡入结束）且 >5s 无交互（常规场景仍 2.5s），
     成功捕获即清零。修"换壁纸后几秒内点击闪"（PrintWindow 强制宿主
     重绘不再与交互赛跑）。follow 的 12 次重试预算在 5s 安静门槛下会
     自然耗尽，由 catchup/稳态 3s 节拍完成捕获——正常。
   - **Step-4 透明模式并入种子管线（2026-08-26）**：启动缓存加载/暖场、
     3s 跟随节拍、签名轮询全部不再限定精确模式——透明模式有快照就用
     真实种子（渲染与精确模式一致），捕获真不可用时才黑种子兜底。
     栅栏拖动两模式统一走 refresh_fence（种子随位置变化）。**动态壁纸
     强制回退已退役**（dynamic_wallpaper_active 已删）：统一渲染后降级
     无意义且会静默改写用户配置；仅保留"捕获反复失败→回退透明"（精确
     模式启动守卫仍等首帧种子，捕获永不成功时需要这个出口）。实测透明
     模式文字为 ClearType+阴影（GDI seeded 特征，D2D 无此观感）。
10. **三态桌面 + 持久化（2026-08-27，用户选定方案）**：托盘菜单两个状态
    感知切换项——按钮1"隐藏全部栅栏⇄显示全部栅栏"（隐藏=**纯净态**：
    栅栏与原生图标都藏，桌面只剩壁纸，ZEN_MODE 标记）；按钮2"恢复原始
    桌面⇄恢复栅栏桌面"（原生图标接管）。desktop_state(normal/zen/native)
    持久化在 settings.json，启动按此恢复（启动不再无条件显示栅栏）。
    **三个坑（勿回退）**：① show_all_fences 的"从未呈现→回退显示图标"
    兜底在 zen/native 会把启动隐藏立即翻转——必须 `&& desktop_state()
    == "normal"` 才兜底；② zen 启动时栅栏永不呈现，reconcile 不会藏
    图标，需在配置加载处显式隐藏（保留接管标记），且 reconcile 纯净
    分支要"主动维持"（Explorer 早期初始化可能异步重显列表，每秒只读
    可见性、失配才重藏）；③ 切换命令要先写 desktop_state 再调
    show_all_fences（它按状态决定 hidden 位）。验收：tools/statetest.ps1
    （三态标签+转换）+ tools/persystest.ps1（zen/native 重启保持）。
9. **无边框常显（2026-08-26，用户选定方案）**：draw_fence 的边框/标题/
   淡底/四角 L 形手柄全部只在 show_chrome（悬停或拖拽中）绘制，默认态
   完全干净。悬停复用 fence_hover 的 400ms 延迟提交（防闪血统）；命中
   与缩放热区是位置判定+1/255 隐形底，与边框可见性无关，未改。
   已知取舍：空栅栏/折叠栅栏平时完全不可见（鼠标扫过才浮现）。
   验证注意：SetCursorPos 不产生 WM_MOUSEMOVE（悬停不会触发），须用
   mouse_event(MOVE) 相对移动；且本机 DPI 放大会让注入坐标偏移
   （120,540 实际落在 176,887），落点要用 GetCursorPos 回读确认。
9. **SPES 拦截模拟输入（2026-08-26 实测）**：mouse_event/keybd_event 的
   **按键/点击**类合成输入会立即触发锁屏（前台变"Windows 默认锁屏界面"），
   随后 CopyFromScreen 报"句柄无效"、SendKeys 报"拒绝访问"、#32768 菜单
   枚举全空——全部是锁屏的表现，不是代码 bug。纯 SetCursorPos /
   MOUSEEVENTF_MOVE 不触发。**自动化点击/键盘验证在本机已不可用**
   （历史上 uitest 鼠标测试可用，策略显然收紧了）；截图/WindowFromPoint/
   枚举类 API 探测在解锁态可用。显示器休眠后 SetCursorPos 挪一下即醒。
   另：桌面壁纸本身带**实时时钟**（动态壁纸，约 (791,192)-(1120,299)），
   截图对比时该区域永远在变，diff 结论要把它排除。
11. **z 序守卫体系（2026-08-28 终修，勿回退）**：两个用户可见症状
   （"Win+D 后栅栏几秒不出现"、"栅栏偶尔浮在应用窗上几秒后消失"）
   的完整机制与修复，全部有 wdprobe 实测背书：
   - **机制**：显示桌面（Win+D/三指下滑/ToggleDesktop）把栅栏压到
     宿主 Progman **之下**（壁纸后面，vis=1 不可见）。该操作**既不发
     WM_WINDOWPOSCHANGING 也不发 WM_WINDOWPOSCHANGED**（两轮探针零
     命中）——历史上那套 CHANGING 翻 HIDE 位 + WM_SIZE 兜底对这条路径
     从未生效过，恢复全靠 3 拍自愈（1.4-3s）。"浮在应用上"则是恢复
     过渡期应用窗从栅栏下方穿过 + 走查把"栅栏上方有可见外来窗"误判
     为出带、迟到的 repair 又把栅栏压下去的复合表现。
   - **修复三层**（ui.rs，全部可独立回退）：① `ZIntent` 线程局部意图
     守卫——自家所有对栅栏窗口的 SetWindowPos/ShowWindow 必须
     `z_scope(ZIntent::*)` 包裹（同线程同步触发消息 vs 外部经消息泵
     派发，天然区分；后台线程只做文件 IO 不碰窗口）；**新增自家定位
     调用时记得带意图，否则会被自己的守卫否决**。② CHANGING 里对外部
     操作注入 SWP_NOZORDER（外部 HWND_TOP 推顶被当场挡下，elevtest
     验证）——**只对真栅栏生效**（GWLP_USERDATA≠0）：菜单宿主等辅助窗
     的 z 无关紧要，IME 子系统会周期性重排它们，否决只会招来无限重试
     的对抗循环（2026-08-28 实测菜单宿主每 3-5s 被重排一次）。
     ③ WinEvent 双钩子（MINIMIZESTART..END + SHOW..REORDER，
     out-of-context + SKIPOWNPROCESS）→ 合并投递 WM_DL3_ZCHECK →
     高速自检。**别用 REORDER-only 钩子**：事件按窗口属主进程过滤，
     自家栅栏被沉的事件被 SKIPOWNPROCESS 滤掉，只能靠"别的窗口被
     批量操作"的事件当触发器；**SHOW(0x8002) 必须包含**——恢复方向
     的窗口重现只发 SHOW，缺它恢复过渡完全无触发；LOCATIONCHANGE
     太热不采用。
   - **桌面态快速轮询（2026-08-28 终版，勿回退）**：三指手势的
     窗口扫动**不发任何 WinEvent**（FOREGROUND/LOCATIONCHANGE 钩子路线
     两次实测失败已 revert，勿再走）——恢复过渡检测改走轮询：走查每轮
     更新 `band_quiet`（全部栅栏健康且带内无可见外来窗=桌面态），
     TIMER_DESKTOP_WATCH 以 250ms 节奏在桌面态跑 zcheck_fences_now
     （非桌面态空转，实测 CPU +0.2%）。下压限速 600ms 且**额度只在真正
     动作时消耗**。**"粘底"(z-glue,归位宿主正上方)已撤销**：紧贴宿主
     =站在菜单开合的底层扰动区,托盘/倒三角菜单每次关闭的系统静默重排
     都会把栅栏沉到宿主之下再被拉回=可见闪屏(2026-08-28 实测);深漂移
     在垃圾层之上反而是历史验证过的安全位置,勿以"稳态好看"为由重新
     粘底。bandtest 的 phaseB 断言已随双通道语义更新
     （lowers 也算 heals，仅风暴或 flag-不-heal 才 FAIL）。
   - **恢复过渡浮窗（2026-08-28 用户实测第二症状）**：Win+D 切回应用
     时，应用窗被成批插到低位再逐个升起，期间**已渲染的窗口位于栅栏
     之下**，栅栏压在它们上面直到走查 3 拍修复=用户看到"回应用后栅栏
     浮几秒才消失"。修：`fence_lower_if_blocked`——高速自检里发现
     "第一个可见外来窗先于栅栏"就把栅栏压到该窗正下方（判据与主走查
     同源：band_invisible/band_aux/自家栅栏，勿再复制粘贴）；全局限速
     1.5s/次，把 SPES 钩子层反复插队可能形成的对抗循环封顶。与走查
     收敛于同一稳态（栅栏贴在最低可见窗之下时，走查从宿主先遇到
     栅栏=健康，无乒乓）。验收 tools/lowertest.ps1（人为把可见窗插到
     栅栏下方，4s 内全部压回其下）。
   - **kill-switch**：settings.json `z_guard:false` 一键回退纯自愈。
   - **走查防抖改故障签名**（WalkFault：Blocked{hwnd+类哈希}/
     NotFoundTop/NotFoundBudget）：签名变化即重置拍数——恢复过渡期
     每拍不同的应用窗不再累计到第 3 拍（旧误报源）；常驻拦截者语义
     不变（3 拍 + 3/13/23 退避）。**沉底（top reached）首拍即修**；
     **预算耗尽只记日志不修**（状态不明）。strike 推进按 500ms 墙钟
     限速（global_tick 会同秒双调 ensure_all_attached）。
   - **repair 锚点重试**：宿主正上方若是高完整性窗（SPES 钩子层），
     以其为锚报 0x80070005（历史 20 次，fence4 曾失踪 3350 拍）——
     失败沿链向上换锚，最多 3 个。**别改成锚宿主本身**：
     hWndInsertAfter=X 语义是"落在 X 正下方"，锚宿主会把栅栏放到
     壁纸后面（2026-08-28 想当然犯过）。
   - **菜单宿主类名已独立**为 DeskFenceMenuHost（原复用栅栏类名，
     探针数出 6 个"栅栏"、drag_elevate_anchor 兄弟扫描会被它干扰）。
   - **run.log 已带本地日期**；walk-break 带宿主句柄。
   - 验收工具：`tools/wdprobe.ps1`（ToggleDesktop 双向 + 120ms 变化
     驱动采样，本项修复的主验证器）、`tools/junkcensus.ps1`（band
     垃圾层普查）、`tools/dfclasses.ps1`（自家窗口类清单）；
     `tools/wdtest.ps1` 采样过慢（首拍 +0.3s、每秒一拍）抓不到
     165ms 内的沉底-归位，勿再用它下结论。
   - 垃圾层现状（junkcensus 实测）：宿主上方 ~400 层，394 隐藏/
     275 微尺寸/251 离屏/20 cloaked；静态垃圾视觉零干扰，唯一会
     动的是 SPES 的 ScW（约 10 个，部分 topmost）。走查只对"可见
     在屏内外来窗"敏感的设计是对的，勿给 ScW 加类名白名单。
12. **环境污染是真实故障模式（2026-08-29 终教训，勿再犯）**：长时间
    排查（几十次杀进程、往 z 栈注入测试窗、双实例互殴、反复桌面切换）
    会把 Explorer 桌面层弄脏——**同一份代码在脏环境里表现异常**，导致
    多轮把环境症状误判为代码 bug、越修越差。防治纪律：
    - **金版本 tag**：`golden-2026-08-29`（= 2b51566，用户验证过
      "栅栏永远在桌面，Win+D/三指双向无浮窗"）。回归命令：
      `git reset --hard golden-2026-08-29`。
    - **排查口诀：先体检，再重置，最后才动代码**。任何"又出问题了"
      先跑 `tools/envcheck.ps1`（只读）；DIRTY 就跑 `tools/envreset.ps1`
      （杀应用→重启 Explorer→等宿主→重启应用→验证），**用同一个
      二进制复测**；干净环境下仍异常才允许查/改代码。
    - **测试纪律**：只读探针（枚举/截图）优先；必须注入窗口的测试
      （bandtest/lowertest 类）控制次数并在长会话后 envreset；
      **模拟沉底优先用 ToggleDesktop（系统真实路径），勿用
      HWND_BOTTOM 手工注入**（后者制造非真实状态且污染栈）。
    - 改代码前必须先有**干净环境下的复现路径**，无复现不动手
      （2026-08-29 无复现乱改连毁四个版本的教训）。
    - 机器判定（探针/探测器）通过≠用户体感通过，涉及闪屏/浮窗的
      改动必须等用户实测确认再提交。

13. **菜单后点空白闪屏——带底 churn 区与就位锚点（2026-08-29 终修，勿回退）**
    **【2026-09-10 机制已换代，见条目 14：churn/深度门槛/深位回退已删，
    本节仅存史，勿按此恢复】**：
    症状=托盘/倒三角菜单关闭后点桌面空白，桌面快速小闪。机制：Win+D 后
    `fence_reanchor_if_below_host`/走查修复把栅栏拉回 `desktop_insert_after`
    （=宿主正上方=z 栈 1-5 步）——正是 2b51566 点名的 menu-churn 区；菜单
    关闭的系统**静默重排**（不发 CHANGING/CHANGED——CHANGING 否决本就无
    条件拦外部 z 变更，静默路径绕过它）把带底栅栏压到宿主下，高速 zcheck
    再 5 连发拉回=整面 DWM 重合成=闪。日志指纹：`track dismissed` 后紧跟
    5 条 `re-anchored above host after external move`。修复（全在 ui.rs）：
    - `band_attach_anchor`：就位锚点（创建/走查修复/re-anchor/拖拽落位
      四处统一）="最低**可见且非 topmost** 外来窗"正下方。应用态=最低
      应用窗（实测 ~380 层深位，数百层垃圾与带底扰动区绝缘）；显示桌面
      态无此类窗→兄弟归队/带底回退（该状态非 topmost 区仅 ~8 层，本无
      避风港，属已知边界，回应用后由晋升送回深位）。
    - **topmost 一律不作锚、不做下探**：带内堆着大量 topmost 风格的隐形
      翻转垃圾（Outlook ATL/tooltip、SPES ScW 钩子层），活跃瞬间冒充
      "最低可见外来窗"；实测"以 topmost 为界向下探到非 topmost 窗"的
      终点是不受过滤保护的 MSCTFIME UI（IME 翻转窗），锚它=栅栏留在带底
      扰动区、晋升永不触发（第一版实踩，下探已删）。
    - `band_aux` 新增 MSCTFIME UI / Default IME（可见性与矩形随输入焦点
      振荡，空闲时 0x0 矩形，explorer 属主；零像素/瞬态不可能遮挡桌面
      内容，与 EdgeUi 输入条同法理容忍）。
    - 走查健康分支新增 **churn 区晋升**：健康但 depth≤12 且解析出的锚点
      比当前位深 20+ 层（滞后防抖）→一次性晋升到锚点下（SetWindowPos 带
      沿链换锚重试×3，防高完整性锚 0x80070005 静默失败）；深位健康栅栏
      绝不重排（重排本身=重合成闪）。`promote-diag` 限频 30s 留诊断。
    - 验证基准：干净环境 boot 后 bandwalk 应见栅栏 ~378-384 步（最低
      应用窗正下方）；envreset 后相同。**环境被反复桌面切换污染后
      ToggleDesktop 会卡死（FG 停在 Progman/ScW，切了没反应）**——验证
      桌面切换行为前先 envcheck，连续切换控制在 2-3 次内，脏了就 envreset。
    - 排障工具链教训：PowerShell 委托回调（EnumWindows 的 delegate）里
      直接 Write-Output 会被吞，收集进 ArrayList 回来再打印；探针结论
      （"窗口消失了"）先怀疑探针再用 FindWindow 复核。
    - **60ms zwatch 终局机制（2026-08-29 下午，勿回退）**：菜单关闭的
      静默沉底=系统把菜单宿主连同其 z 邻居（=紧贴带底的整个连续块，
      12:33:31.469 实测恰好是栅栏 1-5+菜单宿主 6，块外 ATL@7 不动）
      整帧压到宿主之下，高速自检 ~60ms 后拉回=栅栏消失 0.1-0.8s=
      "菜单后点空白轻微闪"的真身（flashdet：菜单本体仅 ~20k 采样px，
      闪=56k-129k 连续 1s 的栅栏列blink）。**应用态栅栏在 ~380 深位离
      菜单宿主十万八千里=天然免疫**；显示态才中招。修复链=主规则锚
      （最低可见非 topmost 外来窗=parked 应用窗）+晋升滞后 6 层——
      Win+D 后 1s 内把栅栏送进 parked 应用窗之下（~12-18 步，已离开
      1-6 沉底块）。
    - **晋升三重门（勿简化）**：①目标比当前深 +6 层（滞后防抖；曾用
      +20 会把"从带底送进 parked 窗之下十几层"的机会挡掉=12:22:58
      沉底实锤）；②目标自身 depth>12（防"锚点在底部簇内穿插"的
      1Hz 晋升振荡风暴，12:48 实测）；③锚必须非 topmost 窗。
    - **topmost band 死路（勿再试）**：插到 topmost 窗正下方会把栅栏
      并入 topmost band（实测 5 栅栏 topmost=True，切回应用=全屏浮窗）；
      SetWindowLongW 清不掉 WS_EX_TOPMOST（win32k 强制 band 管理，
      日志骗人、实测位还在）；HWND_NOTOPMOST 会把窗口移到非 topmost
      带顶部=位置不可控。整个"用 topmost 垃圾做锚+事后清位"路线已
      撤销，源码注释有墓碑。
    - **ToggleDesktop 反复切换会失灵**（FG 卡 Progman/ScW，COM 调用
      no-op）：一次调试会话 COM 切换控制在 2-3 次内；失灵后真实
      Win+D 可解，或 envreset。
    - **残留边界（待用户实测分流）**：显示态下若全部应用窗都"最小化
      停泊"（非 live-parked）且 topmost 垃圾丛林下方无非 topmost 隐形
      窗可垫——栅栏只能留带底，菜单关闭仍可能轻微闪。候选下一步：
      菜单宿主独立线程（怀疑沉底块按线程分组，宿主与栅栏分线程即可
      解耦——待验证块边界是线程还是 z 连续性）。
    - **⑧ 显示桌面态 topmost 免疫（2026-08-29 终修，勿回退）**：用户
      Case A/B 对比实锤——Win+D/三指（ToggleDesktop 批停泊）后菜单关
      闭必闪；逐个最小化回桌面（不进停泊批）后不闪。且停泊批只认
      "曾经被切换沉底过的窗口"：12:33:31 沉底块恰好=栅栏簇+菜单宿主，
      parked 窗(17-21)不被波及。**topmost 窗不参与停泊**（ScW/隐形
      丛林每次切换纹丝不动）→终修:`shown_topmost_tick`(走查末尾,
      ensure_all_attached 尾部调用)在"无任何可见非 topmost 外来窗"
      (band_has_live_foreign,与 resolver 主规则同源)稳定 2 拍后给
      全部栅栏 HWND_TOPMOST=免疫;出现可见应用窗立即 HWND_NOTOPMOST
      +zcheck 下压重归深位(被恢复扫动遮蔽)。**只有 SetWindowPos 的
      HWND_TOPMOST/NOTOPMOST 能改 topmost 位**(SetWindowLongW 改不动,
      勿再试);NOTOPMOST 会把窗口抬到非 topmost 带顶=位置不可控,
      摘除路径只动真正 topmost 的栅栏(先查位)。免疫期间:走查对
      topmost 栅栏按健康豁免(防与免疫互殴)、zcheck 不下压(防模式
      抖动)。进入瞬间(切换沉底→拉回带底→1s 后跳 topmost)有一次
      z 跳变,发生在切换动画后——若用户可感知再前移到 reanchor 路径。
      日志指纹:`shown-topmost: mode ON/OFF`。COM ToggleDesktop 在
      一台机器上反复调用后会静默失灵(切换无效果、无日志),真实
      Win+D 或 envreset 可解——机器验证切换行为时 COM 调用控制在
      2-3 次内。
      **【2026-09-10 已退役，勿按此节恢复】** topmost 免疫整套
      （shown_topmost_tick/SHOWN_PENDING 快速通道/band_has_live_
      foreign 判据链）已随所有权架构删除：它就是"会议窗场景误判
      live=false → 栅栏 topmost 压在应用上、任务栏点不动"的根源。
      沉底问题由条目 14 的 owned 窗口在结构上阻断，本节仅存史。
    - **快速通道(同日补,勿回退)**:①进入——`fence_reanchor_if_
      below_host` 沉底瞬间若 `band_has_live_foreign()==false` 直接
      HWND_TOPMOST(不等走查 2 拍;否则用户"刚到桌面就点菜单"仍在
      闪窗期内,2s→~0.2s 且被切换动画遮蔽;误判由走查退出路径自纠);
      ②退出——zcheck_fences_now 开头检测"免疫中+出现可见应用窗"立即
      摘除+重归位(恢复扫动第一批 WinEvent 毫秒级到达,不等 1s 走查
      ="回应用偶现浮窗"的主潜伏期);③摘除=SetWindowPos 直接插到
      `band_attach_anchor` 解析的最低可见外来窗之下——**绝不用
      HWND_NOTOPMOST 做第一选择**(它会先把栅栏抬到非 topmost 带顶
      =浮在应用上再等人压,只留作锚解析失败兜底)。
    - **【2026-09-10 判据已换代】**本节(含 13 条)的 churn 区/带底
      扰动区/深度门槛(CHURN_BLOCK_DEPTH)/深位垫窗回退全部随条目 14
      的所有权架构删除,勿再按"锚点要躲开带底"的旧语义改代码。
      现行锚点语义只剩一条:`resolve_band_anchor` 单遍受限扫描,
      不越过最低真实可见应用窗,topmost 只作终止边界,坏锚重试走
      同一规则;栅栏沉不到宿主之下(owned),无需再躲任何带底区块。

14. **沉底根治——栅栏所有权架构（2026-09-10 终修，勿回退）**：
    旧架构的根本缺陷是栅栏为**无 owner 的顶层 WS_POPUP**，能否留在
    桌面全靠自愈代码反复重排 z 序。菜单关闭/Win+D 时系统把"菜单宿主
    所在线程的连续 z 段"整块静默压到宿主之下（发 CHANGING 但带
    NOMOVE|NOSIZE 的 z 变更可被否决；纯静默路径不可），栅栏跟着沉底、
    自愈再逐个拉回=整组闪。四轮失败（20 步深度门槛 297eca7、分隔窗
    ee0ed92、放宽免疫 0754d03、延迟拉回 fac5531）都在治标，没有改变
    "栅栏可以被压到壁纸下面"这个前提。终修（7858ab6）：
    - **栅栏 = 桌面宿主持有的顶层 owned 窗口**：create_fence_window
      的 CreateWindowExW 第 9 参从 HWND(0) 改为 desktop_shell_window()
      （Win32 官方语义：owned 窗口必须永远在 owner 之上；注意不是
      WS_CHILD/SetParent——分层子窗口挂 Progman 下不会绘制，历史已踩）。
      效果：系统沉底动作从"静默发生、事后拉回"变成"CHANGING 消息
      可达、z 守卫当场否决"。日志指纹从 `track dismissed` 后 6 条
      `re-anchored` 变成 6 条无害的 `external z change vetoed
      flags=0x217`（0x200 位=代码统一注入的 SWP_NOOWNERZORDER，
      防 owned 调整带动 owner）。
    - **锚点解析单遍化**：`resolve_band_anchor`（纯函数，可单测）+
      `band_attach_anchor(host, skip)` 两参包装。规则：从宿主向上
      一遍扫描；真实可见非 topmost 外来窗是硬上界（能锚它就锚，它是
      坏锚才用此前扫过的隐形垫窗/兄弟栅栏）；topmost 一律终止搜索；
      所有出口排除宿主/自身/topmost/坏锚；无合格候选返回 None 不动
      （绝不回退宿主/HWND_TOP）。调用方：创建(ui.rs 传实际 hwnd)、
      reanchor、走查 repair、lower（下压也走同一 resolver，修掉了
      "紧贴 blocker 上方被 GW_HWNDNEXT 反向邻接误判为已归位"的 bug）。
    - **删除两套互殴机制**：churn 区健康晋升（promote-*）、topmost
      免疫全套。删晋升的依据：owned 关系已保证宿主下界，健康栅栏
      不需要也不应该再被主动重排（重排本身=重合成闪）。删免疫的
      依据：会议窗 live 误判压应用（任务栏点不动）已被用户实锤两次。
    - **重呈现与 z 解耦**：global_tick 的恢复分支只对"缺 presented/
      缺 surface"的栅栏 ULW 重提交（fence_needs_presentation），
      三拍防抖期的纯 z 失位不再触发全组重呈现；present_fence_only
      会回写 presented 位。
    - **repair 防抖**：Blocked 的 attempt 现在受 advanced 门控，
      计数未推进的同一拍不再重复消费第 3/13/23… 拍的修复机会。
    - **已知残留（2026-09-10）**：菜单打开瞬间偶发 1 次单栅栏
      re-anchor（11:29:08 实录，锚在深位，比旧版整组沉底轻一个量级）；
      周期性 veto 记录（Explorer/IME 每几分钟试一次重排 owned 链，
      全被否决）属正常噪声。
    - **验收基准**：三轮菜单开合日志零 re-anchor/repair/walk-break；
      sinkwatch 高频采样（tools/sinkwatch.ps1）数万帧 B 段（宿主下方）
      零 DeskFenceFence；owner 快照六栅栏 owner=Progman、非 topmost、
      全可见。回归工具：`powershell -File tools/sinkwatch.ps1 <ms> <out>`，
      判据 `B[0-9]+,.*,DeskFenceFence,` 计数必须为 0。

15. **文件重命名回车卡死（2026-09-11 终修，勿回退）**：用户重命名文件按
    回车即假死。完整根因链（5ab90fd 修）：
    - **换行进名字**：文件改名框是 ES_MULTILINE EDIT，WM_KEYDOWN 拦
      VK_RETURN 存在漏网路径（IME/前台转移），换行经 WM_CHAR 落进文本——
      修后 WM_CHAR 层再拦 0x0D/0x0A（多行 EDIT 一切文本插入的必经之路，
      最后一道闸，勿删）；粘贴路径另加清洗（换行/制表折空格、控制字符
      剔除，与原生一致）。
    - **自激死循环放大器=失败路径的模态 MessageBoxW**：名字带换行不在
      非法字符表里→rename 必败→弹模态框→编辑框失焦→WM_KILLFOCUS 自动
      提交路径 Post 新 COMMIT→模态消息循环把新 COMMIT 分发→重入失败→
      再弹框……日志指纹=同一秒几十条 `file rename commit`/`file rename
      failed` 交替。**改名失败处置永远不能用模态框**：现行为 MessageBeep
      +编辑框保持打开全选（原生同款，用户改完重试/Esc 取消）。
    - **防重入门闩** FILE_RENAME_COMMITTING：回车/失焦/WM_ACTIVATE/点击
      外部轮询四条提交源可同帧叠加，重入会对同一编辑框提交两次。
    - 非法字符检查补 `c.is_control()`（NTFS 禁控制字符，漏检=rename 必败）。
    - 空名/原名未变=直接关闭编辑框（不叮一声）；非法/失败=保持打开+叮。
    - windows crate feature 坑：MessageBeep 在 System::Diagnostics::Debug
      不在 WindowsAndMessaging；HGLOBAL 在 Foundation；剪贴板三件套需
      Win32_System_DataExchange+Memory。
    - **自救脚本**：仓库根 `恢复桌面.bat`（双击=taskkill 卡死实例+新进程
      `--restore-desktop` 恢复图标后退出；延迟用 ping 不用 timeout——
      Git Bash 的 GNU timeout 会抢名）。bat 必须 CRLF、纯 ASCII 内容。
    - **双击 exe=一键自救（2026-09-11 用户约定语义，勿改）**：启动时
      `clear_previous_instances()`（main.rs）先给旧实例托盘窗发 WM_CLOSE
      走 quit_app 完整清理，健康实例毫秒级自退，2s 超时才强杀兜底——
      **不判健康与否，只有先后顺序**，勿加 IsHungAppWindow 类判定
      （有误判边界）；`--restore-desktop` 分支同样先清场再恢复（活着的
      旧实例会把恢复的图标重新藏回，不清场自救无效）。任何启动都收敛到
      "唯一且健康"的新实例；托盘菜单负责日常退出。单实例靠 pid 枚举
      （shell::pids_by_name 排除自身），不是 CreateMutex——对卡死实例
      更鲁棒，勿改。
    - 用户"cmd 里没这个命令"的原因：PowerShell 不搜当前目录，需
      `.\deskfence.exe`；cmd 里需先 cd 到 exe 所在目录。

## 代码位置备忘

- 渲染：src/render.rs（ink 常驻：透明底+1/255 隐形命中层+seeded GDI 文字；
  透明/精确两模式共用同一文字管线，区别仅种子来源与启动守卫）
- 拖拽管线（2026-08-24 改为**插入式**，2026-09-08 三档语义分明）：
  自动档 = 插入线模式：拖动中被拖者跟手、其余完全不动，指示线
  （UiState.insert_line，overlay 绘制）提示插入点，松手按
  ui.rs `fence_insertion_plan` + model.rs `row_insert_layout` 落位
  （行内槽位模型，行贴顶由 settle 归一保证）；网格档 = 棋盘模式：
  只对齐最近图标格线，不吸附栅栏、无插入线（2026-09-08）；
  自由档 = 随手放 + 邻居磁吸（model.rs snap_gap_to_neighbors，x/y 双轴）。
  图标 = `update_ghost_preview` 只算目标槽与指示线，松手
  `reorder_paths_as_block` 拼接。（旧的实时挤压预览 preview_move_layout、
  chain_positions、对齐参考线死代码 guide_x/guide_y/update_guides/
  snap_candidates 均已删，别按旧文档找）
- 应用图标：矢量源 `assets/deskfence-icon.svg`，`python tools/rebuild_icon.py`
  重建 `assets/deskfence.ico`（需 `pip install resvg-py`），随后 cargo build
  --release 重新嵌入。
- 文档(2026-08-26 全面更新至 ink 常驻基线):`docs/architecture.html` = 全链路
  架构图(渲染模型/启动/渲染/壁纸/菜单/悬停/拖拽/自愈/退出 10 条链路 + 症状
  速查表,排障先看它);`website/index.html` = 商用官网(Apple 风格,全内联
  SVG 卡通演示+自动循环动画段,零外部依赖,截图资产已移除)
