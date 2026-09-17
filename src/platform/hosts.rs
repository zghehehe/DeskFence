//! 桌面宿主发现与 band 锚点/走查判据。
//! 宿主发现（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：WorkerW/
//! Progman 宿主枚举（带 500ms 缓存）、宿主选择、DWM cloaked 判定、拖拽提升
//! 锚点，以及桌面壳窗口/图标列表查找（desktop_shell_window/desktop_listview
//! ——refresh_hosts 与栅栏 owned 创建都依赖它，随本模块迁出）。
//! band 锚点解析族与走查判据（2026-09-17 从 selfheal.rs 原样搬出，纯搬家
//! 不改行为）：present 创建栅栏取锚与 selfheal 修复/走查判据都只依赖本
//! 模块，present 与 selfheal 的互相依赖就此断开（present→hosts、
//! selfheal→present 单向）。
//! 叶子模块——只依赖 std / windows crate / crate::shell / crate::model /
//! crate::winids / crate::state(resize_now_ms)，不依赖 crate::ui。

use std::sync::{Mutex, OnceLock};

use windows::core::BOOL;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::Rect;
use crate::shell;
use crate::state::resize_now_ms;
use crate::winids::{SyncHandle, MENU_HOST_HWND, TRAY_HWND};

// ---------------- 桌面壳窗口与图标列表查找 ----------------

/// # Safety
/// EnumWindows 的回调契约：lparam 是调用方透传的原值（desktop_listview 的
/// 栈槽位 Option<HWND> 指针），枚举同步执行、回调在返回前完成，槽位生命
/// 周期覆盖；本回调只读写槽位与栈缓冲，命中即返回 FALSE 终止枚举。
unsafe extern "system" fn find_workerw_lv(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let slot: &mut Option<HWND> = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
    if slot.is_some() {
        return BOOL(0);
    }
    let mut buf = [0u16; 256];
    if unsafe { GetClassNameW(hwnd, &mut buf) } > 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let cls = String::from_utf16_lossy(&buf[..end]);
        // DefView 可能挂在 Progman 直下,也可能挂在任一 WorkerW 下(壁纸切换后),
        // 两种都接受 —— 实测某些环境下 FindWindowW("Progman") 会失败,必须靠枚举兜底
        if cls == "WorkerW" || cls == "Progman" {
            // 局部 Vec 保持字符串存活，避免临时指针悬垂（use-after-free）
            let defview_cls = shell::wide("SHELLDLL_DefView");
            let defview = unsafe {
                FindWindowExW(
                    Some(hwnd),
                    None,
                    PCWSTR::from_raw(defview_cls.as_ptr()),
                    None,
                )
                .unwrap_or_default()
            };
            if !defview.0.is_null() {
                let lv_cls = shell::wide("SysListView32");
                let lv = unsafe {
                    FindWindowExW(Some(defview), None, PCWSTR::from_raw(lv_cls.as_ptr()), None)
                        .unwrap_or_default()
                };
                if !lv.0.is_null() {
                    *slot = Some(lv);
                    return BOOL(0);
                }
            }
        }
    }
    BOOL(1)
}

/// 查找桌面图标列表（SysListView32，Progman 或 WorkerW）
pub(crate) fn desktop_listview() -> Option<HWND> {
    // SAFETY: 三个类名均为 NUL 宽串（同步查找期间存活）；EnumWindows 的
    // lparam 承载栈槽位指针（同步枚举，回调在返回前完成）。
    unsafe {
        let progman_cls = shell::wide("Progman");
        let defview_cls = shell::wide("SHELLDLL_DefView");
        let lv_cls = shell::wide("SysListView32");
        let progman = FindWindowW(PCWSTR::from_raw(progman_cls.as_ptr()), None).unwrap_or_default();
        if !progman.0.is_null() {
            let defview = FindWindowExW(
                Some(progman),
                None,
                PCWSTR::from_raw(defview_cls.as_ptr()),
                None,
            )
            .unwrap_or_default();
            if !defview.0.is_null() {
                let lv =
                    FindWindowExW(Some(defview), None, PCWSTR::from_raw(lv_cls.as_ptr()), None)
                        .unwrap_or_default();
                if !lv.0.is_null() {
                    return Some(lv);
                }
            }
        }
        let mut slot: Option<HWND> = None;
        let _ = EnumWindows(
            Some(find_workerw_lv),
            LPARAM(&mut slot as *mut Option<HWND> as isize),
        );
        slot
    }
}

/// 桌面壳窗口(WorkerW 或 Progman),用作顶层栅栏的 owner 和 z 序下界。
/// SetWindowPos 的锚点必须在其上方;直接锚此窗口表示放在它下面。
pub(crate) fn desktop_shell_window() -> Option<HWND> {
    let lv = desktop_listview()?;
    // SAFETY: lv 是现查的现存窗口；GetParent 只查询父子链上的既有窗口，
    // 空结果已判空。
    unsafe {
        let defview = GetParent(lv).unwrap_or_default();
        if !defview.0.is_null() {
            let parent = GetParent(defview).unwrap_or_default();
            if !parent.0.is_null() {
                return Some(parent);
            }
            return Some(defview);
        }
        None
    }
}

// ---------------- 桌面宿主(WorkerW 收养) ----------------

/// 可收养栅栏窗口的桌面宿主:带图标的 WorkerW/Progman(主屏)或
/// 通过 0x052C 消息生成的每显示器 WorkerW(副屏)。坐标为屏幕坐标。
#[derive(Clone, Copy)]
pub struct HostInfo {
    pub hwnd: HWND,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub primary: bool,
    /// 宿主是否可见:Explorer 重启重建期间 WorkerW 可能短暂隐藏,
    /// 收养到隐藏宿主会导致栅栏不可见,必须过滤。
    pub visible: bool,
}

type HostsCacheInner = OnceLock<Mutex<(std::time::Instant, Vec<HostInfo>)>>;
static HOSTS_CACHE: SyncHandle<HostsCacheInner> = SyncHandle(OnceLock::new());

pub(crate) fn invalidate_hosts_cache() {
    if let Some(cache) = HOSTS_CACHE.get() {
        let mut c = cache.lock().unwrap();
        c.0 = std::time::Instant::now() - std::time::Duration::from_secs(10);
    }
}

/// 桌面宿主列表(带 500ms 缓存,拖动高频调用不重复枚举窗口)
pub(crate) fn desktop_hosts() -> Vec<HostInfo> {
    {
        if let Some(cache) = HOSTS_CACHE.get() {
            let c = cache.lock().unwrap();
            if c.0.elapsed() < std::time::Duration::from_millis(1000) {
                return c.1.clone();
            }
        }
    }
    let hosts = refresh_hosts();
    let cache = HOSTS_CACHE.get_or_init(|| Mutex::new((std::time::Instant::now(), Vec::new())));
    let mut c = cache.lock().unwrap();
    *c = (std::time::Instant::now(), hosts.clone());
    hosts
}

/// # Safety
/// EnumWindows 的回调契约：lparam 指向 refresh_hosts 的栈元组
/// (Vec<HostInfo>, HWND)（同步枚举期间有效）；本回调只追加宿主信息、
/// 读栈缓冲/栈矩形，恒返回 TRUE 走完全表。
unsafe extern "system" fn enum_workerw_host(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut (Vec<HostInfo>, HWND)) };
    let mut buf = [0u16; 256];
    if unsafe { GetClassNameW(hwnd, &mut buf) } > 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if String::from_utf16_lossy(&buf[..end]) == "WorkerW" {
            unsafe {
                let mut r: RECT = std::mem::zeroed();
                let _ = GetWindowRect(hwnd, &mut r);
                ctx.0.push(HostInfo {
                    hwnd,
                    x: r.left as f32,
                    y: r.top as f32,
                    w: (r.right - r.left) as f32,
                    h: (r.bottom - r.top) as f32,
                    primary: hwnd == ctx.1,
                    visible: IsWindowVisible(hwnd).as_bool(),
                });
            }
        }
    }
    BOOL(1)
}

fn refresh_hosts() -> Vec<HostInfo> {
    let primary = desktop_shell_window().unwrap_or(HWND(std::ptr::null_mut()));
    let mut hosts: Vec<HostInfo> = Vec::new();
    // SAFETY: ctx 是栈元组，其指针经 lparam 透传给回调（EnumWindows 同步
    // 枚举，回调在返回前完成）。
    unsafe {
        let mut ctx = (hosts, primary);
        let _ = EnumWindows(
            Some(enum_workerw_host),
            LPARAM(&mut ctx as *mut (Vec<HostInfo>, HWND) as isize),
        );
        hosts = ctx.0;
    }
    // 老路径主桌面是 Progman(非 WorkerW):补一个 primary 宿主
    if !primary.0.is_null() && !hosts.iter().any(|h| h.hwnd == primary) {
        // SAFETY: 全零 RECT 合法；primary 是现查的现存窗口，
        // GetWindowRect 只读填充。
        let mut r: RECT = unsafe { std::mem::zeroed() };
        // SAFETY: 同上。
        unsafe {
            let _ = GetWindowRect(primary, &mut r);
        }
        hosts.push(HostInfo {
            hwnd: primary,
            x: r.left as f32,
            y: r.top as f32,
            w: (r.right - r.left) as f32,
            h: (r.bottom - r.top) as f32,
            primary: true,
            visible: unsafe { IsWindowVisible(primary).as_bool() },
        });
    }
    hosts.sort_by_key(|h| !h.primary);
    hosts
}

/// 为栅栏选择宿主:中心点落在哪个【可见】宿主就收养到哪;都不覆盖时返回 None
/// (窗口暂缓创建,由全局定时器在宿主就绪后补挂,绝不复用 HWND_TOP 回退)。
pub fn host_for_rect(rect: &Rect, hosts: &[HostInfo]) -> Option<HostInfo> {
    let cx = rect.x + rect.w * 0.5;
    let cy = rect.y + rect.h * 0.5;
    hosts
        .iter()
        .filter(|h| h.visible)
        .find(|h| cx >= h.x && cx < h.x + h.w && cy >= h.y && cy < h.y + h.h)
        .copied()
}

/// DWM cloaked 判定:窗口"可见"位有效但 DWM 不合成其像素——物理上遮不住任何东西。
/// 典型:SystemSettings/TextInputHost 的全屏 CoreWindow(cloak=2)、Shell 经验宿主、
/// 某些安全/管控软件钩子层的全屏瞬态。菜单开合瞬间它们被塞进宿主与栅栏之间,曾触发整链
/// 重排(每次=z 序重排闪屏),必须跳过。
pub(crate) fn window_is_cloaked(w: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: DwmGetWindowAttribute 契约——pvAttribute 指向调用方缓冲、
    // cbAttribute 传其大小；cloaked 是栈变量，调用期间有效。
    let ok = unsafe {
        DwmGetWindowAttribute(
            w,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok();
    ok && cloaked != 0
}

/// 拖拽提升锚点:被拖栅栏需要压过其他兄弟栅栏(拖过邻居时不被盖住),
/// 但绝不能高于正常窗口——HWND_TOP 曾把它顶到整个 z 栈顶端(浮窗)。
/// 返回"最高兄弟栅栏"的句柄(插到它之后=兄弟之上、正常窗口之下);
/// 没有其他兄弟栅栏时返回 None(无需提升)。
pub(crate) fn drag_elevate_anchor(host: HWND, dragged: HWND) -> Option<HWND> {
    let mut anchor: Option<HWND> = None;
    // SAFETY(走查循环): GetWindow/GetClassNameW 均为同步纯查询（无指针
    // 参数/栈缓冲按长度截断），失败得 null/0 由判空终止。
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) }.unwrap_or_default();
    for _ in 0..64 {
        if w.0.is_null() {
            break;
        }
        if w == dragged {
            w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
            continue;
        }
        // 只沿"连续的兄弟栅栏段"向上找;段结束(遇到非栅栏窗)即停
        let mut cls_buf = [0u16; 32];
        let n = unsafe { GetClassNameW(w, &mut cls_buf) };
        let is_fence = n == 14
            && cls_buf[..14]
                == [
                    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x46, 0x65, 0x6E, 0x63,
                    0x65,
                ];
        if !is_fence {
            break;
        }
        anchor = Some(w);
        w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
    }
    anchor
}

// ---------------- band 锚点解析与走查判据 ----------------
// （2026-09-17 从 selfheal.rs 原样搬出，纯搬家不改行为）

/// 栅栏窗口类名("DeskFenceFence",14 字符)——供无锁判定自家栅栏。
pub const FENCE_CLASS: [u16; 14] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x46, 0x65, 0x6E, 0x63, 0x65,
];

/// 锚点解析可在窗口过程或持状态锁时调用,此处不能再次取状态锁。
pub(crate) fn is_own_fence_window(w: HWND) -> bool {
    let mut cls_buf = [0u16; 16];
    // SAFETY: cls_buf 是 32 字节栈缓冲；GetClassNameW 至多写 buf.len() 项
    // 并返回实写长度（后续切片按 n 截断）；w 是枚举/走查得到的现存窗口。
    let n = unsafe { GetClassNameW(w, &mut cls_buf) };
    n as usize == FENCE_CLASS.len() && cls_buf[..FENCE_CLASS.len()] == FENCE_CLASS
}

fn is_topmost_window(w: HWND) -> bool {
    // WS_EX_TOPMOST = 0x8
    (unsafe { GetWindowLongW(w, GWL_EXSTYLE) } & 0x8) != 0
}

// ---------------- UIPI 坏锚缓存 ----------------
// 被拒过的锚点暂时排除;重试仍只能使用首个可见外来窗下方的安全候选。
// 成功插入即清除。TTL 限制句柄复用造成的影响。
const BAD_ANCHOR_TTL_MS: u64 = 60_000;
static BAD_ANCHORS: Mutex<Vec<(isize, u64)>> = Mutex::new(Vec::new());

pub(crate) fn bad_anchor_mark(h: HWND) {
    let now = resize_now_ms();
    let mut g = BAD_ANCHORS.lock().unwrap();
    g.retain(|(k, t)| now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS && *k != h.0 as isize);
    g.push((h.0 as isize, now));
}

pub(crate) fn bad_anchor_clear(h: HWND) {
    let now = resize_now_ms();
    let mut g = BAD_ANCHORS.lock().unwrap();
    g.retain(|(k, t)| now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS && *k != h.0 as isize);
}

fn bad_anchor_recent(h: HWND) -> bool {
    let now = resize_now_ms();
    let g = BAD_ANCHORS.lock().unwrap();
    g.iter()
        .any(|(k, t)| *k == h.0 as isize && now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS)
}

/// 窗口查询与策略分离:纯策略只消费由宿主向上排列的窗口属性。
#[derive(Clone, Copy, Default)]
struct AnchorWindow {
    handle: isize,
    own: bool,
    invisible: bool,
    auxiliary: bool,
    topmost: bool,
    bad: bool,
}

/// 首个真实可见非 topmost 外来窗是不可越过的上界。能锚它就返回;
/// 被拒时只用此前扫过的垫窗或兄弟。topmost 是终止边界,即使隐形/自家
/// 也不能继续向上搜索。所有出口排除宿主、自身、topmost 和坏锚。
fn resolve_band_anchor(
    host: isize,
    skip: isize,
    windows: impl IntoIterator<Item = AnchorWindow>,
) -> Option<isize> {
    let mut padding = None;
    let mut sibling = None;
    for w in windows {
        if w.handle == 0 || w.handle == host || w.topmost {
            break;
        }
        if w.handle == skip {
            continue;
        }
        if w.own {
            if !w.bad && sibling.is_none() {
                sibling = Some(w.handle);
            }
        } else if w.invisible || w.auxiliary {
            if !w.bad {
                padding = Some(w.handle);
            }
        } else {
            return if w.bad {
                padding.or(sibling)
            } else {
                Some(w.handle)
            };
        }
    }
    padding.or(sibling)
}

/// SetWindowPos(F, A) 将 F 放到 A 正下方。只向宿主上方扫描一次,由纯策略
/// 选择普通带内的安全锚;没有合格候选就不移动,不回退宿主/self/topmost。
pub(crate) fn band_attach_anchor(host: HWND, skip: HWND) -> Option<HWND> {
    let vs = virtual_screen_rect();
    let menu_host = MENU_HOST_HWND.get().copied();
    let tray = TRAY_HWND.get().copied();
    // SAFETY(整链): GetWindow(GW_HWNDPREV) 沿 z 链向上取现存窗口（无指针
    // 参数；返回 null 表示到顶，迭代器终止）；扫描在 from_fn 闭包内同步
    // 完成，w 的中间状态不逃逸。
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) }.unwrap_or_default();
    let windows = std::iter::from_fn(|| {
        if w.0.is_null() {
            return None;
        }
        let current = w;
        w = unsafe { GetWindow(current, GW_HWNDPREV) }.unwrap_or_default();
        Some(AnchorWindow {
            handle: current.0 as isize,
            own: is_own_fence_window(current),
            invisible: band_invisible(current, &vs),
            auxiliary: band_aux(current, menu_host, tray),
            topmost: is_topmost_window(current),
            bad: bad_anchor_recent(current),
        })
    })
    .take(1000);
    resolve_band_anchor(host.0 as isize, skip.0 as isize, windows)
        .map(|h| HWND(h as *mut std::ffi::c_void))
}

// ---------------- band 走查共享判据 ----------------
// 主走查与快速下压使用相同的可忽略窗口语义。锚点解析额外把 topmost
// 作为边界,避免将栅栏并入 topmost 带;真实失位仍由走查和下压负责恢复。

pub(crate) struct VirtualScreen {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

pub(crate) fn virtual_screen_rect() -> VirtualScreen {
    // SAFETY: 四个 GetSystemMetrics 均无指针参数、无失败前提。
    unsafe {
        VirtualScreen {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

pub const MENU_CLASS: [u16; 6] = [0x23, 0x33, 0x32, 0x37, 0x36, 0x38]; // "#32768"
pub const TRAY_CLASS: [u16; 13] = [
    0x53, 0x68, 0x65, 0x6C, 0x6C, 0x5F, 0x54, 0x72, 0x61, 0x79, 0x57, 0x6E, 0x64,
]; // "Shell_TrayWnd"
   // 系统触摸/输入边缘条作为桌面辅助窗容忍。
pub const EDGEUI_CLASS: [u16; 22] = [
    0x45, 0x64, 0x67, 0x65, 0x55, 0x69, 0x49, 0x6E, 0x70, 0x75, 0x74, 0x54, 0x6F, 0x70, 0x57, 0x6E,
    0x64, 0x43, 0x6C, 0x61, 0x73, 0x73,
];

// IME 候选/状态窗与线程宿主属于既有辅助窗容忍集,不作为真实应用窗边界。
pub const MSCTFIME_CLASS: [u16; 11] = [
    0x4D, 0x53, 0x43, 0x54, 0x46, 0x49, 0x4D, 0x45, 0x20, 0x55, 0x49,
]; // "MSCTFIME UI"
pub const DEFAULT_IME_CLASS: [u16; 11] = [
    0x44, 0x65, 0x66, 0x61, 0x75, 0x6C, 0x74, 0x20, 0x49, 0x4D, 0x45,
]; // "Default IME"
   // 第三方输入法 TSF 基础设施窗组沿用同一辅助窗语义。
pub const GENERIC_IME_CLASS: [u16; 3] = [0x49, 0x4D, 0x45]; // "IME"
pub const SOIME_TSF_CLASS: [u16; 14] = [
    0x53, 0x6F, 0x49, 0x6D, 0x65, 0x42, 0x53, 0x5F, 0x54, 0x53, 0x46, 0x5F, 0x55, 0x49,
]; // "SoImeBS_TSF_UI"
pub const SOBS_UI_CLASS: [u16; 7] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x55, 0x49]; // "SoBS_UI"
pub const SOBS_HINT_CLASS: [u16; 9] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x48, 0x69, 0x6E, 0x74]; // "SoBS_Hint"
pub const CATS_PANEL_CLASS: [u16; 18] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x43, 0x61, 0x74, 0x73, 0x50, 0x61, 0x6E,
    0x65, 0x6C,
]; // "DeskFenceCatsPanel"

/// band 走查的不可见判据:隐藏/最小化/离屏/退化尺寸(≤2px)/cloaked。
pub(crate) fn band_invisible(w: HWND, vs: &VirtualScreen) -> bool {
    let mut wr = RECT::default();
    // SAFETY: wr 是栈输出指针（GetWindowRect 契约）；其余为纯句柄查询。
    let rect_ok = unsafe { GetWindowRect(w, &mut wr) }.is_ok();
    !rect_ok
        || unsafe { IsIconic(w).as_bool() }
        || !unsafe { IsWindowVisible(w).as_bool() }
        || wr.right - wr.left <= 2
        || wr.bottom - wr.top <= 2
        || wr.right <= vs.x
        || wr.bottom <= vs.y
        || wr.left >= vs.x + vs.w
        || wr.top >= vs.y + vs.h
        || window_is_cloaked(w)
}

/// 自有辅助窗及既有系统辅助窗容忍集;真实应用窗不按尺寸或深度跳过。
pub(crate) fn band_aux(w: HWND, menu_host: Option<HWND>, tray: Option<HWND>) -> bool {
    if menu_host == Some(w) || tray == Some(w) {
        return true;
    }
    let mut cls_buf = [0u16; 32];
    // SAFETY: 同 is_own_fence_window：栈缓冲 + 长度截断；w 为现存窗口。
    let n = unsafe { GetClassNameW(w, &mut cls_buf) };
    (n == 6 && cls_buf[..6] == MENU_CLASS)
        || (n == 13 && cls_buf[..13] == TRAY_CLASS)
        || (n == 22 && cls_buf[..22] == EDGEUI_CLASS)
        || (n == 11 && cls_buf[..11] == MSCTFIME_CLASS)
        || (n == 11 && cls_buf[..11] == DEFAULT_IME_CLASS)
        || (n == 3 && cls_buf[..3] == GENERIC_IME_CLASS)
        || (n == 14 && cls_buf[..14] == SOIME_TSF_CLASS)
        || (n == 7 && cls_buf[..7] == SOBS_UI_CLASS)
        || (n == 9 && cls_buf[..9] == SOBS_HINT_CLASS)
        // 分类面板可覆盖栅栏,不因此触发整组修复。
        || (n == 18 && cls_buf[..18] == CATS_PANEL_CLASS)
}

#[cfg(test)]
mod tests {
    use super::{resolve_band_anchor, AnchorWindow};

    const HOST: isize = 1;
    const FENCE: isize = 2;

    fn window(handle: isize) -> AnchorWindow {
        AnchorWindow {
            handle,
            ..Default::default()
        }
    }
    fn padding(handle: isize) -> AnchorWindow {
        AnchorWindow {
            invisible: true,
            ..window(handle)
        }
    }

    #[test]
    fn shallow_visible_window_is_hard_boundary() {
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [window(3), padding(4), window(5)]),
            Some(3)
        );
    }

    #[test]
    fn bad_visible_anchor_uses_only_preceding_padding() {
        let bad = AnchorWindow {
            bad: true,
            ..window(4)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [padding(3), bad, padding(5), window(6)]),
            Some(3)
        );
    }

    #[test]
    fn bad_anchor_without_safe_padding_returns_none() {
        let bad = AnchorWindow {
            bad: true,
            ..window(3)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [bad, window(HOST), padding(4)]),
            None
        );
    }

    #[test]
    fn host_boundary_cannot_be_crossed_or_used_as_anchor() {
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [window(HOST), padding(3)]),
            None
        );
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [padding(3), window(HOST), padding(4)]),
            Some(3)
        );
    }

    #[test]
    fn topmost_boundary_returns_only_non_topmost_padding() {
        for invisible in [false, true] {
            let topmost = AnchorWindow {
                topmost: true,
                invisible,
                ..window(4)
            };
            assert_eq!(
                resolve_band_anchor(HOST, FENCE, [padding(3), topmost, window(5)]),
                Some(3)
            );
        }
    }

    #[test]
    fn topmost_without_padding_has_no_anchor() {
        let topmost = AnchorWindow {
            topmost: true,
            ..window(3)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [topmost, padding(4)]),
            None
        );
    }

    #[test]
    fn self_is_excluded_from_every_candidate_kind() {
        for own in [false, true] {
            for invisible in [false, true] {
                let me = AnchorWindow {
                    own,
                    invisible,
                    ..window(FENCE)
                };
                assert_eq!(resolve_band_anchor(HOST, FENCE, [me]), None);
                assert_eq!(resolve_band_anchor(HOST, FENCE, [me, window(3)]), Some(3));
            }
        }
    }

    #[test]
    fn bad_padding_and_bad_siblings_are_never_returned() {
        let bad_padding = AnchorWindow {
            bad: true,
            ..padding(4)
        };
        let bad_sibling = AnchorWindow {
            own: true,
            bad: true,
            ..window(5)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [padding(3), bad_padding, bad_sibling]),
            Some(3)
        );
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [bad_padding, bad_sibling]),
            None
        );
    }

    #[test]
    fn safe_sibling_is_fallback_below_bad_visible_window() {
        let sibling = AnchorWindow {
            own: true,
            ..window(3)
        };
        let bad = AnchorWindow {
            bad: true,
            ..window(4)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [sibling, bad, padding(5)]),
            Some(3)
        );
    }

    #[test]
    fn retry_after_blacklisting_stays_below_original_boundary() {
        let first = window(4);
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [padding(3), first, window(5)]),
            Some(4)
        );
        let rejected = AnchorWindow { bad: true, ..first };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [padding(3), rejected, window(5)]),
            Some(3)
        );
        let rejected_padding = AnchorWindow {
            bad: true,
            ..padding(3)
        };
        assert_eq!(
            resolve_band_anchor(HOST, FENCE, [rejected_padding, rejected, window(5)]),
            None
        );
    }

    #[test]
    fn adjacent_visible_blocker_remains_a_lowering_anchor() {
        let me = AnchorWindow {
            own: true,
            ..window(FENCE)
        };
        assert_eq!(resolve_band_anchor(HOST, FENCE, [window(3), me]), Some(3));
    }

    /// 手编 UTF-16 类名须与分类面板实际注册名一致。
    #[test]
    fn cats_panel_class_encoding_matches_registered_name() {
        let expect: Vec<u16> = "DeskFenceCatsPanel".encode_utf16().collect();
        assert_eq!(super::CATS_PANEL_CLASS.to_vec(), expect);
    }
}
