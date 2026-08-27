# DeskFence 工作约定（给后续会话的 Agent）

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

## 代码位置备忘

- 渲染：src/render.rs（ink 常驻：透明底+1/255 隐形命中层+seeded GDI 文字；
  透明/精确两模式共用同一文字管线，区别仅种子来源与启动守卫）
- 拖拽管线（2026-08-24 改为**插入式**）：拖动中被拖者跟手、其余完全不动，
  指示线（UiState.insert_line，overlay 绘制）提示插入点，松手才拼接重排；
  栅栏 = ui.rs `fence_insertion_plan` + model.rs `chain_positions`（整链紧凑、
  放不下换行、fit_to_monitors 夹回不许出屏）；图标 = `update_ghost_preview`
  只算目标槽与指示线，松手 `reorder_paths_as_block` 拼接。自由移动档无插入线。
  （旧的实时挤压预览 preview_move_layout 已删，别按旧文档找）
- 应用图标：矢量源 `assets/deskfence-icon.svg`，`python tools/rebuild_icon.py`
  重建 `assets/deskfence.ico`（需 `pip install resvg-py`），随后 cargo build
  --release 重新嵌入。
- 文档(2026-08-26 全面更新至 ink 常驻基线):`docs/architecture.html` = 全链路
  架构图(渲染模型/启动/渲染/壁纸/菜单/悬停/拖拽/自愈/退出 10 条链路 + 症状
  速查表,排障先看它);`website/index.html` = 商用官网(Apple 风格,全内联
  SVG 卡通演示+自动循环动画段,零外部依赖,截图资产已移除)
