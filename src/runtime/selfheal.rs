//! 自愈/z 序子系统:走查、意图守卫、WinEvent 与快速下压。
//! band 锚点解析与走查判据已迁 hosts.rs(2026-09-17,断与 present 的互相
//! 依赖),经 `use crate::hosts::*` 消费;栅栏由桌面宿主拥有;健康窗口不
//! 重排,不使用 topmost 免疫。
//! 与 ui.rs 双向依赖(同 crate 内合法)。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
// windows crate 未导出的 WinEvent 标志(0.62 仍缺),按 WinUser.h 补定义
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
use crate::hosts::*;
use crate::logging::log;
use crate::present::*;
use crate::render;
use crate::state::*;
use crate::winids::*;
use windows::Win32::UI::WindowsAndMessaging::*;
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

/// 窗口类名哈希(FNV-1a over UTF-16):日志里匿名化外来窗口类名用。
/// (2026-09-16 提 pub 供 tests/ 断言确定性/区分度)
pub fn class_hash(cls: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in cls {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
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
            .filter(|(_, h)| !unsafe { IsWindow(Some(**h)).as_bool() })
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
            // SAFETY(走查循环): GetWindow 沿 z 链同步取现存窗口（无指针
            // 参数），失败得 null 由判空终止；cls_buf/dr 为栈缓冲。
            let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) }.unwrap_or_default();
            let mut budget = 0usize;
            for _ in 0..1000 {
                if w.0.is_null() {
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
                    w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
                    continue;
                }
                let mut cls_buf = [0u16; 32];
                let n = unsafe { GetClassNameW(w, &mut cls_buf) };
                out_of_band = true;
                blocker = (w.0 as isize, class_hash(&cls_buf[..n.max(0) as usize]));
                if to_move.is_empty() {
                    let mut dr = RECT::default();
                    let _ = unsafe { GetWindowRect(w, &mut dr) };
                    let own = s.windows.values().any(|v| *v == w);
                    log(&format!(
                        "walk-break: fence {id} host=0x{:x} blocked by cls={} own={} h=0x{:x} rect=({},{})-({},{}) vis={} iconic={}",
                        host.hwnd.0 as usize,
                        String::from_utf16_lossy(&cls_buf[..n.max(0) as usize]),
                        own, w.0 as usize, dr.left, dr.top, dr.right, dr.bottom,
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
                        host.hwnd.0 as usize, budget, why, strikes_n
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
                    // SAFETY: h 是本进程栅栏窗口（walk 收集自 state.windows）；
                    // z_scope 声明自家恢复意图，放行 z 守卫。
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
                    // SAFETY: h 是本进程栅栏窗口；after 是 resolver 给出的
                    // 安全锚点（绝不 topmost/坏锚）；z_scope(ZIntent::Repair)
                    // 声明自家修复；NOMOVE|NOSIZE=纯 z 移动。
                    let attempt = unsafe {
                        SetWindowPos(
                            h,
                            Some(after),
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
                                    after.0 as usize
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
                            id, h.0 as usize, after.0 as usize, e
                        ));
                    }
                }
                if attached {
                    s.attached.insert(*id);
                    moved.push(*id);
                    // 修复后的局部遮挡诊断(64 步、矩形相交),不参与带位判定。
                    // SAFETY: 走查链上同步查询，契约同上（栈缓冲/纯句柄调用）。
                    let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) }.unwrap_or_default();
                    for _ in 0..64 {
                        if w.0.is_null() || w == h {
                            break;
                        }
                        let mut wr = RECT::default();
                        let _ = unsafe { GetWindowRect(w, &mut wr) };
                        if band_invisible(w, &vs) || band_aux(w, host1, trayw) {
                            w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
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
    // SAFETY(走查链): GetWindow 同步向上取现存窗口，判空终止。
    let mut w = unsafe { GetWindow(hwnd, GW_HWNDPREV) }.unwrap_or_default();
    for _ in 0..400 {
        if w.0.is_null() {
            return;
        }
        if w == shell {
            let Some(after) = band_attach_anchor(shell, hwnd) else {
                return;
            };
            let _z = z_scope(ZIntent::Repair);
            // SAFETY: hwnd 是本进程栅栏窗口；after 是受限锚点（resolver
            // 排除宿主/自身/topmost/坏锚）；NOMOVE|NOSIZE=纯 z 修复。
            let attempt = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(after),
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
                    after.0 as usize
                ));
                return;
            }
            bad_anchor_clear(after);
            log(&format!(
                "z-guard: fence re-anchored above host after external move (after=0x{:x})",
                after.0 as usize
            ));
            return;
        }
        w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
    }
}

// ---- 全局 z 序事件触发的高速自检 ----
// 外部 SHOW/HIDE/REORDER/MINIMIZE 事件只作触发器;合并后在 UI 线程实查。
// SKIPOWNPROCESS 避免自家修复产生的事件再次触发自检。
pub(crate) static ZCHECK_PENDING: AtomicBool = AtomicBool::new(false);
pub(crate) static ZORDER_HOOKS: SyncHandle<std::sync::OnceLock<(HWINEVENTHOOK, HWINEVENTHOOK)>> =
    SyncHandle(std::sync::OnceLock::new());

/// # Safety
/// SetWinEventHook 的 OUTOFCONTEXT 回调：系统在**安装钩子的线程**（主线程
/// 消息循环）同步调用；本实现不解引用任何实参（钩子句柄/事件参数全部
/// 忽略），只做原子合并 + PostMessageW（无指针参数），O(1) 快速返回。
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
        let tray = TRAY_HWND
            .get()
            .copied()
            .unwrap_or(HWND(std::ptr::null_mut()));
        if !tray.0.is_null() {
            unsafe {
                let _ = PostMessageW(Some(tray), WM_DL3_ZCHECK, WPARAM(0), LPARAM(0));
            }
        } else {
            ZCHECK_PENDING.store(false, Ordering::Relaxed);
        }
    }
}

/// 两组全局事件钩子覆盖桌面切换双向。LOCATIONCHANGE 过热,不采用。
pub(crate) fn install_zorder_hooks() {
    // SAFETY: zorder_event_cb 是匹配 WINEVENTPROC ABI 的回调；OUTOFCONTEXT
    // 要求回调在安装线程（主线程）执行——由消息循环保证；句柄对存入
    // OnceLock（进程终身不卸载）；SKIPOWNPROCESS 防自家修复自触发。
    let h1 = unsafe {
        SetWinEventHook(
            0x0016,
            0x0017,
            None,
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
            None,
            Some(zorder_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if h1.0.is_null() || h2.0.is_null() {
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
    // SAFETY(走查链): GetWindow 同步向上取现存窗口；判空/遇自身即终止。
    let mut w = unsafe { GetWindow(shell, GW_HWNDPREV) }.unwrap_or_default();
    for _ in 0..400 {
        if w.0.is_null() || w == hwnd {
            return false;
        }
        if own.contains(&w) || band_invisible(w, &vs) || band_aux(w, *menu_host, *tray) {
            w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
            continue;
        }
        // 已有 blocker,不再以反向邻接检查把紧贴其上的栅栏放过。
        // resolver 遇坏锚只能回到安全垫窗;topmost 只作终止边界。
        let Some(after) = band_attach_anchor(shell, hwnd) else {
            return false;
        };
        let _z = z_scope(ZIntent::Repair);
        // SAFETY: hwnd 是本进程栅栏窗口；after 是受限锚点（绝不 topmost/
        // 坏锚）；z_scope(ZIntent::Repair) 声明自家修复；纯 z 移动。
        let attempt = unsafe {
            SetWindowPos(
                hwnd,
                Some(after),
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
                after.0 as usize
            ));
            return false;
        }
        bad_anchor_clear(after);
        log(&format!(
            "z-guard: fence lowered below anchor 0x{:x} (blocker=0x{:x})",
            after.0 as usize, w.0 as usize
        ));
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{walk_repair_due, WalkFault};

    fn blocked() -> WalkFault {
        WalkFault::Blocked { hwnd: 3, class: 4 }
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
}
