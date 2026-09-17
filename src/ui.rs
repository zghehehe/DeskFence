//! 窗口管理与交互：栅栏窗口、命中测试、移动/缩放/滚动、右键菜单、刷新(重命名子系统见 rename.rs)

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::BOOL;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ScreenToClient, HBRUSH};
use windows::Win32::System::Com::CoInitializeEx;
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
};
// windows crate 未导出的 WinEvent 标志(0.62 仍缺),按 WinUser.h 补定义
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::drag::*;
use crate::envhealth::*;
use crate::hosts::*;
use crate::iconcache::{load_icon_cache_file, save_icon_cache_file_now};
use crate::logging::log;
use crate::menu::{delete_fence_ex, quit_app, set_render_mode, show_tray_menu};
use crate::metrics::*;
use crate::model::{self, Fence, FileItem, Rect};
use crate::monitors::*;
use crate::ole;
use crate::present::*;
use crate::rename::*;
use crate::render;
use crate::render::{Renderer, Surface};
use crate::selfheal::*;
use crate::settings::*;
use crate::shell;
use crate::state::*;
use crate::wallpaper::{
    capture_fallback_due, is_black_frame, load_wallpaper_cache, save_wallpaper_cache,
    wallpaper_changed_under_fences,
};
use crate::winids::*;

const TIMER_GLOBAL: usize = 1;
/// 悬停延迟提交定时器（图标高亮与原生桌面一致需悬停 ~400ms 才出现）
pub(crate) const TIMER_HOVER: usize = 2;
const TIMER_ANIMATION: usize = 3;
/// 壁纸跟随定时器:Themes 目录事件后 250ms 防抖再捕获比对,
/// 未变化则短重试(Explorer 分多步写缓存、DWM 切换略有延迟)
const TIMER_WALLPAPER_FOLLOW: usize = 6;
/// 桌面态快速自检定时器:三指手势的窗口扫动不发任何 WinEvent,
/// 恢复过渡的检测只能靠轮询(见 zcheck_fences_now 注释)
const TIMER_DESKTOP_WATCH: usize = 7;
pub(crate) const RENAME_COMMIT_MSG: u32 = WM_USER + 1;
pub(crate) const RENAME_CANCEL_MSG: u32 = WM_USER + 2;

const TRAY_MSG: u32 = WM_APP + 1;
/// 第二实例请求:显示全部栅栏
const WM_DL3_SHOW_ALL: u32 = WM_APP + 2;
/// 键盘微调:移动栅栏(lparam = 虚拟键)
const WM_DL3_NUDGE: u32 = WM_APP + 3;
/// 文件操作键盘命令：wParam=fence_id，lParam=vk | ctrl<<16 | shift<<24
const WM_DL3_KEY: u32 = WM_APP + 4;
/// 点击了栅栏之外(桌面空白):wParam=x, lParam=y(屏幕坐标) → 清除选择态
const WM_DL3_CLEAR_SEL: u32 = WM_APP + 5;
/// 壁纸缓存目录(Themes\TranscodedWallpaper)有变化:幻灯片轮换/换壁纸的
/// 毫秒级事件信号,由目录 watcher 线程投递,UI 侧防抖后重捕获
const WM_DL3_WALLPAPER_DIRTY: u32 = WM_APP + 6;
/// 后台扫描完成:扫描线程投递,UI 线程在托盘消息里应用结果(apply_pending_scan)
const WM_DL3_SCAN_APPLY: u32 = WM_APP + 8;
/// 全局 z 序事件触发的高速自检请求(WinEvent 回调合并投递)
/// windows crate 未导出(0.62 仍缺),按 Win32 头文件补定义
const WM_MOUSELEAVE: u32 = 0x02A3;
static TICK_COUNT: AtomicU32 = AtomicU32::new(0);

/// Clear all transient interaction state before a fence lifecycle change.
fn clear_all_interaction(s: &mut UiState) {
    s.hover.clear();
    s.hover_pending.clear();
    s.hover_hit.clear();
    s.fence_hover.clear();
    s.fence_hover_pending.clear();
    s.selected_paths.clear();
    s.focused_path = None;
    s.selection_anchor = None;
    s.marquee = None;
    s.active_fence = None;
    s.drag = None;
    s.drag_ghost = None;
    s.ghost_preview = None;
    s.arrival_animations.clear();
}

pub(crate) fn clear_fence_interaction(s: &mut UiState, fence_id: u32) {
    s.hover.remove(&fence_id);
    s.hover_pending.remove(&fence_id);
    s.hover_hit.remove(&fence_id);
    s.fence_hover.remove(&fence_id);
    s.fence_hover_pending.remove(&fence_id);
    s.arrival_animations.retain(|a| a.fence_id != fence_id);
    let owns_interaction = s.active_fence == Some(fence_id)
        || s.drag.as_ref().is_some_and(|d| d.fence_id == fence_id)
        || s.ghost_preview
            .as_ref()
            .is_some_and(|p| p.fence_id == fence_id);
    if owns_interaction {
        s.active_fence = None;
        s.drag = None;
        s.marquee = None;
        s.drag_ghost = None;
        s.ghost_preview = None;
    }
    // Selection is global because pinned items can appear in multiple fences.
    // A lifecycle change invalidates any visual ownership, so clear it wholesale.
    s.selected_paths.clear();
    s.focused_path = None;
    s.selection_anchor = None;
}

pub(crate) fn finish_interaction_cleanup() {
    // SAFETY: ReleaseCapture 只作用于当前线程的鼠标捕获状态，无指针参数。
    unsafe {
        let _ = ReleaseCapture();
    }
    let s = state().lock().unwrap();
    if s.arrival_animations.is_empty() {
        if let Some(tray) = TRAY_HWND.get().copied() {
            // SAFETY: tray 是本进程创建的托盘窗口，句柄存活至退出；
            // 纯定时器调用，无指针参数。
            unsafe {
                let _ = KillTimer(Some(tray), TIMER_ANIMATION);
            }
        }
    }
    if s.drag_ghost.is_none() && s.arrival_animations.is_empty() {
        if let Some(hwnd) = s.guide_hwnd {
            // SAFETY: guide_hwnd 是本进程创建的引导窗（销毁时 wndproc 清槽），
            // 槽位非 None 即窗口仍在。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// 运行中同步图标大小：桌面改了图标大小后，定时器里检测并跟随
fn sync_icon_size() {
    let want = current_icon_size();
    if (want - model::icon_size()).abs() > 0.5 {
        model::set_icon_size(want);
        // 图标格距随新 pads 重算后,栅栏按原有"列数×行数"等比换算新宽高,
        // 与原生桌面 Ctrl+滚轮 改图标大小时的观感一致
        {
            let mut s = state().lock().unwrap();
            let updates: Vec<(u32, f32, f32)> = s
                .fences
                .iter()
                .map(|f| {
                    let metrics = s
                        .metrics
                        .get(&f.id)
                        .copied()
                        .unwrap_or_else(model::DpiMetrics::system);
                    let cols = ((f.rect.w - metrics.pad * 2.0) / metrics.cell_w)
                        .round()
                        .max(1.0);
                    let rows = ((f.rect.h - metrics.title_h - metrics.pad * 2.0) / metrics.cell_h)
                        .round()
                        .max(1.0);
                    let new_metrics = model::DpiMetrics::system();
                    (
                        f.id,
                        cols * new_metrics.cell_w + new_metrics.pad * 2.0 + 2.0,
                        new_metrics.title_h
                            + rows * new_metrics.cell_h
                            + new_metrics.pad * 2.0
                            + 2.0,
                    )
                })
                .collect();
            for (id, w, h) in updates {
                if let Some(f) = s.fences.iter_mut().find(|f| f.id == id) {
                    f.rect.w = w;
                    f.rect.h = h;
                }
            }
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
        // Cached pixel buffers were produced at the previous desktop icon size.
        // Keeping them would force an upscaled second pass and blur the icon.
        state().lock().unwrap().icon_cache.clear();
        log(&format!(
            "icon size synced to {}; fences rescaled to match",
            want
        ));
        // 格子变大后原矩形可能放不下:先 settle 归一再解重叠
        settle_all_fences();
        resolve_overlaps_by_rows();
        // 运行中重算后同步刷新 sidecar(保存时格距已变)
        save_layout_cells();
        show_all_fences();
    }
}

fn rebuild_render_resources() {
    let Some(renderer) = Renderer::new() else {
        log("renderer rebuild failed; keeping previous resources");
        return;
    };
    let old_surfaces: Vec<Surface> = {
        let mut s = state().lock().unwrap();
        let old = s.surfaces.drain().map(|(_, sf)| sf).collect();
        s.presented.clear();
        s.icon_cache.clear();
        s.renderer = Some(renderer);
        old
    };
    for sf in old_surfaces {
        render::release_surface(sf);
    }
    sync_icon_size();
    refresh_all_fences();
}

// ---------------- 初始化 ----------------

pub fn init() -> bool {
    let t0 = resize_now_ms();
    // SAFETY: 两个调用均无指针参数。进程级 DPI 感知只需启动时设置一次；
    // CoInitializeEx 把主线程初始化为 STA——后续全部 shell/COM 调用与窗口
    // 消息循环都在主线程，正是该初始化所覆盖的线程；失败只记一行日志
    // （后续壁纸签名轮询等 COM 能力各自静默降级），成功类结果（含
    // S_FALSE"已初始化"）无害忽略。
    unsafe {
        // Per-monitor v2 keeps physical pixels crisp when a fence moves between monitors.
        // Fall back for older Windows builds without changing any desktop setting.
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
            let _ = SetProcessDPIAware();
        }
        let hr = CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED
                | windows::Win32::System::Com::COINIT_DISABLE_OLE1DDE,
        );
        if hr.is_err() {
            log(&format!(
                "main thread CoInitializeEx failed: 0x{:08X}",
                hr.0
            ));
        }
    }
    // 必须在设置 DPI awareness 之后读 DPI，否则系统会按未感知返回 96。
    // 顺序：先设 DPI 与注册表留白，再实测图标尺寸（实测成功会同步覆盖留白为精确值）
    model::set_dpi_scale(dpi_scale());
    let (pad_x, pad_y) = shell::desktop_cell_pads();
    // 注册表 IconVerticalSpacing 不随 Ctrl+滚轮更新,极易过时(可能给出
    // <标签带的留白);钳到 MIN_PAD_Y,防栅栏内行与行重叠
    model::set_cell_pads(pad_x, pad_y.max(model::MIN_PAD_Y));
    model::set_icon_size(current_icon_size());
    // 系统右键菜单里的"重命名"改由栅栏内就地编辑完成
    shell::set_rename_request_hook(crate::rename::on_shell_rename_request);
    // 加载用户偏好（自动对齐等）
    let _ = align_mode(); // 预热(加载持久化档位)
    {
        let mut s = state().lock().unwrap();
        if s.renderer.is_none() {
            s.renderer = Renderer::new();
        }
        if s.renderer.is_none() {
            return false;
        }
    }
    register_class();
    // 诊断一行:图标/格距/留白(2026-09-16 排查"图标没跟中图标"时缺地面
    // 真值;注册表 Bag 值+实测格距都在这里留痕)
    log(&format!(
        "boot metrics: icon={}px cells {}x{} pads {:.0}x{:.0} (registry IconSize={})",
        model::icon_size(),
        model::cell_w(),
        model::cell_h(),
        model::cell_pads().0,
        model::cell_pads().1,
        shell::desktop_icon_size()
    ));
    log(&format!("boot init done ({}ms)", resize_now_ms() - t0));
    true
}

fn register_class() {
    // SAFETY(整块): 四个 wndproc 都是 unsafe extern "system" fn，签名与
    // WNDPROC 一致；类名字符串来自 winids.rs 的 NUL 结尾静态宽字符缓冲，
    // 终身有效；hinstance() 是本进程模块实例，进程内有效；hCursor/
    // hbrBackground 置空是 WNDCLASSW 允许的"无默认"（分层窗口不用类画刷）。
    // 重复注册同名类的失败被容忍（返回错误即可）。
    unsafe {
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(fence_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance(),
            hIcon: deskfence_icon(),
            hCursor: HCURSOR(std::ptr::null_mut()),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name(),
        };
        let _ = RegisterClassW(&wc);
        let wc2 = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(tray_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance(),
            hIcon: deskfence_icon(),
            hCursor: HCURSOR(std::ptr::null_mut()),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: tray_class_name(),
        };
        let _ = RegisterClassW(&wc2);
        let wc3 = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(guide_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance(),
            hIcon: deskfence_icon(),
            hCursor: HCURSOR(std::ptr::null_mut()),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: guide_class_name(),
        };
        let _ = RegisterClassW(&wc3);
        // 菜单前台宿主:沿用 fence_wndproc(与历史行为一致,仅类名独立,
        // 避免被探针/兄弟扫描误认;窗口过程按 GWLP_USERDATA 查不到即走默认路径)
        let wc4 = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(fence_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance(),
            hIcon: deskfence_icon(),
            hCursor: HCURSOR(std::ptr::null_mut()),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: menu_host_class_name(),
        };
        let _ = RegisterClassW(&wc4);
    }
}

/// 壁纸快照例行重捕获的最大间隔(兜底)。壁纸变化的主路径是事件驱动:
/// 手动换壁纸走 WM_SETTINGCHANGE(毫秒级);幻灯片轮换走 Themes 目录
/// watcher 或 IDesktopWallpaper 签名轮询(秒级,标准系统有效;部分定制/受管控
/// 环境两者皆不可用时退化为本兜底轮询)。
/// 2026-08-26 从 60s 放宽到 10min:ink 常驻后快照只作文字种子,不新鲜
/// 没有视觉代价;而每次稳态捕获的 PrintWindow 都会强制桌面宿主重绘,
/// 落在交互后的未稳态窗口就是用户偶发的"点击后闪一下"——捕获越少,
/// 命中敏感窗口的概率越小。真实壁纸变化仍由事件驱动毫秒级跟随。
const WALLPAPER_REFRESH_MS: u64 = 600_000;

/// 捕获/刷新各桌面宿主的壁纸像素(带节流)。快照的作用是**文字种子**与
/// 启动守卫(ink 常驻后栅栏背景实时透出,不再依赖快照)。失败不清空旧快照
/// (保留种子继续用,连续失败才触发精确模式回退透明)。原生桌面图标在
/// DeskFence 接管时是隐藏的,宿主(Progman/WorkerW)捕获到的就是纯壁纸;
/// 捕获结果疑似全黑(PrintWindow 对个别窗口会失败)同样视为失败。
fn ensure_wallpaper(s: &mut UiState) -> bool {
    let now = resize_now_ms();
    // wallpaper_ms == 0 是"强制重捕获/从未捕获"哨兵:进程刚启动的一个刷新周期内
    // now < WALLPAPER_REFRESH_MS,若用饱和减法判断,清零反而会被节流拦下,
    // 导致启动后整整一个刷新周期快照一直为空(首帧只能画 D2D 文字)。
    if s.wallpaper_ms != 0 && now.saturating_sub(s.wallpaper_ms) < WALLPAPER_REFRESH_MS {
        return false;
    }
    // 交互刚结束:推迟捕获。PrintWindow 会强制桌面宿主重绘,宿主在前台
    // 切换后的未稳定态下重绘会产生 ±4% 的亮度跳变(用户看到的"点桌面/
    // 关菜单后闪一下");稳态下的重绘无感。保持哨兵让下一个 tick 再试。
    //
    // 换壁纸后的重捕获懒化(2026-08-26):ink 常驻后快照只作文字种子,
    // 背景已实时透出,捕获没有抢时间的必要;而宿主颜色状态刚被壁纸切换
    // 重建,过早强制重绘会闪(用户"换壁纸后几秒内点击闪一下")。要求
    // 距壁纸失效>1.5s(DWM 淡入结束)且>5s 无交互才捕获;常规场景维持
    // 2.5s 推移不变。阴影已墨水化,种子晚到没有视觉代价。
    let dirty = s.wallpaper_dirty_since != 0;
    if dirty && now.saturating_sub(s.wallpaper_dirty_since) < 1500 {
        s.wallpaper_ms = 0;
        return false;
    }
    let quiet_ms = if dirty { 5000 } else { 2500 };
    if now.saturating_sub(LAST_INTERACTION_MS.load(Ordering::Relaxed)) < quiet_ms {
        s.wallpaper_ms = 0;
        return false;
    }
    s.wallpaper_ms = now;
    let hosts = desktop_hosts();
    // 防污染守卫:PrintWindow 拍的是桌面宿主,原生图标可见时会把图标一并
    // 拍进快照,栅栏拿它当不透明背景就会烙下重影(启动"乱七八糟"的根源)。
    // 捕获期间临时隐藏图标列表,拍完立即恢复;仅当栅栏尚未接管桌面时才需要
    // 恢复(已接管时图标本就处于隐藏态,不触碰)。等一帧合成时间,避免拍到
    // 隐藏前的旧内容。
    let hidden_for_capture = !DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed)
        && !NATIVE_DESKTOP_OVERRIDE.load(Ordering::Relaxed)
        && hosts.iter().any(|h| h.visible)
        && set_desktop_icons_visible(false);
    if hidden_for_capture {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut caps = Vec::new();
    let mut fail_dbg: Vec<String> = Vec::new();
    for host in hosts.iter().filter(|h| h.visible) {
        match shell::capture_window_pixels(host.hwnd) {
            Ok((px, w, ph)) => {
                // 黑帧=换壁纸过渡期暂态(判定见 is_black_frame 注释):
                // 不是捕获失败,绝不能计入 wallpaper_fails
                if is_black_frame(&px, w, ph) {
                    // 壁纸切换过渡期 DWM 会给宿主刷纯黑:这是暂态内容,
                    // 不是捕获失败——绝不能计入 wallpaper_fails(会堆积触发
                    // "回退透明"误落盘),也不能当日志刷屏。节流记录一次即可。
                    let last = EMPTY_CAPTURE_LOG_MS.load(Ordering::Relaxed);
                    let now = resize_now_ms();
                    if now.saturating_sub(last) > 5000 {
                        EMPTY_CAPTURE_LOG_MS.store(now, Ordering::Relaxed);
                        log("wallpaper capture looks empty (transition); keep old snapshot");
                    }
                    continue;
                }
                caps.push(render::WallpaperPixels {
                    px,
                    w,
                    h: ph,
                    origin_x: host.x as i32,
                    origin_y: host.y as i32,
                });
            }
            Err(reason) => {
                fail_dbg.push(format!(
                    "hwnd=0x{:x} {}x{}: {}",
                    host.hwnd.0 as usize, host.w as i32, host.h as i32, reason
                ));
            }
        }
    }
    if hidden_for_capture {
        let _ = set_desktop_icons_visible(true);
    }
    if caps.is_empty() {
        // 只有真实捕获失败(PrintWindow 报错/宿主缺失)才计入回退计数;
        // 纯黑过渡帧走上面的 continue,不进 fail_dbg。两种情况都保留旧快照:
        // 清空会让栅栏刷新退化成 D2D 文字帧再切回,文字阴影会闪。
        if !fail_dbg.is_empty() {
            s.wallpaper_fails = s.wallpaper_fails.saturating_add(1);
            let n = s.wallpaper_fails;
            if n <= 2 || n.is_multiple_of(25) {
                log(&format!(
                    "wallpaper capture empty (fails={}): visible_hosts={} [{}]",
                    n,
                    hosts.iter().filter(|h| h.visible).count(),
                    fail_dbg.join("; ")
                ));
            }
        }
        false
    } else {
        s.wallpaper_fails = 0;
        s.wallpaper_dirty_since = 0; // 懒捕获任务完成:种子已就绪
                                     // 区域感知比较:只有栅栏底下的像素变了才算"变"(时钟壁纸的分钟
                                     // 跳动不再触发全量重绘与缓存落盘)。快照本体总是更新,种子保持
                                     // 最新;changed=false 时调用方不重绘,切换无感。
        let t0 = resize_now_ms();
        let changed = wallpaper_changed_under_fences(&s.wallpapers, &caps, &s.fences);
        s.wallpapers = caps;
        if changed {
            save_wallpaper_cache(&s.wallpapers);
        }
        log(&format!(
            "wallpaper capture ok ({}ms, changed={})",
            resize_now_ms() - t0,
            changed
        ));
        changed
    }
}

/// "捕获到纯黑过渡帧"的节流日志时间戳
static EMPTY_CAPTURE_LOG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 壁纸变化(手动换壁纸/主题切换/幻灯片轮换)时请求重捕获。保留旧快照
/// (栅栏不呈现退化帧),清零节流哨兵,并同时武装追赶与跟随定时器:
/// 跟随定时器做"捕获-比对-变了才重绘",捕获到过渡黑帧时自动重试。
pub(crate) fn invalidate_wallpaper() {
    if let Ok(mut s) = state().try_lock() {
        s.wallpaper_ms = 0;
        s.wallpaper_fails = 0;
        s.wallpaper_dirty_since = resize_now_ms();
    }
    arm_wallpaper_catchup();
    arm_wallpaper_follow();
}

/// 壁纸跟随的重试计数(目录事件后最多 ~3s 内跟踪到内容变化)
static WALLPAPER_FOLLOW_RETRIES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// 武装壁纸跟随定时器(250ms)。重触发即重置计数。
fn arm_wallpaper_follow() {
    WALLPAPER_FOLLOW_RETRIES.store(0, Ordering::Relaxed);
    if let Some(&tray) = TRAY_HWND.get() {
        // SAFETY: tray 是本进程托盘窗口；纯定时器调用，无指针参数。
        unsafe {
            let _ = SetTimer(Some(tray), TIMER_WALLPAPER_FOLLOW, 250, None);
        }
    }
}

/// 壁纸跟随:Themes 目录事件触发的"捕获-比对-变了才重绘"。Explorer 写
/// 缓存是临时文件+改名的多步操作,DWM 切换也可能略晚于文件落盘,首次
/// 捕获可能与旧快照相同——此时每 250ms 重试,内容真正变化或超时才停。
fn wallpaper_follow_tick(hwnd: HWND) {
    let changed = {
        let mut s = match state().try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        s.wallpaper_ms = 0;
        ensure_wallpaper(&mut s)
    };
    let n = WALLPAPER_FOLLOW_RETRIES.fetch_add(1, Ordering::Relaxed) + 1;
    if changed || n >= 12 {
        // SAFETY: hwnd 是收到 WM_TIMER 的托盘窗口（本进程所有），纯定时器调用。
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_WALLPAPER_FOLLOW);
        }
        WALLPAPER_FOLLOW_RETRIES.store(0, Ordering::Relaxed);
        if changed {
            log("wallpaper follow: content changed -> redraw fences");
            refresh_all_fences();
        }
    }
}

/// 壁纸追赶:强制重捕获,成功则整帧重绘全部栅栏并协调原生图标可见性
/// (首帧就绪 → 原生图标隐藏,一次原子切换);连续 ~30s 仍失败则交还给
/// 3s 稳态节拍与"捕获反复失败 → 回退透明"守护。
fn wallpaper_catchup_tick(hwnd: HWND) {
    let tries = WALLPAPER_CATCHUP_TRIES.fetch_add(1, Ordering::Relaxed) + 1;
    let ok = {
        let mut s = match state().try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        s.wallpaper_ms = 0;
        ensure_wallpaper(&mut s);
        !s.wallpapers.is_empty()
    };
    if ok {
        // SAFETY: hwnd 是托盘窗口（本进程所有），纯定时器调用。
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_WALLPAPER_CATCHUP);
        }
        WALLPAPER_CATCHUP_ARMED.store(false, Ordering::Relaxed);
        log(&format!(
            "wallpaper catch-up captured after {} tries ({}ms)",
            tries,
            resize_now_ms()
        ));
        refresh_all_fences();
        reconcile_desktop_icons();
    } else if tries >= 150 {
        // SAFETY: 同上：托盘窗口的定时器，纯调用无指针参数。
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_WALLPAPER_CATCHUP);
        }
        WALLPAPER_CATCHUP_ARMED.store(false, Ordering::Relaxed);
        log("wallpaper catch-up gave up; steady tick takes over");
    }
}

/// 重绘每个栅栏，最后才决定是否临时隐藏原生图标。它是 Win+D、托盘
/// “显示全部”和 Explorer 重建后的统一恢复入口。
pub fn show_all_fences() {
    // Remove windows left behind by undo/reset before creating the current layout.
    // 尊重持久化的桌面状态:zen/native 下栅栏应保持隐藏。菜单"显示全部
    // 栅栏/恢复栅栏桌面"与第二实例唤起都会先把状态置回 normal 再调用,
    // 因此这里按状态决定 hidden 位;启动恢复与 Explorer 重启恢复由此
    // 保持在用户选择的状态。
    let want_visible = desktop_state() == "normal";
    let orphaned: Vec<HWND> = {
        let mut s = state().lock().unwrap();
        for fence in &mut s.fences {
            fence.hidden = !want_visible;
        }
        let valid: HashSet<u32> = s.fences.iter().map(|f| f.id).collect();
        let orphan_ids: Vec<u32> = s
            .windows
            .keys()
            .filter(|id| !valid.contains(id))
            .copied()
            .collect();
        let mut windows = Vec::new();
        for id in orphan_ids {
            if let Some(h) = s.windows.remove(&id) {
                windows.push(h);
            }
            if let Some(sf) = s.surfaces.remove(&id) {
                render::release_surface(sf);
            }
            s.metrics.remove(&id);
            s.presented.remove(&id);
            s.attached.remove(&id);
        }
        windows
    };
    for h in orphaned {
        // SAFETY: h 是刚从 state.windows 摘下的孤儿栅栏窗口（本进程创建、
        // 摘除后不再有任何引用）；先注销 OLE 拖放注册再销毁——销毁后注册
        // 将指向已死窗口；DestroyWindow 投递 WM_DESTROY 走 fence_wndproc
        // 的清理路径（同线程，同步完成）。
        unsafe {
            let _ = RevokeDragDrop(h);
            let _ = DestroyWindow(h);
        }
    }
    // 不能信任 Explorer 切换桌面前的 WorkerW 缓存。
    invalidate_hosts_cache();
    // Keep the already rendered fence pixels on screen during reattachment.
    // Only fall back to Explorer icons when no fence has ever presented.
    // 纯净/原生态下栅栏"从未呈现"是设计使然,绝不能触发这个兜底——否则
    // zen 启动的图标隐藏会被它立即翻转(实测:隐藏后 0.1s 内被重显+清标记)。
    if state().lock().unwrap().presented.is_empty() && desktop_state() == "normal" {
        let _ = set_desktop_icons_visible(true);
        DESKTOP_ICONS_HIDDEN.store(false, Ordering::Relaxed);
        model::clear_icons_marker();
    }
    ensure_all_attached();
    let ids: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences.iter().map(|f| f.id).collect()
    };
    for id in &ids {
        ensure_fence_window(*id);
    }
    if BOOT_VERBOSE.load(Ordering::Relaxed) {
        log(&format!("boot windows created ({}ms)", resize_now_ms()));
    }
    // 批量呈现:所有栅栏先在隐藏状态下画完并提交 ULW(像素暂存),然后一
    // 次性放行。放行循环只有 ShowWindow 系统调用(~µs/个),DWM 同帧合成,
    // 用户看到的是全部栅栏同一帧弹出——而不是"画完一个亮一个"、首末相差
    // 整个串行绘制时长的扫过感。已可见窗口(运行期刷新)不受影响。
    {
        let mut s = state().lock().unwrap();
        s.defer_show_until_batch = true;
    }
    for id in &ids {
        refresh_fence(*id);
    }
    {
        let mut s = state().lock().unwrap();
        s.defer_show_until_batch = false;
        let t0 = resize_now_ms();
        let mut n_shown = 0usize;
        for f in s.fences.iter() {
            if f.hidden {
                continue;
            }
            if let Some(h) = s.windows.get(&f.id) {
                let _z = z_scope(ZIntent::Show);
                // SAFETY: h 是本进程栅栏窗口；z_scope(ZIntent::Show) 声明
                // 这是自家显示操作，放行 z 守卫（守卫只否决外部重排）。
                unsafe {
                    let _ = ShowWindow(*h, SW_SHOWNOACTIVATE);
                }
                n_shown += 1;
            }
        }
        if BOOT_VERBOSE.load(Ordering::Relaxed) {
            log(&format!(
                "boot batch show {} fences took {}ms",
                n_shown,
                resize_now_ms() - t0
            ));
        }
    }
    reconcile_desktop_icons();
}

/// 缺类补建:按当前分类规则找出"有文件但无对应栅栏"的类别并新建栅栏,
/// 返回新建的类别名。rescan 与 boot 共用——只改分类规则(如 md 文档→代码)
/// 不动文件集合,rescan 的"无变化早退"永远等不到补建,boot 也必须跑一遍,
/// 否则受影响文件无栅栏可归=隐身(2026-09-01)。
pub(crate) fn ensure_missing_category_fences(s: &mut UiState) -> Vec<String> {
    let mut have: std::collections::HashSet<String> = s
        .fences
        .iter()
        .filter(|f| !f.category.is_empty())
        .map(|f| f.category.clone())
        .collect();
    let mut added = Vec::new();
    if auto_category() {
        // 动态分类表(2026-09-08):按可编辑表补建;表里新增的空分类因无
        // 文件不会在此建栏(由面板"新增"显式建),已删分类不再迭代=不复活
        for cat_def in model::category_table() {
            let cat: &str = &cat_def.name;
            if have.contains(cat) {
                continue;
            }
            if s.files.iter().any(|f| f.category == cat) {
                // 用户手动删过的分类在墓碑期内不复活;该类出现**新文件**
                // (mtime 晚于删除时刻)才清除墓碑并补建
                if let Some(ts) = category_tombstone_at(cat) {
                    let has_newer = s.files.iter().any(|f| f.category == cat && f.mtime_ms > ts);
                    if has_newer {
                        clear_category_tombstone(cat);
                        log(&format!("category fence '{cat}' resurrected by newer file"));
                    } else {
                        continue;
                    }
                }
                added.push(cat.to_string());
                have.insert(cat.to_string());
            }
        }
    } else if !have.contains(model::FALLBACK_CATEGORY) {
        // 自定义分类模式:未归位文件统一进兜底"其他",保证没有任何文件隐身
        // (2026-09-09 起不再自动新建"未分类"栅栏——切模式不冒出多余栅栏)
        added.push(model::FALLBACK_CATEGORY.to_string());
    }
    let mut new_ids = Vec::new();
    for cat in &added {
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        new_ids.push(max_id);
        // 尺寸按该类当前内容数收窄:不足 5 项宽 1 列(2026-09-02 用户要求)
        let count = s.files.iter().filter(|f| &f.category == cat).count();
        s.fences.push(Fence {
            id: max_id,
            title: cat.clone(),
            category: cat.clone(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: {
                let (dw, dh) = default_size_for_items(count);
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: dw,
                    h: dh,
                }
            },
            collapsed: false,
            scroll_rows: 0,
            locked: false,
            hidden: false,
            manual_size: false,
            sort_mode: model::default_sort_mode(),
        });
    }
    // 新栅栏落位:第一行最后一个栅栏右侧,靠顶对齐,保持默认间隔
    // (与手动新建同一规则 new_fence_rect,2026-09-02;旧的左上角落位会
    // 压到占住左上角的既有栅栏)
    for id in &new_ids {
        let Some((w, h)) = s
            .fences
            .iter()
            .find(|f| f.id == *id)
            .map(|f| (f.rect.w, f.rect.h))
        else {
            continue;
        };
        let rect = new_fence_rect(s, w, h);
        if let Some(fence) = s.fences.iter_mut().find(|f| f.id == *id) {
            fence.rect = rect;
        }
    }
    added
}

/// 桌面文件变更刷新
/// 空分类栅栏自动移除(2026-09-04 用户要求):分类成员走光(改名换类/
/// 删除)后栅栏不再占位,右侧栅栏经 delete_fence_ex 的行内左移补位。
/// 只处理分类栅栏(category 非空;手动新建的空栏是用户预留,不自动删);
/// 不记墓碑。逐个走 delete_fence_ex(含 undo/左移/落盘),须在 state 锁外调用。
fn remove_empty_category_fences() {
    let empty_ids: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences
            .iter()
            .filter(|f| !f.category.is_empty())
            .filter(|f| model::display_list(f, &s.files).is_empty())
            .map(|f| f.id)
            .collect()
    };
    for id in empty_ids {
        log(&format!("auto-removed empty category fence {}", id));
        delete_fence_ex(id, false);
    }
}

/// 重扫进行中标记:同一时刻至多一个后台扫描线程(重复请求合并进 REQUEUED)
static SCAN_INFLIGHT: AtomicBool = AtomicBool::new(false);
/// 扫描期间又来了重扫请求:本轮应用完再补一轮,收敛到最新状态
static SCAN_REQUEUED: AtomicBool = AtomicBool::new(false);
/// 后台线程产出的扫描结果(带取值时的代际),等 UI 线程取走应用(单槽)
static SCAN_RESULT: Mutex<Option<(u64, Vec<FileItem>)>> = Mutex::new(None);

/// 异步重扫入口:文件系统枚举+显示名解析(~百 ms 级 shell 调用,4 线程并行)
/// 全部搬到后台线程,结果经 WM_DL3_SCAN_APPLY 回 UI 线程应用——UI 线程不再
/// 被 watcher 事件/手动"刷新"卡住(2026-09-09,遗留#5;此前扫描在 UI 线程,
/// 54 文件冷缓存时栅栏交互可感知卡顿)。后台线程只做纯数据扫描,不碰任何
/// 窗口(自愈体系线程不变式);应用段(原 rescan 的 diff+重绘)全在 UI 线程。
/// 需要同步语义的调用方(改名提交:跨栏迁移动画必须排在扫描应用之后)
/// 用 rescan_now()。
pub fn rescan() {
    if SCAN_INFLIGHT.swap(true, Ordering::Relaxed) {
        SCAN_REQUEUED.store(true, Ordering::Relaxed);
        return;
    }
    let epoch = scan_epoch();
    std::thread::spawn(move || {
        let files = with_recycle_bin(shell::scan_desktop());
        *SCAN_RESULT.lock().unwrap() = Some((epoch, files));
        // TRAY_HWND 在托盘初始化时创建,rescan 的全部调用方都在其后;万一
        // 未就绪,结果留在槽里由 global_tick 兜底应用
        if let Some(tray) = TRAY_HWND.get().copied() {
            // SAFETY: 后台扫描线程对 UI 线程窗口的唯一合法触碰方式=
            // PostMessage 异步投递（消息由 UI 线程消息泵处理）；无指针参数，
            // 无共享内存。
            unsafe {
                let _ = PostMessageW(Some(tray), WM_DL3_SCAN_APPLY, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// 同步重扫(rescan 拆分前的原行为):扫描+应用一次完成,调用返回即生效。
/// 仅供改名提交使用——它已把内存文件列表同步到新路径,rescan 只为缺类
/// 补建+收敛,且迁移动画必须在应用之后排队(异步版做不到这个顺序)。
pub fn rescan_now() {
    invalidate_pending_scans(); // 在途异步快照已过时,丢弃(见 state.rs SCAN_EPOCH)
    apply_scan(with_recycle_bin(shell::scan_desktop()));
}

/// 取走后台扫描结果并应用(托盘 WM_DL3_SCAN_APPLY / global_tick 兜底)。
/// 代际失配的陈旧快照直接丢弃;应用完毕清 INFLIGHT,期间有新请求
/// (REQUEUED)则再起一轮。
fn apply_pending_scan() {
    let cur = scan_epoch();
    let pending = SCAN_RESULT.lock().unwrap().take();
    if let Some((epoch, files)) = pending {
        if epoch == cur {
            apply_scan(files);
        } else {
            log("stale scan snapshot dropped (memory synced behind scanner)");
        }
    }
    if SCAN_INFLIGHT.swap(false, Ordering::Relaxed) && SCAN_REQUEUED.swap(false, Ordering::Relaxed)
    {
        rescan();
    }
}

/// 应用一批扫描结果(必须 UI 线程):扫描宽恕合并、diff 判定、状态更新、
/// 缺类补建、重绘收敛。
fn apply_scan(mut files: Vec<FileItem>) {
    let (added_paths, removed_any, recat_any, gained_cats) = {
        let s = state().lock().unwrap();
        // 扫描宽恕:上一轮在册、本轮扫不到的路径,连续 SCAN_MISS_DROP 轮
        // 才真正移除(未达阈值时从上一轮找回,保持文件可见)——元数据瞬态
        // 读取失败不再引发"文件消失/栅栏重排"(用户实测"文档自动移位")
        {
            let mut miss = scan_miss_map().lock().unwrap();
            let present: std::collections::HashSet<String> =
                files.iter().map(|f| f.path.clone()).collect();
            for f in s.files.iter() {
                if present.contains(&f.path) {
                    miss.remove(&f.path);
                } else if shell::path_gone_from_disk(&f.path) {
                    // 磁盘上已确认不在(shell 菜单删除/外部进程删除):当轮移除,
                    // 不进宽恕。宽恕只保护"元数据瞬态锁导致 read_dir 漏读"——
                    // 那种情况属性查询依然成功。没有这层,外部删除的图标只能
                    // 等宽恕轮数收敛,表现为"明明删了,栅栏里还在"。
                    miss.remove(&f.path);
                    log(&format!(
                        "scan: '{}' gone from disk, removed immediately",
                        f.path
                    ));
                } else {
                    let c = miss.entry(f.path.clone()).or_insert(0);
                    // 计数递增(2026-09-09 修复):此前 c 从不递增,自然消失路径
                    // 永远到不了阈值=删掉的文件图标永久滞留(拖拽删除不受影响,
                    // 那条路 mark_scan_removed 直接写满阈值)
                    if *c < SCAN_MISS_DROP {
                        files.push(f.clone());
                    }
                    *c += 1;
                }
            }
            miss.retain(|k, _| present.contains(k) || s.files.iter().any(|f| f.path == *k));
        }
        let added = model::newly_added_paths(&s.files, &files);
        let new_set: std::collections::HashSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();
        let removed = s.files.iter().any(|f| !new_set.contains(f.path.as_str()));
        // 分类漂移也算变化(2026-09-03):同名文件的分类变了(改名内存同步
        // 后、或将来分类规则调整)不能走"无变化早退"——早退会跳过
        // ensure_missing_category_fences,改名成 mp4 的文件永远留在文档栏
        let mut old_cats: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for f in s.files.iter() {
            old_cats.insert(f.path.as_str(), f.category.as_str());
        }
        let recat = files.iter().any(|f| {
            old_cats
                .get(f.path.as_str())
                .is_some_and(|&c| c != f.category)
        });
        // 有成员"迁入"的分类(2026-09-03):外部改名/分类规则调整导致某文件
        // 分类变化,与在应用内改名同权——清该分类墓碑,否则墓碑挡住缺类补
        // 建,迁入成员无栏可归=隐身
        let mut gained: Vec<String> = Vec::new();
        for f in files.iter() {
            if let Some(old) = old_cats.get(f.path.as_str()) {
                if *old != f.category && !gained.contains(&f.category) {
                    gained.push(f.category.clone());
                }
            }
        }
        (added, removed, recat, gained)
    };
    // 文件集合没有任何变化:桌面目录的文件系统事件(Explorer 的元数据
    // 触碰、菜单交互的伴生事件)不值得做任何重绘。此前的无条件
    // show_all_fences 让每次 watcher dirty 都全量重绘 5 个栅栏,
    // 表现为点桌面/关菜单后栅栏区域闪一下。
    // 改名提交置位 RENAME_RESCAN_PENDING 强制走一遍:补建缺类栅栏+收敛
    // (swap 副作用仅在 diff 为空时求值,与原嵌套 if 语义一致)。
    if added_paths.is_empty()
        && !removed_any
        && !recat_any
        && !RENAME_RESCAN_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    let new_cats: Vec<String> = {
        let mut s = state().lock().unwrap();
        s.files = files;
        // 桌面变更可能同时改变快捷方式目标、文件关联或 Shell overlay 状态。
        // 缓存键还包含像素尺寸，因此扫描时统一失效可避免保留陈旧图标。
        let keep: std::collections::HashSet<String> =
            s.files.iter().map(|f| f.path.clone()).collect();
        // 只清已消失文件的图标缓存(2026-09-09:原实现全清,桌面一有变化
        // 全部图标重新 SHGFI 提取=可感知的卡顿)
        s.icon_cache.retain(|k, _| {
            k.split('\0')
                .next()
                .map(|p| keep.contains(p))
                .unwrap_or(false)
        });
        // 已删文件的常用记录同步剔除(2026-09-03):usage.json 残留旧路径时,
        // 同名新建会继承旧次数直接顶到"常用"第一位(用户实测)
        let pruned = model::prune_usage(&keep);
        if pruned > 0 {
            log(&format!("pruned {pruned} stale usage entries"));
        }
        s.selected_paths.retain(|p| keep.contains(p));
        if s.focused_path.as_ref().is_some_and(|p| !keep.contains(p)) {
            s.focused_path = None;
        }
        if s.selection_anchor
            .as_ref()
            .is_some_and(|p| !keep.contains(p))
        {
            s.selection_anchor = None;
        }
        for fence in &mut s.fences {
            fence
                .item_order
                .retain(|p| keep.contains(p) || fence.pinned.contains(p));
        }
        // 迁入分类的墓碑清理必须在补建之前(否则墓碑挡路,迁入成员隐身)
        for cat in &gained_cats {
            if category_tombstone_at(cat).is_some() {
                clear_category_tombstone(cat);
                log(&format!(
                    "category '{cat}' tombstone cleared by incoming member"
                ));
            }
        }
        let new_cats_inner = ensure_missing_category_fences(&mut s);
        // 到达顺序登记进 item_order(2026-09-04):新出现/迁入的成员追加到
        // 所在栅栏拖拽顺序表末尾,排序按"先来在左、后来靠右"——迁移来的
        // 文件不再因旧 mtime 排到最前面
        // MutexGuard 的 Deref 不支持字段级分裂借用:先克隆文件列表
        let files_snapshot = s.files.clone();
        for fence in s.fences.iter_mut() {
            let items = model::display_list(fence, &files_snapshot);
            let missing: Vec<String> = items
                .iter()
                .filter(|it| !fence.item_order.contains(&it.path))
                .map(|it| it.path.clone())
                .collect();
            fence.item_order.extend(missing);
        }
        new_cats_inner
    };
    // 栅栏创建已全部收口在 ensure_missing_category_fences 内部(单一创建
    // 来源)。此前这里还有第二个创建循环——rescan 路径每个新分类会建出
    // 两个同名栅栏(2026-09-02 修"mp3 一来冒出两个媒体")。
    // 只有真的新增了分类栅栏才收敛；周期 rescan 不应把用户手动摆放的
    // 位置重排回左上角（推挤式保留相对位置 + 行贴顶归一，而不是流式重排）。
    if !new_cats.is_empty() {
        settle_preserve_positions();
    }
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    rebuild_pins();
    refit_auto_fence_heights();
    // 空分类栅栏自动移除(右侧左移补位)——先于 show_all_fences,避免
    // 空栏闪现;不记墓碑,该类再来文件时缺类补建照常重建
    remove_empty_category_fences();
    // 先排队入场动画再渲染栅栏(2026-09-03):栅栏帧按 hide_arrivals 跳过
    // 飞行中成员的墨水——新文件"先落在桌面格、飞入落地后才在栅栏显形";
    // 若先渲染后排队,文件会瞬间出现在栅栏里,动画沦为重复影子
    start_arrival_animations(&added_paths);
    show_all_fences();
}

/// 默认栅栏尺寸:2 列宽 × 4 行高(与 build_global_config 同规则;内容超出自动滚动)
pub(crate) fn default_fence_size() -> (f32, f32) {
    let (title_h, pad) = model::chrome(model::dpi_scale());
    (
        model::cell_w() * 2.0 + pad * 2.0 + 2.0,
        title_h + model::cell_h() * 4.0 + pad * 2.0 + 2.0,
    )
}

/// 按内容数给默认尺寸(2026-09-02 用户要求):不足 5 项宽 1 列,≥5 项宽 2 列;
/// 高固定 4 行。与首次运行布局(build_global_config)同一规则
pub(crate) fn default_size_for_items(n: usize) -> (f32, f32) {
    let cols = if n < 5 { 1usize } else { 2usize };
    let (title_h, pad) = model::chrome(model::dpi_scale());
    (
        model::cell_w() * cols as f32 + pad * 2.0 + 2.0,
        title_h + model::cell_h() * 4.0 + pad * 2.0 + 2.0,
    )
}

/// 新栅栏落位(2026-09-02 统一规则,手动/自动新建共用):第一行最后一个
/// 栅栏右侧,与其靠顶对齐、保持默认间隔;无栅栏时放工作区左上角;
/// 行尾放不下夹回屏内(残余重叠由随后的 settle 推开兜底)。
pub(crate) fn new_fence_rect(s: &UiState, w: f32, h: f32) -> Rect {
    let visible: Vec<Rect> = s
        .fences
        .iter()
        .filter(|f| !f.hidden && !f.collapsed)
        .map(|f| f.rect)
        .collect();
    let rows = model::rows_from_rects(&visible);
    let (x, y) = match rows.first() {
        Some(row) if !row.is_empty() => {
            let last = &visible[row[row.len() - 1]];
            (last.x + last.w + model::GAP, last.y)
        }
        _ => {
            let (vx, vy, _, _) = work_area();
            (vx, vy)
        }
    };
    let r = Rect { x, y, w, h };
    let (vx, vy, vw, vh) = work_area_for_rect(&r);
    let mut tmp = [r];
    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
    tmp[0]
}

/// 扫描结果注入回收站虚拟条目(固定显示在"软件"栅栏第一位,可拖拽文件进去删除)
fn with_recycle_bin(mut files: Vec<FileItem>) -> Vec<FileItem> {
    if !files.iter().any(|f| model::is_recycle_bin(&f.path)) {
        files.push(model::recycle_bin_item());
    }
    files
}

fn client_area_animations_enabled() -> bool {
    // SAFETY: SPI_GETCLIENTAREAANIMATION 契约要求 pvParam 指向 BOOL；
    // enabled 是栈变量，调用期间有效；失败路径不写并返回 false（视为
    // 系统禁用动画，保守跳过动画只出最终帧）。
    unsafe {
        let mut enabled = BOOL(1);
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut BOOL as *mut std::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && enabled.as_bool()
    }
}

/// 在桌面(围栏之外)找一个空图标格,作为新文件"先落在桌面"的原生落点。
/// 列优先扫描(与 Explorer 桌面排列一致),跳过与任何可见栅栏相交的格子;
/// used 收集本次动画已占用的格子避免多个新文件叠在一起。
fn desktop_free_slot(s: &UiState, used: &mut Vec<Rect>) -> Option<(f32, f32)> {
    let (vx, vy, vw, vh) = work_area();
    let cw = model::cell_w();
    let ch = model::cell_h();
    let cols = (vw / cw).floor() as i32;
    let rows = (vh / ch).floor() as i32;
    if cols < 1 || rows < 1 {
        return None;
    }
    let margin = 2.0;
    for col in 0..cols {
        for row in 0..rows {
            let x = vx + margin + col as f32 * cw;
            let y = vy + margin + row as f32 * ch;
            let cell = Rect { x, y, w: cw, h: ch };
            let blocked = s
                .fences
                .iter()
                .any(|f| !f.hidden && model::intersects(&cell, &f.rect))
                || used.iter().any(|u| model::intersects(&cell, u));
            if !blocked {
                used.push(cell);
                return Some((x + (cw - model::icon_size()) / 2.0, y + 4.0));
            }
        }
    }
    None
}

/// 定位能展示 path 的栅栏及其可见槽位的图标屏幕坐标(图标左上角)。
/// auto_scroll=槽位在滚动页外时把栅栏滚到该行(新建入场用);false=页外
/// 直接返回 None(迁移动画抓旧栏起点用,不应为起飞而滚动旧栏)。
pub(crate) fn fence_slot_screen_pos(
    s: &mut UiState,
    path: &str,
    auto_scroll: bool,
) -> Option<(u32, (f32, f32))> {
    let item = s.files.iter().find(|item| item.path == path)?.clone();
    let mut fence = s
        .fences
        .iter()
        .find(|fence| {
            fence.pinned.contains(&item.path)
                || fence.category.is_empty()
                || fence.category == item.category
        })?
        .clone();
    if fence.hidden || fence.collapsed {
        return None;
    }
    let items = model::display_list(&fence, &s.files);
    let index = items.iter().position(|c| c.path == item.path)?;
    let metrics = s
        .metrics
        .get(&fence.id)
        .copied()
        .unwrap_or_else(model::DpiMetrics::system);
    let mut layout = model::layout_with_metrics(&fence, items.len(), &metrics);
    if index < layout.first_index || index >= layout.first_index + layout.visible {
        // 新条目落在滚动页外(2026-09-03):"常用"排序下新文件使用次数为
        // 零只能排最后,内容超一页的栅栏(如文档 2×4=8 槽)会把它排进
        // 第二页——旧逻辑这里静默跳过,新文件既无动画也看不见落在哪
        // (用户实测"新建没动画"的真因)。改为把栅栏滚到该条目所在行,
        // 让飞入落点可见;连滚都滚不到(不可能:行数≤总行数)才放弃。
        if !auto_scroll {
            return None;
        }
        let cols = layout.cols.max(1);
        let row = index / cols;
        let max_scroll = layout.total_rows.saturating_sub(layout.rows);
        if max_scroll == 0 {
            return None;
        }
        let scroll = row.min(max_scroll);
        fence.scroll_rows = scroll;
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence.id) {
            f.scroll_rows = scroll;
        }
        layout = model::layout_with_metrics(&fence, items.len(), &metrics);
        if index < layout.first_index || index >= layout.first_index + layout.visible {
            return None;
        }
        log(&format!(
            "arrival: scrolled fence {} to row {} for off-page item",
            fence.id, scroll
        ));
    }
    let (cell_x, cell_y) = model::cell_pos_with_metrics(&layout, index, &metrics);
    let icon_offset = (metrics.cell_w - metrics.icon_px) / 2.0;
    Some((
        fence.id,
        (
            fence.rect.x + cell_x + icon_offset,
            fence.rect.y + cell_y + 4.0,
        ),
    ))
}

fn start_arrival_animations(added_paths: &[String]) {
    if added_paths.is_empty() || !client_area_animations_enabled() {
        return;
    }
    let now = resize_now_ms();
    let mut s = state().lock().unwrap();
    let mut pending = Vec::new();
    let mut used_slots: Vec<Rect> = Vec::new();
    for path in added_paths.iter().take(6) {
        if model::is_recycle_bin(path) {
            continue;
        }
        let Some(item) = s.files.iter().find(|item| &item.path == path).cloned() else {
            continue;
        };
        let Some((fence_id, to)) = fence_slot_screen_pos(&mut s, path, true) else {
            continue;
        };
        let metrics = s
            .metrics
            .get(&fence_id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        // 新文件先"落在桌面空白处"(围栏外的原生网格位),停留片刻再飞入栅栏;
        // 找不到围栏外空位时回退为从栅栏标题中心飞出
        let from = desktop_free_slot(&s, &mut used_slots).unwrap_or_else(|| {
            let (fx, fy, fw) = s
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| (f.rect.x, f.rect.y, f.rect.w))
                .unwrap_or((0.0, 0.0, 200.0));
            (
                fx + fw * 0.5 - metrics.icon_px * 0.5,
                (fy + metrics.title_h * 0.5 - metrics.icon_px * 0.5).max(0.0),
            )
        });
        pending.push(ArrivalAnimation {
            fence_id,
            path: item.path,
            name: render::display_name(&item.name),
            from,
            to,
            // 停留 420ms 再起飞,保证"先出现在桌面"可被看见;多文件阶梯延迟
            started_ms: now + 420 + pending.len() as u64 * 90,
            duration_ms: 520,
        });
    }
    if pending.is_empty() {
        return;
    }
    for p in &pending {
        log(&format!(
            "arrival: animate '{}' fence={} from=({:.0},{:.0}) to=({:.0},{:.0})",
            p.name, p.fence_id, p.from.0, p.from.1, p.to.0, p.to.1
        ));
    }
    s.arrival_animations.extend(pending);
    ensure_guide_window(&mut s);
    if let Some(tray) = TRAY_HWND.get().copied() {
        // SAFETY: tray 是本进程托盘窗口；纯定时器调用，无指针参数。
        unsafe {
            let _ = SetTimer(Some(tray), TIMER_ANIMATION, 16, None);
        }
    }
    refresh_guide(&mut s);
}

/// 跨栏迁移动画(2026-09-03):改名改扩展名(如 mp3→md)导致分类变化时,
/// 从旧栏旧槽位飞向新栏新槽位——与新建入场动画共用同一 overlay 管线。
/// moves: (路径, 旧栅栏id, 旧槽位屏幕x, y)。必须在 rescan 之后调用:
/// 旧栏已不含该成员、新栏帧已渲染;排队后刷新新栏把成员藏到落地
/// (hide_arrivals),飞行由 TIMER_ANIMATION 驱动,落地由 tick 显形。
pub(crate) fn queue_migration_animations(moves: &[(String, u32, f32, f32)]) {
    if moves.is_empty() || !client_area_animations_enabled() {
        return;
    }
    let now = resize_now_ms();
    let mut refresh_ids: Vec<u32> = Vec::new();
    {
        let mut s = state().lock().unwrap();
        let mut queued_any = false;
        for (path, old_fence, fx, fy) in moves {
            let Some((fence_id, to)) = fence_slot_screen_pos(&mut s, path, true) else {
                continue;
            };
            if fence_id == *old_fence {
                continue;
            }
            let name = s
                .files
                .iter()
                .find(|i| &i.path == path)
                .map(|i| render::display_name(&i.name))
                .unwrap_or_default();
            log(&format!(
                "arrival: migrate '{}' fence {}->{} from=({:.0},{:.0}) to=({:.0},{:.0})",
                name, old_fence, fence_id, fx, fy, to.0, to.1
            ));
            s.arrival_animations.push(ArrivalAnimation {
                fence_id,
                path: path.clone(),
                name,
                from: (*fx, *fy),
                to,
                // 旧栏位置短暂停留(150ms)再起飞,飞行距离跨栏更远,不加停留
                started_ms: now + 150,
                duration_ms: 520,
            });
            if !refresh_ids.contains(&fence_id) {
                refresh_ids.push(fence_id);
            }
            queued_any = true;
        }
        if !queued_any {
            return;
        }
        ensure_guide_window(&mut s);
        refresh_guide(&mut s);
    }
    // 锁外刷新:refresh_fence 内部要拿 state 锁
    for id in refresh_ids {
        refresh_fence(id);
    }
    if let Some(tray) = TRAY_HWND.get().copied() {
        // SAFETY: tray 是本进程托盘窗口；纯定时器调用，无指针参数。
        unsafe {
            let _ = SetTimer(Some(tray), TIMER_ANIMATION, 16, None);
        }
    }
}

fn tick_arrival_animations() {
    let landed: Vec<u32>;
    {
        let mut s = state().lock().unwrap();
        if s.arrival_animations.is_empty() {
            return;
        }
        // 已落地(动画到期)的成员:记下栅栏,refresh_guide 的 retain 清掉
        // 它们之后逐栏刷新——栅栏此前按 hide_arrivals 跳过其墨水,落地即显形
        let now = resize_now_ms();
        landed = s
            .arrival_animations
            .iter()
            .filter(|a| now.saturating_sub(a.started_ms) > a.duration_ms + 180)
            .map(|a| a.fence_id)
            .collect();
        ensure_guide_window(&mut s);
        refresh_guide(&mut s);
        if s.arrival_animations.is_empty() {
            if let Some(tray) = TRAY_HWND.get().copied() {
                // SAFETY: tray 是本进程托盘窗口；纯定时器调用，无指针参数。
                unsafe {
                    let _ = KillTimer(Some(tray), TIMER_ANIMATION);
                }
            }
            if s.drag_ghost.is_none() {
                if let Some(hwnd) = s.guide_hwnd {
                    // SAFETY: guide_hwnd 是本进程引导窗，槽位非 None 即窗口仍在。
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
            }
        }
    }
    // 锁外刷新:refresh_fence 内部要拿 state 锁,持锁重入必死锁
    for fence_id in landed {
        refresh_fence(fence_id);
    }
}

/// 启动初始化
pub fn startup() {
    log(&format!("boot begin ({}ms)", resize_now_ms()));
    // 拖入落位回调注入:ole 保持纯 COM 胶水(零上层依赖),drag 在此注册
    ole::set_fence_drop_cb(crate::drag::on_fence_drop_cb);
    // 崩溃恢复:上次运行隐藏了桌面图标但进程已死 → 先恢复原生图标
    if let Some(pid) = model::load_icons_marker() {
        if pid != std::process::id() {
            let _ = set_desktop_icons_visible(true);
            model::clear_icons_marker();
            log("restored desktop icons from previous dead session");
        }
    }
    // 自启迁移(2026-09-16):老版本只有 Run 键自启,升级后一次性迁到计划
    // 任务(登录即触发,绕过 Run 键排队);迁移完成后此函数零开销
    shell::migrate_autostart_to_task();
    // 启动关键路径并行化:显示名解析(54 文件 ~1.3s)与图标提取
    // (.lnk/exe 单个可达 ~180ms)都是纯 shell 调用,与壁纸捕获/配置加载
    // 无数据依赖。这里先做纯文件系统扫描(~20ms),把 shell 部分全部丢给
    // 两个后台线程,主线程同时跑配置/壁纸暖场,join 后首帧直接全缓存命中。
    let t_scan0 = resize_now_ms();
    let raw_files = shell::scan_desktop_raw();
    let warm_px = model::DpiMetrics::system().icon_px;
    let warm_paths: Vec<String> = raw_files.iter().map(|f| f.path.clone()).collect();
    let t_raw = resize_now_ms() - t_scan0;
    // 冷启动加速(2026-08-27):持久化图标+显示名缓存。命中=零 SHGFI/零图标
    // 提取——此前每次启动全量现提(~57 项,.lnk 冷盘+杀软扫描单文件可达数百
    // ms),是"开机后栅栏比原生桌面晚数秒"的主要可控来源。校验 mtime+px+
    // 定长字节,不合规条目跳过;未命中项照旧后台提取,落盘由 global_tick
    // 检测提取计数变化后安静 4s 自动完成(启动关键路径零 IO)。
    let boot_px = warm_px.round().clamp(16.0, 256.0) as u32;
    let (boot_icons, boot_names) = load_icon_cache_file(&raw_files);
    let n_boot_icons = boot_icons.len();
    // 显示名解析+去重+排序在后台完成后再注入回收站虚拟条目(与旧行为一致:
    // 回收站不受桌面同名文件的去重影响)
    let bg_names = std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let mut files = raw_files;
        shell::finalize_scan_with(&mut files, Some(&boot_names));
        files = with_recycle_bin(files);
        (files, t0.elapsed().as_millis() as u64)
    });
    // 图标提取只做缓存未命中的路径
    let icon_misses: Vec<String> = warm_paths
        .iter()
        .filter(|p| !boot_icons.contains_key(&format!("{p}\0{boot_px}")))
        .cloned()
        .collect();
    let n_icon_misses = icon_misses.len();
    let bg_icons = std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let cache = shell::prewarm_icon_cache(&icon_misses, warm_px);
        (cache, t0.elapsed().as_millis() as u64)
    });
    // 渲染器预热(D2D/GDI 首用路径烧 ~100-170ms)同样移出关键路径:
    // 与壁纸暖场/shell 后台线程并行,show 之前 join。
    let renderer_warm = std::thread::spawn(|| {
        let t0 = resize_now_ms();
        warm_renderer_scratch();
        resize_now_ms() - t0
    });
    // 配置加载不依赖文件列表,先做;空配置(首次运行)的默认布局
    // 生成需要文件列表,推迟到 join 之后
    let config_was_empty = {
        let mut s = state().lock().unwrap();
        let mut empty_config = false;
        if s.fences.is_empty() {
            let t_cfg0 = resize_now_ms();
            let mut loaded = model::load_config();
            // 按持久化的桌面状态恢复(2026-08-27 起):zen/native=全部
            // 栅栏保持隐藏;normal=全部显示(忽略历史遗留的 hidden 位)。
            let persist_hidden = desktop_state() != "normal";
            for f in loaded.iter_mut() {
                f.hidden = persist_hidden;
            }
            if persist_hidden {
                log(&format!("boot restores desktop_state={}", desktop_state()));
            }
            if desktop_state() == "zen" {
                // 纯净态启动:栅栏全程不呈现,图标协调不会去藏图标,
                // 这里显式隐藏(保留接管标记,崩溃后下次启动仍能自动恢复)
                ZEN_MODE.store(true, Ordering::Relaxed);
                if !NATIVE_DESKTOP_OVERRIDE.load(Ordering::Relaxed)
                    && set_desktop_icons_visible(false)
                {
                    DESKTOP_ICONS_HIDDEN.store(true, Ordering::Relaxed);
                    model::save_icons_marker(std::process::id());
                    log("zen boot: native icons hidden, wallpaper only");
                }
            }
            empty_config = loaded.is_empty();
            if !empty_config {
                s.fences = loaded;
            }
            log(&format!(
                "boot load_config took {}ms empty={}",
                resize_now_ms() - t_cfg0,
                empty_config
            ));
        }
        empty_config
    };
    model::load_usage();
    {
        model::set_auto_category(model::load_settings().auto_category); // 预热:开关单一真相
        SHOW_CHROME.store(model::load_settings().show_chrome, Ordering::Relaxed);
        // 界面语言预热:settings.lang + 系统 locale -> 有效语言(菜单/面板查表)
        let st = model::load_settings();
        crate::lang::set_effective(crate::lang::resolve(
            &st.lang,
            crate::lang::system_prefers_zh(),
        ));
    }
    // 首帧壁纸来源(两模式共用,2026-08-26 起透明模式同样需要种子):优先加载
    // 持久化缓存(快,且免去"原生图标可见时现场捕获"的残影/闪烁问题);无缓存
    // (首次运行)才同步暖场捕获(最多重试 ~0.5s,捕获期间临时隐藏原生图标防
    // 残影)。暖场失败不计入稳态回退计数;后续由 200ms 追赶定时器接管直至成功。
    {
        let cached = load_wallpaper_cache();
        let mut tries = 0u32;
        let mut ok = false;
        if let Some(caps) = cached {
            ok = true;
            let mut s = state().lock().unwrap();
            s.wallpapers = caps;
            // wallpaper_ms 保持 0:栅栏接管桌面(图标隐藏)后的第一次例行
            // 重捕获会校验新鲜度,内容变了才重绘+落盘
            log(&format!(
                "boot wallpaper cache loaded ({}ms)",
                resize_now_ms()
            ));
        } else {
            for _ in 0..10 {
                tries += 1;
                ok = {
                    let mut s = state().lock().unwrap();
                    s.wallpaper_ms = 0;
                    ensure_wallpaper(&mut s);
                    !s.wallpapers.is_empty()
                };
                if ok {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        if let Ok(mut s) = state().try_lock() {
            s.wallpaper_fails = 0;
        }
        log(&format!(
            "boot wallpaper warm-up: tries={}, ok={} ({}ms)",
            tries,
            ok,
            resize_now_ms()
        ));
    }
    let t_warm_render = renderer_warm.join().unwrap_or(0);
    // join 后台 shell 预热:合并文件列表与图标缓存,补齐依赖文件列表的
    // 首次运行默认布局
    let (files, t_names) = bg_names.join().unwrap_or_else(|_| (Vec::new(), 0));
    let (icon_prewarm, t_icons) = bg_icons.join().unwrap_or_else(|_| (Default::default(), 0));
    let mut created_cats: Vec<String> = Vec::new();
    {
        let mut s = state().lock().unwrap();
        let n_files = files.len();
        s.files = files;
        // 启动即清理已删文件的常用记录(2026-09-03):应用关闭期间删的文件
        // 同样会在 usage.json 留残账,同名新建继承旧次数顶到常用第一位
        {
            let keep: std::collections::HashSet<String> =
                s.files.iter().map(|f| f.path.clone()).collect();
            let pruned = model::prune_usage(&keep);
            if pruned > 0 {
                log(&format!("pruned {pruned} stale usage entries at boot"));
            }
        }
        // 启动持久化缓存命中先入,后台新提取覆盖同键(构造上 fresh 优先)
        for (k, v) in boot_icons {
            s.icon_cache.entry(k).or_insert(v);
        }
        for (k, v) in icon_prewarm {
            s.icon_cache.insert(k, v);
        }
        if config_was_empty {
            let base = model::build_global_config(&s.files);
            s.fences = base;
            if s.fences.is_empty() {
                let (dw, dh) = default_fence_size();
                s.fences.push(Fence {
                    id: 1,
                    title: "桌面整理".into(),
                    category: String::new(),
                    pinned: Vec::new(),
                    item_order: Vec::new(),
                    rect: Rect {
                        x: 30.0,
                        y: 60.0,
                        w: dw,
                        h: dh,
                    },
                    collapsed: false,
                    scroll_rows: 0,
                    locked: false,
                    hidden: false,
                    manual_size: false,
                    sort_mode: model::default_sort_mode(),
                });
            }
            // 首启即默认布局(2026-09-16):与托盘"恢复默认布局"同一排列
            // (第一行、左对齐、顶对齐、GAP)。此前 build_global_config 的
            // 写死横排超屏后被逐个夹回右缘=用户看到"乱七八糟"
            apply_default_layout(&mut s.fences);
            let _ = n_files;
        } else {
            // 非空配置启动:分类规则可能已变(如 md 文档→代码)而文件集合没变,
            // rescan 不会触发,这里补建缺类栅栏,防止受影响文件无栅栏可归=隐身
            created_cats = ensure_missing_category_fences(&mut s);
            if !created_cats.is_empty() {
                log(&format!(
                    "boot created missing category fences: {created_cats:?}"
                ));
            }
            // 跨会话/跨机器行列保持(2026-09-16):config 存像素,保存时的格距
            // 记在 sidecar;当前格距不同(图标尺寸/机器变了)则按"列数×行数"
            // 等比换算——不然 4 行的栅栏在变小后会漂成 5/6 行
            if let Some((ocw, och)) = load_layout_cells() {
                let (cw, ch) = (model::cell_w(), model::cell_h());
                if (ocw - cw).abs() > 0.5 || (och - ch).abs() > 0.5 {
                    let mut rects: Vec<Rect> = s.fences.iter().map(|f| f.rect).collect();
                    model::rescale_rects_to_cells(&mut rects, ocw, och);
                    for (f, r) in s.fences.iter_mut().zip(rects) {
                        f.rect = r;
                    }
                    log(&format!(
                        "boot rescaled fences {ocw:.0}x{och:.0} -> {cw:.0}x{ch:.0} (rows/cols preserved)"
                    ));
                }
            }
        }
    }
    rebuild_pins();
    log(&format!(
        "boot raw_scan={}ms bg_names={}ms bg_icons={}ms icon_hits={}/misses={} prewarmed={} files={} warmrender={}ms ({}ms)",
        t_raw,
        t_names,
        t_icons,
        n_boot_icons,
        n_icon_misses,
        render::ICON_EXTRACT_COUNT.load(Ordering::Relaxed),
        state().lock().unwrap().files.len(),
        t_warm_render,
        resize_now_ms()
    ));
    settle_all_fences();
    if !created_cats.is_empty() || config_was_empty {
        // 与 rescan 一致:补建/首启后立即持久化(settle 之后的矩形才是
        // 最终位置);首启不落盘的话 config.json 直到用户首次交互才存在
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    refit_auto_fence_heights();
    // 重叠兜底:换机/换图标尺寸夹回后若仍有栅栏相交,按整行收缩到放得下
    resolve_overlaps_by_rows();
    log(&format!("boot pre-show done ({}ms)", resize_now_ms()));
    show_all_fences(); // 桌面壳未就绪时暂缓,由全局定时器自动补挂
    log(&format!("boot first pass done ({}ms)", resize_now_ms()));
    init_tray(); // 托盘 + 全局定时器(挂接/图标/主题/刷新自愈)
    if let Some(&tray) = TRAY_HWND.get() {
        // 壁纸事件驱动:盯住主题缓存目录(幻灯片轮换毫秒级信号);
        // 手动换壁纸由 WM_SETTINGCHANGE 覆盖,例行轮询只做长间隔兜底
        shell::start_wallpaper_watcher(tray, WM_DL3_WALLPAPER_DIRTY);
    }
    install_keyboard_hook();
    install_mouse_hook();
    shell::start_desktop_watcher();
    reconcile_desktop_icons();
    // Complete a second synchronous pass; never block startup with an empty desktop.
    ensure_all_attached();
    refresh_all_fences();
    reconcile_desktop_icons();
    BOOT_VERBOSE.store(false, Ordering::Relaxed);
    log(&format!("boot done ({}ms)", resize_now_ms()));
    // 启动环境体检(默认配置的一部分):把桌面层健康状态留在日志里,
    // 用户报障时日志可直接区分"代码问题/环境问题"(2026-08-29 教训)。
    {
        let (ok, report) = env_health_report();
        if ok {
            log("env-check at boot: ok");
        } else {
            log(&format!("env-check at boot: ISSUES\n{report}"));
        }
    }
    // 行列保持 sidecar 记当前格距(下次启动对比用)
    save_layout_cells();
    // 首启引导(2026-09-11):first_run_done=false 才弹;关闭时勾选"不再提示"
    // 才写 true(2026-09-16 起),否则下次启动仍弹;栅栏已呈现、托盘已就绪后
    // 出现,不再早于桌面接管
    crate::firstrun::maybe_show();
}

/// IDesktopWallpaper 签名的最近值(幻灯片轮换检测)
static WALLPAPER_SIG: Mutex<String> = Mutex::new(String::new());
static WALLPAPER_SIG_WARN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// 定时器兜底：真实左键处于按下状态且光标在编辑框外 → 提交。
/// 光标 merely 悬停在编辑框外不提交（与 Explorer 一致，避免误提交）。
fn finish_rename_if_clicked_outside() {
    let (fence_edit, file_edit) = {
        let s = state().lock().unwrap();
        (s.rename_edit, s.file_rename_edit)
    };
    if fence_edit.is_none() && file_edit.is_none() {
        return;
    }
    let lbutton_down = (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0;
    if !lbutton_down {
        return;
    }
    let mut pt = POINT::default();
    // SAFETY: pt 是栈上输出指针，调用期间有效；失败保持 (0,0)，
    // 由 point_in_window_rect 判为"在编辑框外"前已被真实左键按下条件拦住。
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    if let Some(edit) = fence_edit {
        if !point_in_window_rect(edit, pt.x, pt.y) {
            // SAFETY: edit 是本进程的就地改名编辑框；PostMessage 异步提交，
            // 由编辑框自身的 wndproc 串行处理，无指针参数。
            unsafe {
                let _ = PostMessageW(Some(edit), RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
        }
    }
    if let Some(edit) = file_edit {
        if !point_in_window_rect(edit, pt.x, pt.y) {
            log("COMMIT via timer fallback");
            // SAFETY: 同上：本进程文件改名编辑框的异步提交，无指针参数。
            unsafe {
                let _ = PostMessageW(Some(edit), FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
        }
    }
}
/// 全局自愈:定时器与显示变化时调用。
/// 1) 修复窗口与桌面宿主的挂接(启动竞态/Explorer 重启后自动补挂);
/// 2) 协调原生图标可见性;3) 图标尺寸/主题跟随;4) 桌面文件刷新。
fn global_tick() {
    finish_rename_if_clicked_outside();

    // 兜底:后台扫描结果因 TRAY_HWND 未就绪而没被消息路径取走时,这里补应用
    if SCAN_RESULT.lock().is_ok_and(|r| r.is_some()) {
        apply_pending_scan();
    }

    let t = TICK_COUNT.fetch_add(1, Ordering::Relaxed);
    // 环境自稳:30s 节拍体检,持续异常超宽限期自动重建桌面层(见
    // env_watchdog_tick)。这是"运行期间保证环境正常"的默认机制。
    if t % 30 == 7 {
        env_watchdog_tick();
    }
    // 图标缓存落盘调度:运行期懒提取(DPI 切换/新文件/残影预览等任何
    // icon_cache 增量)都体现在提取计数上;计数变化→记脏,安静 4s 后写盘。
    // 启动冷提取的首次落盘也由此自动完成,无需在启动关键路径上做 IO。
    {
        let seen = ICON_EXTRACT_SEEN.load(Ordering::Relaxed);
        let now = render::ICON_EXTRACT_COUNT.load(Ordering::Relaxed);
        if now != seen {
            ICON_EXTRACT_SEEN.store(now, Ordering::Relaxed);
            ICON_SAVE_DIRTY_MS.store(resize_now_ms(), Ordering::Relaxed);
        }
        let dirty = ICON_SAVE_DIRTY_MS.load(Ordering::Relaxed);
        if dirty != 0 && resize_now_ms().saturating_sub(dirty) > 4000 && t.is_multiple_of(4) {
            ICON_SAVE_DIRTY_MS.store(0, Ordering::Relaxed);
            let px = model::DpiMetrics::system()
                .icon_px
                .round()
                .clamp(16.0, 256.0) as u32;
            save_icon_cache_file_now(px);
        }
    }
    // 注意:不要用 0x052C 消息生成 WorkerW —— 每次调用都会让 Win11 桌面层
    // 在 Progman/WorkerW 之间切换宿主,导致桌面反复重建(栅栏消失、桌面空白)。
    // 只用现有宿主,缺失时等待 Explorer 自然重建,由 ensure_all_attached 自愈。
    ensure_all_attached();
    // z 序暂时失位不代表 ULW 表面丢失。只恢复尚未呈现/缺失表面的
    // 栅栏,不因 attached 的三拍防抖对全组重复提交画面。
    let ids: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences
            .iter()
            .filter(|f| {
                fence_needs_presentation(
                    f.hidden,
                    s.presented.contains(&f.id),
                    s.surfaces.contains_key(&f.id),
                )
            })
            .map(|f| f.id)
            .collect()
    };
    if !ids.is_empty() {
        let mut full = Vec::new();
        {
            let s = state().lock().unwrap();
            for id in &ids {
                if !s.surfaces.contains_key(id) {
                    full.push(*id);
                }
            }
        }
        for id in &ids {
            if !full.contains(id) {
                present_fence_only(*id);
            }
        }
        for id in full {
            refresh_fence(id);
        }
    }
    reconcile_desktop_icons();
    sync_icon_size();
    // 精确模式守护:捕获反复失败(无任何可用快照)时真正切回透明并落盘——
    // 托盘/桌面菜单的"渲染模式"勾选随之落到"透明",用户能清楚看到回退发生了。
    // (动态壁纸强制回退已退役 2026-08-26:统一 seeded 渲染后背景/阴影不再
    // 依赖快照新鲜度,降级没有意义且会静默改写用户配置。)
    if render_mode() == "precise" {
        // 捕获失败回退带双重保险:必须"当前没有任何可用快照"(有旧快照就继续用,
        // 等 10min 兜底/事件重试恢复)且失败计数达阈值且过了 10s 启动宽限——
        // 登录早期/壁纸切换过渡期的瞬态失败绝不能把"精确"误落盘成"透明"
        let capture_failed = state()
            .try_lock()
            .map(|s| {
                capture_fallback_due(!s.wallpapers.is_empty(), s.wallpaper_fails, resize_now_ms())
            })
            .unwrap_or(false);
        if capture_failed {
            set_render_mode("transparent");
            log("precise mode: wallpaper capture failed repeatedly -> fallback to transparent");
        }
    }
    // 壁纸跟随(两模式共用,2026-08-26 起透明模式同样需要快照作文字种子)。
    // 快照到期时重捕获,内容有变(带容差:捕获亮度有 ~4% 时序波动,严格比较
    // 会引发无谓全量重绘=闪)才刷新栅栏。
    if t.is_multiple_of(3) {
        let changed = {
            let mut s = state().lock().unwrap();
            if s.wallpapers.is_empty() {
                s.wallpaper_ms = 0; // 强制重捕获
            }
            ensure_wallpaper(&mut s)
        };
        if changed {
            refresh_all_fences();
        }
    }
    // 壁纸事件源之三:IDesktopWallpaper 签名(每显示器当前壁纸路径+背景色+
    // 填充模式)。幻灯片轮换时注册表与 TranscodedWallpaper 缓存都可能不更新
    // (实测部分定制系统换壁纸不写该目录),但 GetWallpaper 反映当前帧。1s 一次纯
    // COM 字符串比较,开销可忽略;变化即触发"捕获-比对-重绘"跟随。
    // (两模式共用:透明模式也要种子)
    {
        match shell::wallpaper_signature() {
            Some(sig) => {
                let mut last = WALLPAPER_SIG.lock().unwrap();
                if last.is_empty() {
                    log("wallpaper signature baseline ready");
                    *last = sig;
                } else if *last != sig {
                    log("wallpaper signature changed -> follow armed");
                    *last = sig;
                    drop(last);
                    if let Ok(mut s) = state().try_lock() {
                        s.wallpaper_ms = 0;
                    }
                    arm_wallpaper_follow();
                }
            }
            None => {
                if !WALLPAPER_SIG_WARN.swap(true, Ordering::Relaxed) {
                    log("wallpaper signature unavailable (IDesktopWallpaper failed)");
                }
            }
        }
    }
    if t.is_multiple_of(10) {
        let changed = {
            let mut s = state().lock().unwrap();
            match s.renderer.as_mut() {
                Some(r) => {
                    let old = (r.light, r.accent);
                    r.refresh_theme();
                    (r.light, r.accent) != old
                }
                None => false,
            }
        };
        if changed {
            refresh_all_fences();
        }
    }
    if shell::take_desktop_dirty() || t.is_multiple_of(30) {
        rescan();
    }
}

// ---------------- 托盘图标 ----------------

/// User explicitly requested native desktop icons to remain visible.
static NATIVE_DESKTOP_OVERRIDE: AtomicBool = AtomicBool::new(false);

/// 图标缓存落盘调度状态(见 global_tick 内说明)
static ICON_EXTRACT_SEEN: AtomicU64 = AtomicU64::new(0);
static ICON_SAVE_DIRTY_MS: AtomicU64 = AtomicU64::new(0);

fn set_desktop_icons_visible(visible: bool) -> bool {
    let Some(lv) = desktop_listview() else {
        log("desktop listview not found");
        return false;
    };
    // SAFETY: lv 是桌面图标列表视图（Explorer 的 SHELLDLL_DefView 子窗口，
    // desktop_listview 现查现用，句柄在调用期间有效）；ShowWindow 只切
    // 可见位，不触碰该窗口的其他资源。
    unsafe {
        let _ = ShowWindow(lv, if visible { SW_SHOW } else { SW_HIDE });
    }
    true
}

/// 无 UI 的灾难恢复入口。只恢复 Explorer 的原生图标显示状态，绝不触碰
/// 图标坐标、排序、桌面文件或 DeskFence 的布局配置。
pub fn restore_desktop_now() -> bool {
    let ok = set_desktop_icons_visible(true);
    DESKTOP_ICONS_HIDDEN.store(false, Ordering::Relaxed);
    model::clear_icons_marker();
    if ok {
        log("native desktop restored by recovery command");
    }
    ok
}

/// 栅栏必须完成一次真实呈现、窗口有效且所属桌面宿主仍然可见，才可以临时
/// 隐藏原生图标。IsWindowVisible 单独成立不代表用户真的能看到栅栏。
fn any_fence_presented_on_desktop() -> bool {
    let hosts = desktop_hosts();
    let s = state().lock().unwrap();
    let now = resize_now_ms();
    s.fences.iter().any(|f| {
        !f.hidden
            && s.presented.contains(&f.id)
            && s.surfaces.contains_key(&f.id)
            && s.windows
                .get(&f.id)
                .is_some_and(|h| unsafe { IsWindowVisible(*h).as_bool() })
            && host_for_rect(&f.rect, &hosts).is_some()
            // 健康宽限:attached 每 tick 清空重建,z 防抖期(≤3 拍)栅栏会
            // 短暂缺席该集合,但用户眼里它一直好好地在桌面上。只要 8s 内
            // 曾完整就绪(z 在带+呈现+可见+宿主在位),就不算"失去呈现",
            // 绝不因此走保底恢复把原生图标放出来。
            && (s.attached.contains(&f.id)
                || now.saturating_sub(
                    *s.last_healthy_ms.get(&f.id).unwrap_or(&0),
                ) < 8000)
    })
}

/// 协调原生桌面图标可见性。任何栅栏宿主/呈现状态异常都优先恢复原生图标，
/// 以保证用户绝不会得到空白桌面。
pub(crate) fn reconcile_desktop_icons() {
    if NATIVE_DESKTOP_OVERRIDE.load(Ordering::Relaxed) {
        let _ = set_desktop_icons_visible(true);
        DESKTOP_ICONS_HIDDEN.store(false, Ordering::Relaxed);
        model::clear_icons_marker();
        return;
    }
    let fences_ready = any_fence_presented_on_desktop();
    if fences_ready {
        // 接管不变式(2026-08-29):判定依据是图标**实际可见性**而非标志位
        // ——Explorer 在 ToggleDesktop/自身重建后可能重新显示图标列表,
        // 只看 DESKTOP_ICONS_HIDDEN 会死锁(标志 true 但图标可见,永不
        // 重新隐藏)。栅栏在桌面=图标必须藏,这就是"不被环境干扰"。
        let lv_vis = desktop_listview().is_some_and(|lv| unsafe { IsWindowVisible(lv).as_bool() });
        if (!DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) || lv_vis)
            && set_desktop_icons_visible(false)
        {
            DESKTOP_ICONS_HIDDEN.store(true, Ordering::Relaxed);
            model::save_icons_marker(std::process::id());
            log("desktop icons hidden after fence presentation verified");
        }
    } else if ZEN_MODE.load(Ordering::Relaxed) {
        // 纯净态:主动维持图标隐藏。不依赖 DESKTOP_ICONS_HIDDEN 前提——
        // TaskbarCreated 路径的 restore_desktop_now 会把标志清成 false,
        // 若以标志为前提,清掉后 zen 的图标维持整条失效=纯净态破功
        // (2026-08-31)。每秒只做"读可见性"的检查,失配才重新隐藏,
        // 平时零骚扰。崩溃安全不受影响:接管标记仍在。
        let re_showing =
            desktop_listview().is_some_and(|lv| unsafe { IsWindowVisible(lv).as_bool() });
        if re_showing
            && !NATIVE_DESKTOP_OVERRIDE.load(Ordering::Relaxed)
            && set_desktop_icons_visible(false)
        {
            DESKTOP_ICONS_HIDDEN.store(true, Ordering::Relaxed);
            model::save_icons_marker(std::process::id());
            log("zen: re-hid native icons (Explorer re-showed them)");
        }
    } else if DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) {
        // 保底恢复前的诊断快照:谁是"没就绪"的栅栏(可见性/宿主缺哪个),
        // 防止误判断(如菜单收尾瞬态)误触发整桌回退。2026-08-27 排查
        // "菜单后点空白→原生闪现 1-2s"专用。
        {
            let s = state().lock().unwrap();
            let mut why: Vec<String> = Vec::new();
            for f in s.fences.iter().filter(|f| !f.hidden) {
                let vis = s
                    .windows
                    .get(&f.id)
                    .is_some_and(|h| unsafe { IsWindowVisible(*h).as_bool() });
                if !(vis && s.presented.contains(&f.id)) {
                    why.push(format!(
                        "{}:vis={}pres={}",
                        f.id,
                        vis,
                        s.presented.contains(&f.id)
                    ));
                }
            }
            log(&format!(
                "icon-restore guard trip: {} unready [{}]",
                why.len(),
                why.join(",")
            ));
        }
        let _ = restore_desktop_now();
        log("desktop icons restored because fence presentation is unavailable");
    }
}

pub(crate) fn toggle_desktop_icons() {
    let want_hidden = !DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed);
    if want_hidden && !any_fence_presented_on_desktop() {
        log("refused to hide native desktop: no verified fence presentation");
        return;
    }
    if set_desktop_icons_visible(!want_hidden) {
        NATIVE_DESKTOP_OVERRIDE.store(!want_hidden, Ordering::Relaxed);
        DESKTOP_ICONS_HIDDEN.store(want_hidden, Ordering::Relaxed);
        if want_hidden {
            model::save_icons_marker(std::process::id());
        } else {
            model::clear_icons_marker();
        }
        log(&format!("desktop icons hidden={}", want_hidden));
    }
}

pub(crate) fn restore_desktop_icons() {
    if DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) {
        set_desktop_icons_visible(true);
        DESKTOP_ICONS_HIDDEN.store(false, Ordering::Relaxed);
    }
    model::clear_icons_marker();
}

/// # Safety
/// 引导窗（拖拽残影/入场动画 overlay）的窗口过程，register_class 注册、
/// 系统在 UI 线程同步回调；本实现不解引用消息参数，仅对销毁消息清理
/// state 槽位，其余交 DefWindowProcW 透传（raw 参数按窗口过程契约有效）。
unsafe extern "system" fn guide_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DESTROY || msg == WM_NCDESTROY {
        if let Ok(mut s) = state().try_lock() {
            if s.guide_hwnd == Some(hwnd) {
                s.guide_hwnd = None;
                if let Some(surface) = s.guide_surface.take() {
                    render::release_surface(surface);
                }
            }
        }
    }
    // SAFETY: 参数原样透传给默认过程，按 wndproc 契约有效；hwnd 是本类窗口。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// # Safety
/// 托盘窗口的窗口过程（register_class 注册，系统在 UI 线程同步回调）。
/// hwnd 是本进程托盘窗口；体内 unsafe 操作只使用栈上参数（GetCursorPos
/// 的栈 POINT、DefWindowProcW 透传 raw 消息参数）与本进程窗口句柄，
/// 无跨调用指针。
unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY(整块): 各分支按消息契约使用参数——TRAY_MSG 的坐标现取
    // (栈 pt)、定时器 id 来自本进程 SetTimer、WM_CLOSE 走自家 quit_app；
    // 未识别消息交 DefWindowProcW 透传。
    unsafe {
        if msg == taskbar_created_msg() {
            // Explorer rebuilt its taskbar and desktop host. Keep the native desktop
            // visible while the fences are reattached, then re-add our tray icon.
            // zen 态例外:纯净态不该放图标——放了会和 reconcile 的 zen 维持
            // 对抗(2026-08-31 实测:watchdog 重启 Explorer 后图标闪现又被
            // 藏回,反复拉锯)。zen 的图标隐藏由 reconcile 每秒维持兜底。
            if desktop_state() != "zen" {
                let _ = restore_desktop_now();
            }
            add_tray_icon(hwnd);
            show_all_fences();
            return LRESULT(0);
        }
        if msg == TRAY_MSG {
            match (lparam.0 as u32) & 0xFFFF {
                WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU => {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    show_tray_menu(pt.x, pt.y);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        if msg == WM_DL3_SHOW_ALL {
            // 第二实例唤起 = 回到正常态(持久化)
            ZEN_MODE.store(false, Ordering::Relaxed);
            set_desktop_state_stored("normal");
            show_all_fences();
            return LRESULT(0);
        }
        if msg == WM_DL3_NUDGE {
            nudge_fence(wparam.0 as u32, lparam.0 as u32);
            return LRESULT(0);
        }
        if msg == WM_DL3_KEY {
            dispatch_file_key(wparam.0 as u32, lparam.0 as usize);
            return LRESULT(0);
        }
        if msg == WM_DL3_CLEAR_SEL {
            handle_clear_selection_click(wparam.0 as i32, lparam.0 as i32);
            return LRESULT(0);
        }
        if msg == WM_DL3_WALLPAPER_DIRTY {
            // 目录事件到达:250ms 防抖(合并写文件风暴,等 DWM 完成切换),
            // 由 TIMER_WALLPAPER_FOLLOW 做"捕获-比对-变了才重绘"
            log("wallpaper dir event -> follow armed");
            arm_wallpaper_follow();
            return LRESULT(0);
        }
        if msg == WM_DL3_ZCHECK {
            // 合并后的高速自检:栅栏在宿主之下(显示桌面批次)立即重挂
            ZCHECK_PENDING.store(false, Ordering::Relaxed);
            zcheck_fences_now();
            return LRESULT(0);
        }
        if msg == WM_DL3_SCAN_APPLY {
            // 后台扫描完成:UI 线程应用结果(扫描线程只产数据不碰窗口)
            apply_pending_scan();
            return LRESULT(0);
        }
        if msg == crate::winids::WM_DL3_RESCAN {
            // shell 动词(删除/移动等)在应用背后改了桌面:经消息异步请求重扫,
            // shell 模块因此不反向依赖 ui(2026-09-17 断上行边)
            rescan();
            return LRESULT(0);
        }
        if msg == WM_SETTINGCHANGE {
            rebuild_render_resources();
            invalidate_hosts_cache();
            // 壁纸可能变化:作废旧快照,重捕获(精确模式随之更新底图)
            invalidate_wallpaper();
            show_all_fences();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_GLOBAL {
            global_tick();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_DESKTOP_WATCH {
            // 桌面态快速自检:三指手势的窗口扫动不发任何 WinEvent(两轮
            // 实测零触发),恢复过渡只能靠 250ms 轮询兜住;band_quiet 由
            // 1s 走查维护,正常使用时这里什么都不做。
            if state().lock().unwrap().band_quiet {
                zcheck_fences_now();
            }
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_ANIMATION {
            tick_arrival_animations();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_WALLPAPER_CATCHUP {
            wallpaper_catchup_tick(hwnd);
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_WALLPAPER_FOLLOW {
            wallpaper_follow_tick(hwnd);
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_RENAME_WATCH {
            finish_rename_if_clicked_outside();
            let any_edit = state()
                .lock()
                .map(|s| s.rename_edit.is_some() || s.file_rename_edit.is_some())
                .unwrap_or(false);
            if !any_edit {
                let _ = KillTimer(Some(hwnd), TIMER_RENAME_WATCH);
            }
            return LRESULT(0);
        }
        if msg == WM_CLOSE {
            // 允许外部(脚本/任务管理器"关闭窗口")请求干净退出:恢复桌面图标再退出
            quit_app();
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn add_tray_icon(hwnd: HWND) {
    // SAFETY: n 是 zeroed+手工填段的栈结构，cbSize 按契约填实际大小；
    // szTip 为 zeroed 定长 128 数组,循环 take(127) 有界复制——终止由
    // zeroed 的末项兜底保证(即使未来 tooltip 文本更长,截断后仍必 NUL
    // 结尾);hWnd 是本进程托盘窗口,hIcon 是 deskfence_icon() 的进程
    // 终身图标句柄。
    unsafe {
        let mut n: NOTIFYICONDATAW = std::mem::zeroed();
        n.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = hwnd;
        n.uID = 1;
        n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        n.uCallbackMessage = TRAY_MSG;
        n.hIcon = deskfence_icon();
        let tip = shell::wide("DeskFence");
        for (i, ch) in tip.iter().take(127).enumerate() {
            n.szTip[i] = *ch;
        }
        if !Shell_NotifyIconW(NIM_ADD, &n).as_bool() {
            log("tray add failed");
        }
    }
}

fn init_tray() {
    // SAFETY(整块): 类名是 winids 的静态 NUL 宽串，hinstance 是本进程模块
    // 实例；两个辅助窗 lpParam=None、创建失败返回 null（随后判空降级为
    // 无托盘，不触空句柄）；ShowWindow/SetTimer 作用于刚创建的本进程窗口，
    // 定时器回调 None=WM_TIMER 进 tray_wndproc（同线程分发）。
    unsafe {
        // 托盘宿主窗口:1x1、点击穿透的工具窗口。
        // 关键约束 1:必须"可见"才能被 SetForegroundWindow 前台化(隐藏窗口
        // 静默失败→僵尸菜单),1 像素 + WS_EX_TRANSPARENT 视觉与命中都无感。
        // 关键约束 2:绝不能 WS_EX_LAYERED——TrackPopupMenu 的开合动画需要
        // owner 的屏幕内容做背景,分层 owner 会让动画损坏成"两段跳变",
        // 表现为菜单打开/关闭时闪(这也是历史上"点桌面关菜单闪屏"的根源)。
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            tray_class_name(),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            GetSystemMetrics(SM_CXSCREEN) - 2,
            GetSystemMetrics(SM_CYSCREEN) - 2,
            1,
            1,
            None,
            None,
            Some(hinstance()),
            None,
        )
        .unwrap_or_default();
        if hwnd.0.is_null() {
            log("tray window create failed");
            return;
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = TRAY_HWND.set(hwnd);
        // 菜单前台宿主:所有弹出菜单(栅栏操作菜单/文件右键菜单)的前台化
        // 目标。绝不能用栅栏窗口(前台化会提升其 z-band 引发拉回闪屏),
        // 也绝不能 WS_EX_LAYERED(分层 owner 损坏菜单开合动画→两段跳变
        // 式闪屏)。1x1 + 点击穿透;位置放在屏幕右下角最后一像素——
        // 不能压在栅栏区域上,1px 前台窗口失活时 DWM 的处理会波及其
        // 正下方的分层窗口(实测在 (0,0) 时引发栅栏整面 ~4% 亮度跳变)。
        let menu_host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            menu_host_class_name(),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            GetSystemMetrics(SM_CXSCREEN) - 2,
            GetSystemMetrics(SM_CYSCREEN) - 2,
            1,
            1,
            None,
            None,
            Some(hinstance()),
            None,
        )
        .unwrap_or_default();
        if !menu_host.0.is_null() {
            let _ = ShowWindow(menu_host, SW_SHOWNOACTIVATE);
            let _ = MENU_HOST_HWND.set(menu_host);
        } else {
            // 菜单前台化宿主建不出来:菜单仍能用(menu_host_or 回退栅栏窗口
            // 做 owner),但前台化降级,僵尸菜单风险上升——值得留一行现场。
            log("menu host window create failed; menus fall back to fence-owner foregrounding");
        }
        // 全局低频自愈定时器：窗口挂接/图标协调/主题跟随/文件刷新。
        // 目录变化由 watcher 置位，避免在拖动期间以 100ms 频率扫描和重挂窗口。
        // 失败=整个秒级心跳停摆(自愈/协调全静默死亡),必须记日志。
        if SetTimer(Some(hwnd), TIMER_GLOBAL, 1000, None) == 0 {
            log("global tick timer create failed");
        }
        // 桌面态快速自检:仅当走查判定 band_quiet(桌面态)时才做实事,
        // 正常使用(带内有可见外来窗)空转,零成本。失败=显示桌面恢复降级
        // 为秒级走查,同样静默,记日志。
        if SetTimer(Some(hwnd), TIMER_DESKTOP_WATCH, 250, None) == 0 {
            log("desktop watch timer create failed");
        }
        // 全局 z 序事件钩子:显示桌面等批量重排的毫秒级触发器(详见
        // zorder_event_cb 注释),高速自检走 WM_DL3_ZCHECK 合并投递。
        install_zorder_hooks();
        add_tray_icon(hwnd);
    }
}

pub fn run_message_loop() -> i32 {
    loop {
        let mut msg = MSG::default();
        // SAFETY: msg 是栈上消息结构，GetMessageW 阻塞等待本线程队列
        // （主线程=创建全部窗口的线程）；panel_message 只读 msg 快照；
        // Translate/Dispatch 把同一栈副本交给系统，调用期间有效。
        unsafe {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                break;
            }
            if r.0 == -1 {
                log("getmessage error");
                break;
            }
            // 分类面板的键盘拦截(Esc=关闭,Enter=提交),命中则跳过默认分发
            if crate::cats_panel::panel_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

// ---------------- 键盘微调 / 键盘钩子 ----------------

/// 方向键微调栅栏位置(光标悬停在栅栏上时生效;Ctrl = 1px 微调,否则按图标网格步进)
static LL_HOOK: SyncHandle<OnceLock<HHOOK>> = SyncHandle(OnceLock::new());

fn fence_id_for_hwnd(hwnd: HWND) -> Option<u32> {
    let s = state().try_lock().ok()?;
    s.windows.iter().find(|(_, h)| **h == hwnd).map(|(k, _)| *k)
}

/// # Safety
/// WH_KEYBOARD_LL 低级键盘钩子回调，系统在**安装钩子的线程**（主线程）
/// 同步调用。ncode==HC_ACTION 时 lparam 指向系统所有的
/// KBDLLHOOKSTRUCT，回调期间可读；实现只读它、经 PostMessageW 把业务
/// 转交托盘窗口，不持有指针快速返回（低级钩子超时会被系统摘除）。
unsafe extern "system" fn ll_keyboard_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY(整块): lparam 解引用依据钩子契约（见 fn 的 Safety 段）；
    // 其余调用无指针参数；CallNextHookEx 原样传参保持钩子链。
    unsafe {
        if ncode as u32 == HC_ACTION {
            let down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
            if down {
                let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let vk = kb.vkCode;
                let is_arrow = vk == VK_LEFT.0 as u32
                    || vk == VK_RIGHT.0 as u32
                    || vk == VK_UP.0 as u32
                    || vk == VK_DOWN.0 as u32;
                let is_command = is_arrow
                    || vk == VK_RETURN.0 as u32
                    || vk == 0x71
                    || vk == 0x2E
                    || vk == VK_ESCAPE.0 as u32
                    || (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
                        && matches!(vk, 0x41 | 0x43 | 0x58 | 0x56);
                if is_command {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let under = WindowFromPoint(pt);
                    if !under.0.is_null() {
                        if let Some(id) = fence_id_for_hwnd(under) {
                            if let Some(&th) = TRAY_HWND.get() {
                                let has_selection = state()
                                    .try_lock()
                                    .map(|s| !s.selected_paths.is_empty())
                                    .unwrap_or(false);
                                // 仅无选择时保留旧的方向键移动栅栏行为；选中图标后方向键导航。
                                let _ = PostMessageW(
                                    Some(th),
                                    if is_arrow
                                        && !has_selection
                                        && (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000)
                                            == 0
                                    {
                                        WM_DL3_NUDGE
                                    } else {
                                        WM_DL3_KEY
                                    },
                                    WPARAM(id as usize),
                                    LPARAM(
                                        (vk as usize
                                            | if (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16
                                                & 0x8000)
                                                != 0
                                            {
                                                1 << 16
                                            } else {
                                                0
                                            }
                                            | if (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16
                                                & 0x8000)
                                                != 0
                                            {
                                                1 << 24
                                            } else {
                                                0
                                            }) as isize,
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, ncode, wparam, lparam)
    }
}

fn install_keyboard_hook() {
    // SAFETY: ll_keyboard_proc 是匹配 HOOKPROC ABI 的钩子函数；
    // hinstance 是本进程模块（低级钩子要求回调在本进程内）；thread id 0
    // =全局钩子（低级钩子在安装线程回调，即主线程的消息循环里）；
    // 句柄存 OnceLock 单次安装，退出由 uninstall_keyboard_hook 摘除。
    unsafe {
        if LL_HOOK.get().is_none() {
            if let Ok(h) =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), Some(hinstance()), 0)
            {
                if !h.0.is_null() {
                    let _ = LL_HOOK.set(h);
                }
            }
        }
    }
}

pub(crate) fn uninstall_keyboard_hook() {
    if let Some(h) = LL_HOOK.get() {
        // SAFETY: h 是 SetWindowsHookExW 返回、存于 OnceLock 的合法句柄，
        // 只在退出路径摘除一次。
        unsafe {
            let _ = UnhookWindowsHookEx(*h);
        }
    }
}

// ---------------- 全局鼠标钩子(桌面空白点击清除选择态) ----------------

static LL_MOUSE_HOOK: SyncHandle<OnceLock<HHOOK>> = SyncHandle(OnceLock::new());

/// 栅栏窗口是 WS_EX_NOACTIVATE 的独立 HWND,点击桌面空白时事件直接进入
/// Explorer 的 WorkerW/Progman,本程序收不到任何消息,于是被选中的图标
/// 高亮会一直残留。用 WH_MOUSE_LL 监听左键按下:落点不在任何栅栏内且命中
/// 桌面宿主窗口时,通知托盘窗口清除选择(与原生 Explorer 行为一致)。
///
/// # Safety
/// WH_MOUSE_LL 低级鼠标钩子回调，系统在安装钩子的线程（主线程）同步
/// 调用。ncode==HC_ACTION 时 lparam 指向系统所有的 MSLLHOOKSTRUCT，
/// 回调期间可读；实现只读坐标并 PostMessage 给托盘窗口，快速返回。
unsafe extern "system" fn ll_mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY(整块): lparam 解引用依据钩子契约（见 fn 的 Safety 段）；
    // PostMessage 无指针参数；CallNextHookEx 原样传参保持钩子链。
    unsafe {
        if ncode as u32 == HC_ACTION && wparam.0 as u32 == WM_LBUTTONDOWN {
            crate::ui::mark_interaction();
            let mm = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if let Some(&tray) = TRAY_HWND.get() {
                let _ = PostMessageW(
                    Some(tray),
                    WM_DL3_CLEAR_SEL,
                    WPARAM(mm.pt.x as usize),
                    LPARAM(mm.pt.y as isize),
                );
            }
        }
        CallNextHookEx(None, ncode, wparam, lparam)
    }
}

fn install_mouse_hook() {
    // SAFETY: ll_mouse_proc 是匹配 HOOKPROC ABI 的钩子函数；低级鼠标钩子
    // 在安装线程回调（主线程）；句柄存 OnceLock 单次安装，退出由
    // uninstall_mouse_hook 摘除。
    unsafe {
        if LL_MOUSE_HOOK.get().is_none() {
            if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), Some(hinstance()), 0)
            {
                if !h.0.is_null() {
                    let _ = LL_MOUSE_HOOK.set(h);
                }
            }
        }
    }
}

pub(crate) fn uninstall_mouse_hook() {
    if let Some(h) = LL_MOUSE_HOOK.get() {
        // SAFETY: h 是 SetWindowsHookExW 返回、存于 OnceLock 的合法句柄，
        // 只在退出路径摘除一次。
        unsafe {
            let _ = UnhookWindowsHookEx(*h);
        }
    }
}

/// 点在任一可见栅栏矩形内?
fn point_in_any_fence(s: &UiState, x: i32, y: i32) -> bool {
    let (px, py) = (x as f32, y as f32);
    s.fences.iter().any(|f| {
        !f.hidden && {
            let r = &f.rect;
            px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h
        }
    })
}

/// 落点处是否为桌面宿主(WorkerW/Progman/桌面图标视图)
fn point_on_desktop_host(x: i32, y: i32) -> bool {
    // SAFETY: pt 是栈坐标；GetClassNameW 的 buf 是 64-u16 栈缓冲，API
    // 至多写 buf.len() 项并返回实写长度（n 已 max(0) 防负数切片）；
    // GetAncestor 只查询 z 链上的既有窗口，空根已判空回退原 hwnd。
    unsafe {
        let pt = POINT { x, y };
        let mut hwnd = WindowFromPoint(pt);
        if hwnd.0.is_null() {
            return false;
        }
        // 命中的可能是桌面的 SysListView32 子窗口,取根窗口再判类名
        let root = GetAncestor(hwnd, GA_ROOT);
        if !root.0.is_null() {
            hwnd = root;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        let name = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        matches!(name.as_str(), "Progman" | "WorkerW" | "SysListView32")
    }
}

/// 栅栏外左键点击(桌面空白) → 清除全部选择/焦点/悬停视觉,刷新受影响栅栏
fn handle_clear_selection_click(x: i32, y: i32) {
    let (had_selection, refresh_ids) = {
        let s = match state().try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if point_in_any_fence(&s, x, y) || !point_on_desktop_host(x, y) {
            return;
        }
        let had = !s.selected_paths.is_empty()
            || s.focused_path.is_some()
            || s.marquee.is_some()
            || s.hover.values().any(|v| v.is_some());
        if !had {
            return;
        }
        // 只刷新视觉会变的栅栏:有悬停残留的 + 有选中内容的;其余不动,
        // 避免每次点桌面空白/托盘都整面重绘造成可见闪烁
        let mut ids: Vec<u32> = Vec::new();
        for (id, v) in s.hover.iter() {
            if v.is_some() {
                ids.push(*id);
            }
        }
        if !s.selected_paths.is_empty() || s.focused_path.is_some() || s.marquee.is_some() {
            for f in s.fences.iter().filter(|f| !f.hidden) {
                if !ids.contains(&f.id) {
                    ids.push(f.id);
                }
            }
        }
        (had, ids)
    };
    if !had_selection {
        return;
    }
    {
        let mut s = state().lock().unwrap();
        s.selected_paths.clear();
        s.focused_path = None;
        s.selection_anchor = None;
        s.marquee = None;
        let fence_ids: Vec<u32> = s.fences.iter().map(|f| f.id).collect();
        for fence_id in fence_ids {
            s.hover.insert(fence_id, None);
            s.hover_pending.remove(&fence_id);
        }
    }
    for id in refresh_ids {
        log(&format!("clear-sel refresh fence {id}"));
        refresh_fence(id);
    }
}

/// 桌面宿主是否就绪(清洁启动门槛用):Progman/WorkerW + 图标视图链存在。
pub fn desktop_host_ready() -> bool {
    desktop_shell_window().is_some()
}

// ---------------- 菜单与操作 ----------------

pub(crate) fn restore_original_desktop() {
    ZEN_MODE.store(false, Ordering::Relaxed);
    set_desktop_state_stored("native");
    let _ = restore_desktop_now();
    set_all_hidden(true);
    log("returned to original desktop without changing files or icon layout");
}

pub(crate) fn set_all_hidden(hidden: bool) {
    let ids: Vec<u32> = {
        let mut s = state().lock().unwrap();
        if hidden {
            clear_all_interaction(&mut s);
        }
        for f in s.fences.iter_mut() {
            f.hidden = hidden;
        }
        // 隐藏状态只在本次运行生效,不持久化:启动永远显示全部栅栏
        s.fences.iter().map(|f| f.id).collect()
    };
    if hidden {
        finish_interaction_cleanup();
    }
    for id in ids {
        ensure_fence_window(id);
        refresh_fence(id);
    }
    if hidden {
        reconcile_desktop_icons();
    }
}

// ---------------- WndProc ----------------

/// # Safety
/// 栅栏窗口与菜单宿主窗口（复用本过程）的窗口过程，register_class 注册、
/// 系统在 UI 线程同步回调。hwnd 是本进程创建的对应类窗口；GWLP_USERDATA
/// 存 fence_id（0=无 id 窗口，如菜单宿主，走默认路径）。函数体内对
/// lparam 的裸解引用按各消息契约成立：WM_WINDOWPOSCHANGING/CHANGED 的
/// lparam 指向系统所有的 WINDOWPOS（窗口过程调用帧内可读写，改 flags 正是
/// 该消息的预期用法）；WM_DPICHANGED 的 lparam 指向建议 RECT（可读）。
unsafe extern "system" fn fence_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let fence_id = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u32;
    if matches!(
        msg,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) {
        if let Some(result) = shell::forward_menu_message(msg, wparam, lparam) {
            return result;
        }
    }
    match msg {
        WM_MOUSEMOVE => {
            let x = (lparam.0 as u32 & 0xFFFF) as f32;
            let y = ((lparam.0 as u32 >> 16) & 0xFFFF) as f32;
            // 丢弃陈旧的排队移动:菜单/对话框的模态循环会推迟本窗口的
            // 消息处理,循环结束后补投递的 move 坐标是模态开始前的旧位置
            // (光标早已移走)。放行会"无中生有"点亮悬停高亮再熄灭——
            // 两次无意义整面重绘,表现为点桌面关闭菜单时栅栏闪一下。
            // 消息坐标与真实光标偏差超过阈值即视为陈旧。
            // 拖动中豁免(2026-09-01):跟随位置一律取 GetCursorPos 实时值,
            // 陈旧消息无副作用;而拖动重渲染积压时丢消息正是"拖动一卡一卡
            // 不跟手"的来源——积压消息被整批丢弃,只剩零星更新。
            let dragging = state()
                .try_lock()
                .map(|g| g.drag.is_some())
                .unwrap_or(false);
            let stale = !dragging && {
                let mut real = POINT::default();
                // SAFETY: real 是栈上输出指针，GetCursorPos/ScreenToClient
                // 均在调用期间完成读写；hwnd 是本窗口。
                unsafe {
                    let _ = GetCursorPos(&mut real);
                    let _ = ScreenToClient(hwnd, &mut real);
                }
                (real.x as f32 - x).abs() > 6.0 || (real.y as f32 - y).abs() > 6.0
            };
            if stale {
                return LRESULT(0);
            }
            track_mouse_leave(hwnd);
            handle_mousemove(hwnd, fence_id, x, y);
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let was = {
                let mut s = match state().try_lock() {
                    Ok(g) => g,
                    Err(_) => return LRESULT(0),
                };
                // 离开窗口必须同时清掉图标 hover,否则最后停过的图标会永久高亮(像被选中)
                s.hover.insert(fence_id, None);
                s.hover_pending.remove(&fence_id);
                s.fence_hover_pending.remove(&fence_id);
                s.fence_hover.insert(fence_id, false).unwrap_or(false)
            };
            // SAFETY: hwnd 是本窗口；纯定时器调用，无指针参数。
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            }
            if was {
                refresh_fence(fence_id);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 as u32 & 0xFFFF) as f32;
            let y = ((lparam.0 as u32 >> 16) & 0xFFFF) as f32;
            handle_lbuttondown(hwnd, fence_id, x, y);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 as u32 & 0xFFFF) as f32;
            let y = ((lparam.0 as u32 >> 16) & 0xFFFF) as f32;
            handle_lbuttonup(hwnd, fence_id, x, y);
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            let x = (lparam.0 as u32 & 0xFFFF) as f32;
            let y = ((lparam.0 as u32 >> 16) & 0xFFFF) as f32;
            handle_dblclk(fence_id, x, y);
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            let x = (lparam.0 as u32 & 0xFFFF) as f32;
            let y = ((lparam.0 as u32 >> 16) & 0xFFFF) as f32;
            handle_rbuttonup(hwnd, fence_id, x, y);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
            handle_wheel(fence_id, delta);
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            handle_setcursor(hwnd, fence_id);
            return LRESULT(1);
        }
        WM_TIMER => {
            if wparam.0 == TIMER_HOVER {
                // 悬停延迟到期：提交 pending 悬停并重绘
                // SAFETY: hwnd 是本窗口；纯定时器调用，无指针参数。
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_HOVER);
                }
                let mut s = match state().try_lock() {
                    Ok(g) => g,
                    Err(_) => return LRESULT(0),
                };
                // 注意:此处已持有 state 锁,不能调 fence_id_for_hwnd(内部会 try_lock 同一把锁,
                // 永远失败导致悬停提交不执行)。wndproc 参数里就有 fence_id,直接用。
                let mut need_refresh = false;
                if let Some(pending) = s.hover_pending.remove(&fence_id) {
                    let prev = *s.hover.get(&fence_id).unwrap_or(&None);
                    if prev != pending {
                        s.hover.insert(fence_id, pending);
                        need_refresh = true;
                    }
                }
                // 卡片延迟提交:鼠标确实停留满悬停时间,卡片才浮现
                if s.fence_hover_pending.remove(&fence_id) == Some(true)
                    && !s.fence_hover.get(&fence_id).copied().unwrap_or(false)
                {
                    s.fence_hover.insert(fence_id, true);
                    need_refresh = true;
                }
                drop(s);
                // 操作菜单只在点击倒三角时出现(WM_LBUTTONUP 的 Hit::Collapse);
                // 悬停不再自动弹出,避免误触发和浮在其他软件上层
                if need_refresh {
                    refresh_fence(fence_id);
                }
            }
            // 栅栏窗口不再持有刷新定时器(由托盘窗口的全局定时器统一调度)
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            // Keep the fence's logical grid size stable. Windows supplies a
            // suggested position for the target monitor, while width/height are
            // reconstructed from the previous row/column count using that
            // fence's new metrics rather than changing every fence globally.
            let suggested = if lparam.0 == 0 {
                None
            } else {
                // SAFETY: WM_DPICHANGED 契约：lparam 非空时指向建议 RECT，
                // 窗口过程调用帧内可读；仅读四个坐标标量。
                let r = unsafe { *(lparam.0 as *const RECT) };
                model::suggested_rect(r.left, r.top, r.right, r.bottom)
            };
            let mut target = None;
            {
                let mut s = state().lock().unwrap();
                let old_metrics = s
                    .metrics
                    .get(&fence_id)
                    .copied()
                    .unwrap_or_else(model::DpiMetrics::system);
                let new_metrics = metrics_for_window(hwnd);
                if let Some(fence) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                    let cols = ((fence.rect.w - old_metrics.pad * 2.0) / old_metrics.cell_w)
                        .round()
                        .max(1.0);
                    let rows = ((fence.rect.h - old_metrics.title_h - old_metrics.pad * 2.0)
                        / old_metrics.cell_h)
                        .round()
                        .max(1.0);
                    if let Some(rect) = suggested {
                        fence.rect.x = rect.x;
                        fence.rect.y = rect.y;
                    }
                    fence.rect.w = cols * new_metrics.cell_w + new_metrics.pad * 2.0 + 2.0;
                    fence.rect.h = new_metrics.title_h
                        + rows * new_metrics.cell_h
                        + new_metrics.pad * 2.0
                        + 2.0;
                    target = Some(fence.rect);
                }
                s.metrics.insert(fence_id, new_metrics);
            }
            if let Some(rect) = target {
                // SAFETY: hwnd 是本栅栏窗口；坐标来自系统建议+新 metrics
                // 重算；SWP_NOZORDER|NOACTIVATE=纯移动缩放，不重排 z
                // （owned 栅栏不得带动 owner，也不触发 z 守卫）。
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        rect.x.round() as i32,
                        rect.y.round() as i32,
                        rect.w.round() as i32,
                        rect.h.round() as i32,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
            } else {
                log(&format!("WM_DPICHANGED without fence {fence_id}"));
            }
            invalidate_hosts_cache();
            refresh_fence(fence_id);
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => {
            // 显示器分辨率/DPI 变化：重建所有 surface 并重排布局、重挂宿主
            invalidate_hosts_cache();
            sync_icon_size();
            settle_all_fences();
            refresh_all_fences();
            ensure_all_attached();
            return LRESULT(0);
        }
        WM_DL3_SHOW_ALL => {
            ZEN_MODE.store(false, Ordering::Relaxed);
            set_desktop_state_stored("normal");
            set_all_hidden(false);
            return LRESULT(0);
        }
        WM_SIZE => {
            // 回退：即使 WM_WINDOWPOSCHANGING 拦截失败，也兜底恢复
            if wparam.0 as u32 == SIZE_MINIMIZED {
                let _z = z_scope(ZIntent::Restore);
                // SAFETY: hwnd 是本窗口；z_scope(ZIntent::Restore) 声明
                // 自家恢复操作；第二个 SetWindowPos 只做 NOZORDER 的
                // 重新定位请求（清理最小化残留状态），无副作用参数。
                unsafe {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            return LRESULT(0);
        }
        WM_WINDOWPOSCHANGING => {
            // 调整 owned 栅栏不能带动 Explorer owner 的层级。
            if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != 0 {
                // SAFETY: WM_WINDOWPOSCHANGING 契约：lparam 指向系统所有的
                // WINDOWPOS，窗口过程内可写；置 SWP_NOOWNERZORDER 是该消息
                // 的预期用法（阻止 owned 调整联动 owner）。
                let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
                wp.flags |= SWP_NOOWNERZORDER;
            }
            // 阻止 Win+D / Win+M 对栅栏的摆布:栅栏常驻桌面,不参与窗口管理。
            // 我们自己主动隐藏(隐藏全部栅栏)时 INTENTIONAL_HIDE 为真,放行;
            // 自家定位操作(创建/拖拽/修复/呈现)以 Z_INTENT 标记放行。
            // z 否决只作用于真栅栏(GWLP_USERDATA=fence_id):菜单宿主等辅助窗
            // 的 z 无关紧要,却会被 IME 子系统周期性重排——否决它只会招来
            // 无限重试的对抗循环(2026-08-28 实测 0xf05d6 每 3-5s 一次)。
            if !INTENTIONAL_HIDE.load(Ordering::SeqCst) && !z_intent_active() {
                // SAFETY: 同消息契约：WINDOWPOS 在窗口过程内可写，改 flags
                // （否决外部隐藏/z 重排）是该消息的预期用法。
                let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
                if (wp.flags.0 & SWP_HIDEWINDOW.0) != 0 && (wp.flags.0 & SWP_SHOWWINDOW.0) == 0 {
                    wp.flags.0 &= !SWP_HIDEWINDOW.0;
                    wp.flags.0 |= SWP_SHOWWINDOW.0;
                    wp.flags.0 |= SWP_NOACTIVATE.0;
                }
                let is_fence = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != 0;
                if is_fence && z_guard_setting() && (wp.flags.0 & SWP_NOZORDER.0) == 0 {
                    wp.flags.0 |= SWP_NOZORDER.0;
                    log(&format!(
                        "z-guard: external z change vetoed h=0x{:x} after=0x{:x} flags=0x{:x}",
                        hwnd.0 as usize, wp.hwndInsertAfter.0 as usize, wp.flags.0
                    ));
                }
            }
            return LRESULT(0);
        }
        WM_WINDOWPOSCHANGED => {
            // 显示桌面的 z 沉底不经可否决的 WINDOWPOSCHANGING(实测 veto
            // 零命中),只能在变更落地后自检并立即归位。自家操作带 ZIntent
            // 不会进入此分支;主动隐藏期间跳过(隐藏态无需在带)。
            // 仅真栅栏需要(GWLP_USERDATA),菜单宿主的重排是正常现象。
            if !INTENTIONAL_HIDE.load(Ordering::SeqCst)
                && !z_intent_active()
                && unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != 0
            {
                // SAFETY: WM_WINDOWPOSCHANGED 契约：lparam 指向已应用的
                // WINDOWPOS，窗口过程内只读；仅取字段做日志与判定。
                let wp = unsafe { &*(lparam.0 as *const WINDOWPOS) };
                log(&format!(
                    "z-guard: external pos-changed h=0x{:x} after=0x{:x} flags=0x{:x}",
                    hwnd.0 as usize, wp.hwndInsertAfter.0 as usize, wp.flags.0
                ));
                fence_reanchor_if_below_host(hwnd);
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        WM_SYSCOMMAND => {
            if (wparam.0 & 0xFFF0) == SC_MINIMIZE as usize {
                return LRESULT(0);
            }
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                return LRESULT(0);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            {
                let mut s = state().lock().unwrap();
                if let Some(sf) = s.surfaces.remove(&fence_id) {
                    render::release_surface(sf);
                }
                s.windows.remove(&fence_id);
                s.metrics.remove(&fence_id);
                s.presented.remove(&fence_id);
                s.attached.remove(&fence_id);
                s.fence_hover.remove(&fence_id);
            }
            // SAFETY: hwnd 是本窗口；销毁前注销 OLE 拖放注册（注册与窗口
            // 生命周期配对，create 时 register_drop_target 建立）。
            unsafe {
                let _ = RevokeDragDrop(hwnd);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
