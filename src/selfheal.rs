//! 自愈/z 序子系统:走查、受限锚点解析、意图守卫、WinEvent 与快速下压。
//! 栅栏由桌面宿主拥有;健康窗口不重排,不使用 topmost 免疫。
//! 与 ui.rs 双向依赖(同 crate 内合法)。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
// windows 0.52 未导出的 WinEvent 标志,按 WinUser.h 补定义
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
use crate::drag::*;
use crate::render;
use crate::ui::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 同一故障签名连续出现才累计拍数;恢复过渡期拦路者换窗即重置。
#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkFault {
    /// 可见外来窗先于栅栏出现在宿主之上
    Blocked { hwnd: isize, class: u64 },
    /// 走查到栈顶未找到:栅栏在宿主之下,首拍即修
    NotFoundTop,
    /// 走查预算耗尽:状态不明,只记日志不动手
    NotFoundBudget,
}

pub(crate) struct WalkStrike {
    fault: WalkFault,
    count: u32,
}

/// 计数未推进时不能重复消费第 3、13、23…拍的修复机会。
fn walk_repair_due(fault: WalkFault, strikes: u32, advanced: bool) -> bool {
    match fault {
        WalkFault::NotFoundTop => true,
        WalkFault::NotFoundBudget => false,
        WalkFault::Blocked { .. } => {
            advanced && (strikes == 3 || (strikes > 3 && (strikes - 3).is_multiple_of(10)))
        }
    }
}

fn class_hash(cls: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in cls {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 栅栏窗口类名("DeskFenceFence",14 字符)——供无锁判定自家栅栏。
const FENCE_CLASS: [u16; 14] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x46, 0x65, 0x6E, 0x63, 0x65,
];

/// 锚点解析可在窗口过程或持状态锁时调用,此处不能再次取状态锁。
pub(crate) fn is_own_fence_window(w: HWND) -> bool {
    let mut cls_buf = [0u16; 16];
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

fn bad_anchor_mark(h: HWND) {
    let now = resize_now_ms();
    let mut g = BAD_ANCHORS.lock().unwrap();
    g.retain(|(k, t)| now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS && *k != h.0);
    g.push((h.0, now));
}

fn bad_anchor_clear(h: HWND) {
    let now = resize_now_ms();
    let mut g = BAD_ANCHORS.lock().unwrap();
    g.retain(|(k, t)| now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS && *k != h.0);
}

fn bad_anchor_recent(h: HWND) -> bool {
    let now = resize_now_ms();
    let g = BAD_ANCHORS.lock().unwrap();
    g.iter()
        .any(|(k, t)| *k == h.0 && now.saturating_sub(*t) < BAD_ANCHOR_TTL_MS)
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
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
    let windows = std::iter::from_fn(|| {
        if w.0 == 0 {
            return None;
        }
        let current = w;
        w = unsafe { GetWindow(current, GW_HWNDPREV) };
        Some(AnchorWindow {
            handle: current.0,
            own: is_own_fence_window(current),
            invisible: band_invisible(current, &vs),
            auxiliary: band_aux(current, menu_host, tray),
            topmost: is_topmost_window(current),
            bad: bad_anchor_recent(current),
        })
    })
    .take(1000);
    resolve_band_anchor(host.0, skip.0, windows).map(HWND)
}

// ---------------- band 走查共享判据 ----------------
// 主走查与快速下压使用相同的可忽略窗口语义。锚点解析额外把 topmost
// 作为边界,避免将栅栏并入 topmost 带;真实失位仍由走查和下压负责恢复。

struct VirtualScreen {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn virtual_screen_rect() -> VirtualScreen {
    unsafe {
        VirtualScreen {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

const MENU_CLASS: [u16; 6] = [0x23, 0x33, 0x32, 0x37, 0x36, 0x38]; // "#32768"
const TRAY_CLASS: [u16; 13] = [
    0x53, 0x68, 0x65, 0x6C, 0x6C, 0x5F, 0x54, 0x72, 0x61, 0x79, 0x57, 0x6E, 0x64,
]; // "Shell_TrayWnd"
   // 系统触摸/输入边缘条作为桌面辅助窗容忍。
const EDGEUI_CLASS: [u16; 22] = [
    0x45, 0x64, 0x67, 0x65, 0x55, 0x69, 0x49, 0x6E, 0x70, 0x75, 0x74, 0x54, 0x6F, 0x70, 0x57, 0x6E,
    0x64, 0x43, 0x6C, 0x61, 0x73, 0x73,
];

// IME 候选/状态窗与线程宿主属于既有辅助窗容忍集,不作为真实应用窗边界。
const MSCTFIME_CLASS: [u16; 11] = [
    0x4D, 0x53, 0x43, 0x54, 0x46, 0x49, 0x4D, 0x45, 0x20, 0x55, 0x49,
]; // "MSCTFIME UI"
const DEFAULT_IME_CLASS: [u16; 11] = [
    0x44, 0x65, 0x66, 0x61, 0x75, 0x6C, 0x74, 0x20, 0x49, 0x4D, 0x45,
]; // "Default IME"
   // 第三方输入法 TSF 基础设施窗组沿用同一辅助窗语义。
const GENERIC_IME_CLASS: [u16; 3] = [0x49, 0x4D, 0x45]; // "IME"
const SOIME_TSF_CLASS: [u16; 14] = [
    0x53, 0x6F, 0x49, 0x6D, 0x65, 0x42, 0x53, 0x5F, 0x54, 0x53, 0x46, 0x5F, 0x55, 0x49,
]; // "SoImeBS_TSF_UI"
const SOBS_UI_CLASS: [u16; 7] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x55, 0x49]; // "SoBS_UI"
const SOBS_HINT_CLASS: [u16; 9] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x48, 0x69, 0x6E, 0x74]; // "SoBS_Hint"
const CATS_PANEL_CLASS: [u16; 18] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x43, 0x61, 0x74, 0x73, 0x50, 0x61, 0x6E,
    0x65, 0x6C,
]; // "DeskFenceCatsPanel"

/// band 走查的不可见判据:隐藏/最小化/离屏/退化尺寸(≤2px)/cloaked。
fn band_invisible(w: HWND, vs: &VirtualScreen) -> bool {
    let mut wr = RECT::default();
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
fn band_aux(w: HWND, menu_host: Option<HWND>, tray: Option<HWND>) -> bool {
    if menu_host == Some(w) || tray == Some(w) {
        return true;
    }
    let mut cls_buf = [0u16; 32];
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

/// 清理失效窗口、延迟创建、逐栅栏检查桌面带位。被拦截按同签名三拍确认,
/// 沉底首拍即修、预算耗尽不动。健康窗口不重排;修复只改变 z 序。
pub(crate) fn ensure_all_attached() {
    let hosts = desktop_hosts();
    let mut created: Vec<u32> = Vec::new();
    {
        let mut s = state().lock().unwrap();
        // Attachment is valid only for the host enumeration from this pass.
        s.attached.clear();
        // 1) 清理已销毁的窗口
        let stale: Vec<u32> = s
            .windows
            .iter()
            .filter(|(_, h)| !unsafe { IsWindow(**h).as_bool() })
            .map(|(k, _)| *k)
            .collect();
        for id in stale {
            s.windows.remove(&id);
            s.metrics.remove(&id);
            s.presented.remove(&id);
            s.attached.remove(&id);
            if let Some(sf) = s.surfaces.remove(&id) {
                render::release_surface(sf);
            }
            s.fence_hover.remove(&id);
        }
        // 2) 创建缺失窗口(宿主已就绪)
        let ids: Vec<u32> = s.fences.iter().map(|f| f.id).collect();
        for id in ids {
            if !s.windows.contains_key(&id) && create_fence_window(&mut s, id, &hosts) {
                created.push(id);
            }
        }
        // 3) 先遇自己即健康;其他自家栅栏、隐形窗、辅助窗可跳过。先遇真实
        // 可见外来窗意味着栅栏浮在它上方,与矩形是否相交无关。健康窗口不
        // 粘底、不晋升,避免同位重排触发重合成。
        let trayw = TRAY_HWND.get().copied();
        let host1 = MENU_HOST_HWND.get().copied();
        let vs = virtual_screen_rect();
        let mut to_move: Vec<u32> = Vec::new();
        let mut all_healthy = true;
        for (id, h) in s.windows.clone() {
            // 拖动中暂时豁免,松手由拖拽路径使用同一受限锚点归位。
            if matches!(&s.drag, Some(d) if d.fence_id == id) {
                s.walk_strikes.remove(&id);
                s.walk_strike_ms.remove(&id);
                s.attached.insert(id);
                continue;
            }
            let Some(fence) = s.fences.iter().find(|f| f.id == id) else {
                continue;
            };
            let Some(host) = host_for_rect(&fence.rect, &hosts) else {
                continue;
            };
            let mut out_of_band = false;
            let mut found = false;
            let mut blocker = (0isize, 0u64);
            // 覆盖宿主上方大量不可见辅助窗;到顶与预算耗尽分别处理。
            let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
            let mut budget = 0usize;
            for _ in 0..1000 {
                if w.0 == 0 {
                    budget = usize::MAX;
                    break;
                }
                budget += 1;
                if h == w {
                    found = true;
                    break;
                }
                if s.windows.values().any(|v| *v == w)
                    || band_invisible(w, &vs)
                    || band_aux(w, host1, trayw)
                {
                    w = unsafe { GetWindow(w, GW_HWNDPREV) };
                    continue;
                }
                let mut cls_buf = [0u16; 32];
                let n = unsafe { GetClassNameW(w, &mut cls_buf) };
                out_of_band = true;
                blocker = (w.0, class_hash(&cls_buf[..n.max(0) as usize]));
                if to_move.is_empty() {
                    let mut dr = RECT::default();
                    let _ = unsafe { GetWindowRect(w, &mut dr) };
                    let own = s.windows.values().any(|v| *v == w);
                    log(&format!(
                        "walk-break: fence {id} host=0x{:x} blocked by cls={} own={} h=0x{:x} rect=({},{})-({},{}) vis={} iconic={}",
                        host.hwnd.0,
                        String::from_utf16_lossy(&cls_buf[..n.max(0) as usize]),
                        own, w.0, dr.left, dr.top, dr.right, dr.bottom,
                        unsafe { IsWindowVisible(w).as_bool() },
                        unsafe { IsIconic(w).as_bool() }
                    ));
                }
                break;
            }
            let fault = if out_of_band {
                Some(WalkFault::Blocked {
                    hwnd: blocker.0,
                    class: blocker.1,
                })
            } else if !found {
                if budget == usize::MAX {
                    Some(WalkFault::NotFoundTop)
                } else {
                    Some(WalkFault::NotFoundBudget)
                }
            } else {
                None
            };
            if let Some(fault) = fault {
                all_healthy = false;
                // 同签名连续三拍才修,之后第 13、23…拍退避重试。
                // 重复调用时,计数及 Blocked 修复均须等待推进。
                let now = resize_now_ms();
                let last = s.walk_strike_ms.get(&id).copied().unwrap_or(0);
                let advanced = last == 0 || now.saturating_sub(last) >= 500;
                if advanced {
                    s.walk_strike_ms.insert(id, now);
                }
                let strikes_n = {
                    let e = s
                        .walk_strikes
                        .entry(id)
                        .or_insert(WalkStrike { fault, count: 0 });
                    if e.fault != fault {
                        e.fault = fault;
                        e.count = 0;
                    }
                    if advanced {
                        e.count += 1;
                    }
                    e.count
                };
                let attempt = walk_repair_due(fault, strikes_n, advanced);
                if matches!(fault, WalkFault::NotFoundTop | WalkFault::NotFoundBudget)
                    && to_move.is_empty()
                {
                    let why = if budget == usize::MAX {
                        "top reached"
                    } else {
                        "budget exhausted"
                    };
                    log(&format!(
                        "walk-break: fence {id} host=0x{:x} NOT FOUND in {} steps ({}), strikes={}",
                        host.hwnd.0, budget, why, strikes_n
                    ));
                }
                if attempt {
                    to_move.push(id);
                } else if strikes_n == 1 {
                    log(&format!(
                        "walk-break: fence {id} flagged strike 1/3, waiting confirm"
                    ));
                }
            } else {
                s.walk_strikes.remove(&id);
                s.walk_strike_ms.remove(&id);
                s.last_healthy_ms.insert(id, resize_now_ms());
                s.attached.insert(id);
                // z 序健康不代表未被最小化;纯 z 修复无法恢复 iconic 状态。
                if unsafe { IsIconic(h) }.as_bool() {
                    let _z = z_scope(ZIntent::Restore);
                    unsafe {
                        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                    }
                    log(&format!("walk: fence {id} was iconic, restored"));
                }
            }
        }
        if !to_move.is_empty() {
            let mut moved: Vec<u32> = Vec::new();
            let mut culprit = String::from("none");
            for id in &to_move {
                let Some(h) = s.windows.get(id).copied() else {
                    continue;
                };
                let Some(frect) = s.fences.iter().find(|f| f.id == *id).map(|f| f.rect) else {
                    continue;
                };
                let Some(host) = host_for_rect(&frect, &hosts) else {
                    continue;
                };
                // 纯 z 修复;失败后标记坏锚并重新解析,绝不裸向上越过可见窗。
                let _z = z_scope(ZIntent::Repair);
                let mut attached = false;
                let mut first_err = None;
                let mut anchor = band_attach_anchor(host.hwnd, h);
                let mut tried = 0;
                while let Some(after) = anchor {
                    if tried >= 3 {
                        break;
                    }
                    tried += 1;
                    let attempt = unsafe {
                        SetWindowPos(
                            h,
                            after,
                            frect.x.round() as i32,
                            frect.y.round() as i32,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                    match attempt {
                        Ok(()) => {
                            attached = true;
                            bad_anchor_clear(after);
                            if tried > 1 {
                                log(&format!(
                                    "repair fence {id} succeeded on retry #{tried} (anchor 0x{:x})",
                                    after.0
                                ));
                            }
                            break;
                        }
                        Err(e) => {
                            bad_anchor_mark(after);
                            if first_err.is_none() {
                                first_err = Some((after, e));
                            }
                            anchor = band_attach_anchor(host.hwnd, h);
                        }
                    }
                }
                if !attached && moved.is_empty() && to_move.len() <= 6 {
                    if let Some((after, e)) = first_err {
                        log(&format!(
                            "repair FAILED fence {} h=0x{:x} after=0x{:x} err={:?}",
                            id, h.0, after.0, e
                        ));
                    }
                }
                if attached {
                    s.attached.insert(*id);
                    moved.push(*id);
                    // 修复后的局部遮挡诊断(64 步、矩形相交),不参与带位判定。
                    let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
                    for _ in 0..64 {
                        if w.0 == 0 || w == h {
                            break;
                        }
                        let mut wr = RECT::default();
                        let _ = unsafe { GetWindowRect(w, &mut wr) };
                        if band_invisible(w, &vs) || band_aux(w, host1, trayw) {
                            w = unsafe { GetWindow(w, GW_HWNDPREV) };
                            continue;
                        }
                        let fx0 = frect.x.round() as i32;
                        let fy0 = frect.y.round() as i32;
                        let fx1 = fx0 + frect.w.round() as i32;
                        let fy1 = fy0 + frect.h.round() as i32;
                        if wr.left < fx1 && wr.right > fx0 && wr.top < fy1 && wr.bottom > fy0 {
                            let mut cls_buf = [0u16; 32];
                            let n = unsafe { GetClassNameW(w, &mut cls_buf) };
                            culprit = String::from_utf16_lossy(&cls_buf[..n.max(0) as usize]);
                        }
                        break;
                    }
                }
            }
            if !moved.is_empty() {
                log(&format!(
                    "z-chain repair: fences {:?} re-attached (occluder={})",
                    moved, culprit
                ));
            }
        }
        s.band_quiet = all_healthy;
        drop(s);
    }
    for id in created {
        refresh_fence(id);
    }
}

// ---------------- z 序意图守卫 ----------------

/// 自家 SetWindowPos/ShowWindow 在 UI 线程同步触发定位消息,以线程局部
/// 意图放行;外部经消息泵派发的操作到达时无此标记。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZIntent {
    /// 创建栅栏窗口并插入底带
    Create,
    /// 主动显示/刷新呈现
    Show,
    /// 拖拽提升/落点归位
    Drag,
    /// z 自愈修复
    Repair,
    /// 最小化兜底恢复
    Restore,
}

thread_local! {
    static Z_INTENT: std::cell::Cell<Option<ZIntent>> = const { std::cell::Cell::new(None) };
}

/// RAII 守卫:作用域内的窗口定位操作被拦截逻辑放行。嵌套时恢复前值。
pub(crate) struct ZScope(Option<ZIntent>);

pub(crate) fn z_scope(intent: ZIntent) -> ZScope {
    let prev = Z_INTENT.with(|c| c.replace(Some(intent)));
    ZScope(prev)
}

impl Drop for ZScope {
    fn drop(&mut self) {
        Z_INTENT.with(|c| c.set(self.0));
    }
}

pub(crate) fn z_intent_active() -> bool {
    Z_INTENT.with(|c| c.get().is_some())
}

/// 沉底兜底:从栅栏向上能遇宿主才归位。owned popup 由宿主管理下界;
/// 此路径处理已经落地的异常,只选受限普通带锚点,没有 topmost 转换。
/// 无状态锁,可在窗口过程直接调用。
pub(crate) fn fence_reanchor_if_below_host(hwnd: HWND) {
    let Some(shell) = desktop_shell_window() else {
        return;
    };
    if shell == hwnd {
        return;
    }
    let mut w = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
    for _ in 0..400 {
        if w.0 == 0 {
            return;
        }
        if w == shell {
            let Some(after) = band_attach_anchor(shell, hwnd) else {
                return;
            };
            let _z = z_scope(ZIntent::Repair);
            let attempt = unsafe {
                SetWindowPos(
                    hwnd,
                    after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
            if let Err(e) = attempt {
                bad_anchor_mark(after);
                log(&format!(
                    "z-guard: re-anchor FAILED after=0x{:x} err={e:?} (anchor blacklisted)",
                    after.0
                ));
                return;
            }
            bad_anchor_clear(after);
            log(&format!(
                "z-guard: fence re-anchored above host after external move (after=0x{:x})",
                after.0
            ));
            return;
        }
        w = unsafe { GetWindow(w, GW_HWNDPREV) };
    }
}

// ---- 全局 z 序事件触发的高速自检 ----
// 外部 SHOW/HIDE/REORDER/MINIMIZE 事件只作触发器;合并后在 UI 线程实查。
// SKIPOWNPROCESS 避免自家修复产生的事件再次触发自检。
pub(crate) static ZCHECK_PENDING: AtomicBool = AtomicBool::new(false);
pub(crate) static ZORDER_HOOKS: std::sync::OnceLock<(HWINEVENTHOOK, HWINEVENTHOOK)> =
    std::sync::OnceLock::new();

unsafe extern "system" fn zorder_event_cb(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _idobject: i32,
    _idchild: i32,
    _idthread: u32,
    _time: u32,
) {
    // 批量事件只投递一条消息,回调保持 O(1)。
    if !ZCHECK_PENDING.swap(true, Ordering::Relaxed) {
        let tray = TRAY_HWND.get().copied().unwrap_or(HWND(0));
        if tray.0 != 0 {
            let _ = PostMessageW(tray, WM_DL3_ZCHECK, WPARAM(0), LPARAM(0));
        } else {
            ZCHECK_PENDING.store(false, Ordering::Relaxed);
        }
    }
}

/// 两组全局事件钩子覆盖桌面切换双向。LOCATIONCHANGE 过热,不采用。
pub(crate) fn install_zorder_hooks() {
    let h1 = unsafe {
        SetWinEventHook(
            0x0016,
            0x0017,
            HMODULE(0),
            Some(zorder_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let h2 = unsafe {
        SetWinEventHook(
            0x8002,
            0x8004,
            HMODULE(0),
            Some(zorder_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if h1.0 == 0 || h2.0 == 0 {
        log("z-guard: winevent hook install failed");
    } else {
        let _ = ZORDER_HOOKS.set((h1, h2));
    }
}

/// 高速自检:沉底立即重挂;外来可见窗位于栅栏下方时限速下压。
/// 收集 HWND 与移动分两步,不持状态锁调用窗口定位。
static LOWER_RATE_MS: u64 = 600;
static LAST_LOWER_MS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn zcheck_fences_now() {
    let (hwnds, menu_host, tray) = {
        let s = state().lock().unwrap();
        (
            s.windows.values().copied().collect::<Vec<HWND>>(),
            MENU_HOST_HWND.get().copied(),
            TRAY_HWND.get().copied(),
        )
    };
    // 只有实际下压才消耗额度,避免杂散事件挡住真正的恢复过渡。
    let now = resize_now_ms();
    let last = LAST_LOWER_MS.load(Ordering::Relaxed);
    let may_lower = last == 0 || now.saturating_sub(last) >= LOWER_RATE_MS;
    let mut acted = false;
    for h in hwnds {
        fence_reanchor_if_below_host(h);
        if may_lower && fence_lower_if_blocked(h, &menu_host, &tray) {
            acted = true;
        }
    }
    if acted {
        LAST_LOWER_MS.store(resize_now_ms(), Ordering::Relaxed);
    }
}

/// 从宿主向上先遇栅栏即健康;先遇可见外来窗则使用同一受限锚点下压。
/// 阻挡窗紧贴在栅栏下方仍须移动;topmost/坏锚不得直接用于 SetWindowPos。
/// 返回是否实际下压,供全局限速使用。
pub(crate) fn fence_lower_if_blocked(
    hwnd: HWND,
    menu_host: &Option<HWND>,
    tray: &Option<HWND>,
) -> bool {
    let Some(shell) = desktop_shell_window() else {
        return false;
    };
    if shell == hwnd {
        return false;
    }
    let vs = virtual_screen_rect();
    let own: Vec<HWND> = state().lock().unwrap().windows.values().copied().collect();
    let mut w = unsafe { GetWindow(shell, GW_HWNDPREV) };
    for _ in 0..400 {
        if w.0 == 0 || w == hwnd {
            return false;
        }
        if own.contains(&w) || band_invisible(w, &vs) || band_aux(w, *menu_host, *tray) {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        // 已有 blocker,不再以反向邻接检查把紧贴其上的栅栏放过。
        // resolver 遇坏锚只能回到安全垫窗;topmost 只作终止边界。
        let Some(after) = band_attach_anchor(shell, hwnd) else {
            return false;
        };
        let _z = z_scope(ZIntent::Repair);
        let attempt = unsafe {
            SetWindowPos(
                hwnd,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
        if let Err(e) = attempt {
            bad_anchor_mark(after);
            log(&format!(
                "z-guard: fence lower FAILED below 0x{:x} err={e:?} (anchor blacklisted)",
                after.0
            ));
            return false;
        }
        bad_anchor_clear(after);
        log(&format!(
            "z-guard: fence lowered below anchor 0x{:x} (blocker=0x{:x})",
            after.0, w.0
        ));
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{resolve_band_anchor, walk_repair_due, AnchorWindow, WalkFault};

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
    fn blocked() -> WalkFault {
        WalkFault::Blocked { hwnd: 3, class: 4 }
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

    #[test]
    fn repeated_strike_does_not_repeat_repair() {
        for strikes in [3, 13, 23] {
            assert!(walk_repair_due(blocked(), strikes, true));
            assert!(!walk_repair_due(blocked(), strikes, false));
        }
    }

    #[test]
    fn blocked_repair_observes_confirmation_and_backoff() {
        for strikes in [0, 1, 2, 4, 12, 14, 22, 24] {
            assert!(!walk_repair_due(blocked(), strikes, true));
        }
    }

    #[test]
    fn sunk_fence_repairs_immediately_but_budget_exhaustion_does_not() {
        assert!(walk_repair_due(WalkFault::NotFoundTop, 1, false));
        assert!(!walk_repair_due(WalkFault::NotFoundBudget, 3, true));
    }

    /// 手编 UTF-16 类名须与分类面板实际注册名一致。
    #[test]
    fn cats_panel_class_encoding_matches_registered_name() {
        let expect: Vec<u16> = "DeskFenceCatsPanel".encode_utf16().collect();
        assert_eq!(super::CATS_PANEL_CLASS.to_vec(), expect);
    }
}
