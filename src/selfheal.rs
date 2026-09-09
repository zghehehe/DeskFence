//! 自愈/z 序子系统(2026-09-08 从 ui.rs 原样搬出,纯搬家不改行为):
//! 走查(ensure_all_attached)、band 判据、锚点解析、意图守卫(ZIntent/
//! z_scope)、WinEvent 双钩子、zcheck 快速通道、下压、显示桌面 topmost 免疫。
//! 本模块属于 ui.rs 拆分增量;与 ui.rs 双向依赖(同 crate 内合法)。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use windows::Win32::Foundation::{
    HMODULE, HWND, LPARAM, RECT, WPARAM,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
// windows 0.52 未导出的 WinEvent 标志,按 WinUser.h 补定义
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::render;
use crate::drag::*;
use crate::ui::*;

/// 一次走查失位的故障签名。防抖只在"同一签名连续出现"时累计拍数:
/// 恢复过渡期穿过 band 的应用窗每拍都是不同窗口,签名一变就重置计数,
/// 不再误触修复(2026-08-28 Chrome/CabinetWClass 拦截误报即此类);而常驻
/// 拦路者(同 HWND 同类)或沉底故障签名稳定,防抖/退避语义保持不变。
#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkFault {
    /// 可见外来窗先于栅栏出现在宿主之上
    Blocked { hwnd: isize, class: u64 },
    /// 走查到栈顶未找到:栅栏确定在宿主之下(显示桌面批次),首拍即修
    NotFoundTop,
    /// 走查预算耗尽:状态不明,只记日志不动手
    NotFoundBudget,
}

pub(crate) struct WalkStrike {
    fault: WalkFault,
    count: u32,
}

fn class_hash(cls: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in cls {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// SetWindowPos places a window *behind* hWndInsertAfter. Passing WorkerW directly
/// therefore puts the fence below the desktop host and can produce a fully blank
/// desktop after Show Desktop changes WorkerW ordering. Use the window immediately
/// above the host so the fence sits between desktop and normal application windows.
/// 宿主之上没有任何窗口时返回 None(不移动):绝不能回退 HWND_TOP——那会把
/// 栅栏顶到整个 z 栈顶端(2026-08-27 实测三个栅栏被顶到宿主之上 215 层,
/// 即用户看到的"栅栏浮在别的窗口上方")。
pub(crate) fn desktop_insert_after(host: HWND) -> Option<HWND> {
    // 宿主正上方第一个**非坏锚**窗口(UIPI 拒锚窗口做了锚,插入必败;
    // 2026-08-31 WeLink elevated 实测)。坏锚在带底紧贴宿主时沿链向上跳过。
    let mut above = unsafe { GetWindow(host, GW_HWNDPREV) };
    for _ in 0..32 {
        if above.0 == 0 {
            return None;
        }
        if !bad_anchor_recent(above) {
            return Some(above);
        }
        above = unsafe { GetWindow(above, GW_HWNDPREV) };
    }
    None
}

/// 栅栏窗口类名("DeskFenceFence",14 字符)——供无锁判定自家栅栏。
const FENCE_CLASS: [u16; 14] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x46, 0x65, 0x6E, 0x63, 0x65,
];

/// 无锁判定自家栅栏窗口:band_attach_anchor 在窗口过程/持锁的走查修复里
/// 直接调用,不能取状态锁;辅助窗(菜单宿主/托盘)类名不同,不会误判。
pub(crate) fn is_own_fence_window(w: HWND) -> bool {
    let mut cls_buf = [0u16; 16];
    let n = unsafe { GetClassNameW(w, &mut cls_buf) };
    n as usize == FENCE_CLASS.len() && cls_buf[..FENCE_CLASS.len()] == FENCE_CLASS
}

fn is_topmost_window(w: HWND) -> bool {
    // WS_EX_TOPMOST = 0x8
    (unsafe { GetWindowLongW(w, GWL_EXSTYLE) } & 0x8) != 0
}

// ---------------- UIPI 坏锚缓存(2026-08-31) ----------------
// 以高完整性(elevated)进程的窗口为 hWndInsertAfter 会被 UIPI 拒绝
// (0x80070005)。实测案例:WeLinkMeeting 以管理员运行,其会议窗参与
// 桌面切换停泊批落到带内低位后,band_attach_anchor 主规则解析出的
// "最低可见外来窗"正是它 → 5 个栅栏的 repair/下压/re-anchor 全部
// 被拒 → 栅栏持续浮在会议窗上方(浮窗),8s 健康宽限过期后 reconcile
// 放出原生图标(图标重合)。被拒过的 hwnd 缓存一段时间,锚解析绕开;
// 成功插入即清除。TTL 兜底句柄复用风险。
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

/// 带内就位锚点(2026-08-29 修"菜单后点桌面闪屏"根因,勿回退):返回栅栏
/// 应插到"其正下方"的窗口。旧实现=宿主正上方(带底,z 序 1-5 步)——那是
/// 菜单开合/IME/辅助窗的底层扰动区:zwatch 60ms 实测(12:33:31.469),
/// 菜单关闭时系统把菜单宿主连同其 z 邻居(=紧贴带底的栅栏簇)整帧静默
/// 沉到宿主之下(不发 CHANGING/CHANGED,否决无从下手),高速自检再整链
/// 拉回=栅栏消失 0.1-0.8s=菜单后点空白的轻微闪。日志指纹:track
/// dismissed 后紧跟 5 条 re-anchored。规则(分两档):
/// 主规则(所有调用方):从宿主向上按走查同源容忍集(隐形/辅助/自家栅栏/
/// topmost 全跳过)找到第一个"可见且非 topmost 的外来窗"L,锚定 L 正下方。
/// 应用态 L=最低可见应用窗(~380 层深位,数百层垃圾与带底扰动区绝缘)。
/// topmost 跳过的原因:带内大量 topmost 风格隐形翻转垃圾(Outlook ATL/
/// tooltip、SPES ScW),活跃瞬间冒充最低可见窗;曾试"以 topmost 为界下探
/// 到非 topmost 窗",终点是不受过滤保护的 MSCTFIME UI(IME 翻转窗),
/// 锚它=留在扰动区(第一版实踩,下探已删)。
/// 深位回退(仅晋升路径 deep=true,显示桌面态):主规则找不到 L 时,锚定
/// "最低的可见或 topmost 外来窗"正下方(本机显示态=ScW 钩子层群底部,
/// ~30 步)——低于一切可见窗=不浮窗,且隔 20+ 层隐形垃圾离开菜单宿主的
/// 停泊扰动区。只在晋升(1s 走查节拍、状态已稳定)启用,不在快速
/// re-anchor/创建/修复路径用:过渡期窗口可见性闪烁瞬间误判会把栅栏锚到
/// topmost 群之下=浮到应用窗上(应用态 ScW 在 ~440 层,高于 Chrome)。
/// 最后回退:健康兄弟栅栏正下方(归队)→宿主正上方(旧行为)。锚点绝不
/// 能是宿主本身(会把栅栏放到壁纸后面)或 HWND_TOP(会浮顶)。
/// UIPI 坏锚降级(2026-08-31):主规则候选若在坏锚缓存(被 0x80070005
/// 拒过,elevated 进程窗口)→改插它 GW_HWNDNEXT 下方窗口之下=栅栏落到
/// 坏锚之下,绝不遮挡;下方无可垫窗才走兄弟归队/带底。
pub(crate) fn band_attach_anchor(host: HWND, skip: HWND, deep: bool) -> Option<HWND> {
    let vs = virtual_screen_rect();
    let menu_host = MENU_HOST_HWND.get().copied();
    let tray = TRAY_HWND.get().copied();
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
    for _ in 0..1000 {
        if w.0 == 0 {
            break;
        }
        if w == skip
            || is_own_fence_window(w)
            || is_topmost_window(w)
            || band_invisible(w, &vs)
            || band_aux(w, menu_host, tray)
        {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        // UIPI 坏锚(如 elevated 会议窗)不能插其下方:改插它 GW_HWNDNEXT
        // 方向(更低)的窗口之下,让栅栏落到坏锚之下=绝不遮挡它;沿下方找
        // 可垫窗,全不可用才走兄弟归队/带底回退。绝不能"跳过继续向上"——
        // 那样锚更浅,栅栏还是浮在坏锚上方(浮窗复现)。下方窗口必然非
        // topmost(同一非 topmost 带内,坏锚下方不会再有 topmost)。
        if bad_anchor_recent(w) {
            let mut lower = unsafe { GetWindow(w, GW_HWNDNEXT) };
            let mut tried = 0;
            while lower.0 != 0 && tried < 8 {
                // 排除宿主:锚宿主=栅栏沉到壁纸后面(勿回退)。
                if lower != host
                    && lower != skip
                    && !is_topmost_window(lower)
                    && !is_own_fence_window(lower)
                    && !bad_anchor_recent(lower)
                {
                    return Some(lower);
                }
                lower = unsafe { GetWindow(lower, GW_HWNDNEXT) };
                tried += 1;
            }
            break; // 坏锚下方无可垫窗:走兄弟归队/带底(栅栏在宿主正上方=坏锚之下)
        }
        return Some(w);
    }
    // 深位回退(2026-09-08 起无条件执行,deep 参数保留兼容):主规则无
    // "可见且非 topmost"外来窗时(典型:桌面态全部应用窗最小化),锚到
    // 非 topmost 带深处"最高的非 topmost 隐形外来窗"(紧贴首个可见非
    // topmost 窗或垃圾带顶)。此处 ~440 步深位远离菜单宿主的静默沉底
    // 块=菜单开合免疫(实测);旧的兄弟归队/带底回退会把栅栏放回宿主
    // 正上方扰动区,菜单一关就被整块压到宿主之下=用户可见的"桌面大闪"
    // (bandwalk 实证)。
    // 可见 topmost 外来窗不终结搜索(2026-09-09,勿回退):SPES epc_pxs
    // 的 ScW 全屏截屏钩子层会短暂漂进宿主正上方浅位,旧版在"首个可见
    // 窗(含 topmost)"处 break,ScW 浅位值班时锚被短路到 depth 11-15 的
    // 沉底块边缘隐形窗(SoBS_Hint 实抓),栅栏被锚进菜单宿主静默沉底块,
    // 菜单关闭即整块沉底+60ms 拉回=可见闪(当日 216 次 re-anchor 实证)。
    // 跳过它们后锚点与钩子层位置彻底无关;主规则已保证走到本回退时
    // 无"可见非 topmost"外来窗,跳过无遮挡风险。
    // 锚必须自身非 topmost:插到 topmost 窗正下方会把栅栏并入 topmost band
    // (2026-08-29 实测 5 栅栏全变 topmost=True;且 SetWindowLongW 清不掉
    // 该位,HWND_NOTOPMOST 又会把窗口移到非 topmost 带顶部=位置不可控,
    // 此路不通,勿再试)。无可垫垃圾则继续兄弟归队/带底。
    {
        let _ = deep; // 参数保留:历史上仅晋升路径选择深位,现统一启用
        let mut best: Option<HWND> = None;
        let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
        for _ in 0..1000 {
            if w.0 == 0 {
                break;
            }
            if w == skip || is_own_fence_window(w) || band_aux(w, menu_host, tray) {
                w = unsafe { GetWindow(w, GW_HWNDPREV) };
                continue;
            }
            if band_invisible(w, &vs) {
                if !is_topmost_window(w) {
                    best = Some(w);
                }
                w = unsafe { GetWindow(w, GW_HWNDPREV) };
                continue;
            }
            if is_topmost_window(w) {
                // 可见 topmost 外来窗(ScW 截屏钩子层类)跳过,不终结搜索:
                // 见函数头注释(2026-09-09 浅位短路闪屏修复)。
                w = unsafe { GetWindow(w, GW_HWNDPREV) };
                continue;
            }
            break; // 首个可见非 topmost 外来窗到顶:锚其下方垫窗,绝不遮挡
        }
        if best.is_some() {
            return best;
        }
    }
    // 走完预算仍无可用外来窗:优先归队到带内最低的兄弟栅栏之下,保持
    // 集群;没有兄弟才回退带底。
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
    for _ in 0..1000 {
        if w.0 == 0 {
            break;
        }
        if w != skip && is_own_fence_window(w) {
            return Some(w);
        }
        w = unsafe { GetWindow(w, GW_HWNDPREV) };
    }
    desktop_insert_after(host)
}

// ---------------- 窗口生命周期 ----------------

// ---------------- band 走查共享判据 ----------------
// 主走查、肇事扫描、z-guard 快速通道三处必须用同一套"可忽略窗口"语义,
// 2026-08-28 抽取为单一来源(此前走查内部即有两份复制粘贴)。

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
// EdgeUiInputTopWndClass:系统触摸/输入边缘条,band 内常驻半透明,只覆盖
// 屏幕边角几像素且自身透明,不可能视觉遮挡桌面内容。它在前后台切换
//(菜单开合)时会上下漂移穿过我们的 band,按 band 原生系统窗容忍。
const EDGEUI_CLASS: [u16; 22] = [
    0x45, 0x64, 0x67, 0x65, 0x55, 0x69, 0x49, 0x6E, 0x70, 0x75, 0x74, 0x54, 0x6F, 0x70, 0x57,
    0x6E, 0x64, 0x43, 0x6C, 0x61, 0x73, 0x73,
];

/// IME 候选/状态窗("MSCTFIME UI")与其线程宿主("Default IME")。2026-08-29
/// 实测:MSCTFIME UI 的可见性/矩形随输入焦点振荡(空闲时 0x0 矩形隐藏,
/// 活跃瞬间在带内"live"),稳稳钉在宿主正上方几层——若当可见外来窗处理,
/// band_attach_anchor 的锚点会被它钉死在带底 churn 区(Win+D 后栅栏永远
/// 晋升不出去=菜单关闭闪屏不愈);它零像素/瞬态,不可能视觉遮挡桌面内容,
/// 与 EdgeUi 输入条同类,按 band 原生系统窗容忍。
const MSCTFIME_CLASS: [u16; 11] = [
    0x4D, 0x53, 0x43, 0x54, 0x46, 0x49, 0x4D, 0x45, 0x20, 0x55, 0x49,
]; // "MSCTFIME UI"
const DEFAULT_IME_CLASS: [u16; 11] = [
    0x44, 0x65, 0x66, 0x61, 0x75, 0x6C, 0x74, 0x20, 0x49, 0x4D, 0x45,
]; // "Default IME"
// 搜狗输入法 TSF 基础设施窗组(2026-09-09):与 MSCTFIME UI / Default IME
// 同法理——每个有焦点的应用进程头上挂一组(IME/SoImeBS_TSF_UI/SoBS_UI/
// SoBS_Hint),空闲 0x0 隐形,可见性随输入焦点毫秒级闪现;闪现瞬间被
// band_attach_anchor 主规则当成"最低可见外来窗"锚定(实抓 0x20488),
// 栅栏被拉到 depth 11-15 的菜单沉底块邻域,菜单关闭即沉底拉回=可见闪
// (当日 216 次 re-anchor 实证)。零像素/瞬态不可能遮挡桌面内容,容忍。
const SOGOU_IME_CLASS: [u16; 3] = [0x49, 0x4D, 0x45]; // "IME"
const SOIME_TSF_CLASS: [u16; 14] = [
    0x53, 0x6F, 0x49, 0x6D, 0x65, 0x42, 0x53, 0x5F, 0x54, 0x53, 0x46, 0x5F, 0x55, 0x49,
]; // "SoImeBS_TSF_UI"
const SOBS_UI_CLASS: [u16; 7] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x55, 0x49]; // "SoBS_UI"
const SOBS_HINT_CLASS: [u16; 9] = [0x53, 0x6F, 0x42, 0x53, 0x5F, 0x48, 0x69, 0x6E, 0x74]; // "SoBS_Hint"

const CATS_PANEL_CLASS: [u16; 18] = [
    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x43, 0x61, 0x74,
    0x73, 0x50, 0x61, 0x6E, 0x65, 0x6C,
]; // "DeskFenceCatsPanel"

/// band 走查的"不可见"判据:隐藏/最小化/离屏/退化尺寸(≤2px,GDI+ 钩子与
/// 锁屏残留 CoreWindow 常以 1x1@0,0 插队,实际遮不住)/cloaked(visible
/// 位有效但 DWM 不合成)。
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

/// band 走查的"自有辅助窗/系统 band 窗"判据:菜单宿主/托盘窗/#32768 弹层/
/// Shell_TrayWnd(自动隐藏任务栏转换瞬态)/EdgeUi 输入条。
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
        || (n == 3 && cls_buf[..3] == SOGOU_IME_CLASS)
        || (n == 14 && cls_buf[..14] == SOIME_TSF_CLASS)
        || (n == 7 && cls_buf[..7] == SOBS_UI_CLASS)
        || (n == 9 && cls_buf[..9] == SOBS_HINT_CLASS)
        // 分类管理面板(2026-09-08):自有辅助窗,可见时可压在栅栏区域上,
        // 不容忍的话面板一开=全栅栏 walk-break→3 拍后 z-chain repair
        // 整面重排=用户可见闪(run.log 17:36:48 实锤,与菜单宿主同款待遇)
        || (n == 18 && cls_buf[..18] == CATS_PANEL_CLASS)
}

/// band 自愈走查(global_tick 每 tick 调用):清理已销毁窗口→缺失窗口
/// 延迟到宿主就绪后创建→逐栅栏从宿主向上核对带位(失位按 WalkFault
/// 签名防抖:连续 3 拍才修、沉底首拍即修、预算耗尽只记日志不动)→
/// 健康但滞留带底 churn 区的浅位栅栏一次性晋升到解析锚点(最低可见非
/// topmost 外来窗)之下。修复动作纯 z(SWP_NOMOVE|SWP_NOSIZE),不碰位置。
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
        // 3) z 序重申:每个存活窗口重新插到其宿主之后(防漂移/Explorer 重建自愈)。
        //    先做整链检查:宿主上方窗口之下恰好是全部栅栏(顺序不限)则视为
        //    已就位,跳过所有 SetWindowPos——对分层窗口,即使参数相同的
        //    SetWindowPos 也会触发 DWM 重新合成;每秒的"洗牌式重申"在
        //    前台 band 变化(菜单交互)后会变成真实 z 移动,表现为
        //    栅栏区域整体闪一下(表面内容并没有变)。
        // z 序自愈(逐栅栏按需,2026-08-27 终版):
        // 对每个栅栏,从其宿主向上(GW_HWNDPREV)走,直到命中该栅栏:
        // - 命中 → 该栅栏就位,不动它(SetWindowPos 同位也触发 DWM 重合成=闪);
        // - 自有辅助窗(菜单宿主/托盘窗/#32768 弹层)与一切"不可见"窗
        //   (最小化/隐藏/矩形与虚拟屏幕不相交,含最小化沉底的 Chrome、
        //   第三方软件离屏 CoreWindow)→ 跳过继续;
        // - 自动隐藏任务栏(Shell_TrayWnd)在隐藏/弹出转换时会短暂沉到桌面
        //   层,但弹起时本就在顶层且矩形(y≥任务栏)与栅栏(y≤内容区)不相交,
        //   做空间相交测试后自然跳过;
        // - 外来**可见且与该栅栏矩形相交**的窗口先于栅栏出现 → 该栅栏真被
        //   遮挡,单独移回宿主之后(只动这一个)。
        // 教训(勿回退):旧"单次遍历收集+不在集内就修"的写法,会在窗口沉到
        // 栅栏包下方时提前折断,把"栅栏其实都在它上面"误判成"全部失位",
        // 全量 SetWindowPos=每次菜单交互都闪。
        let trayw = TRAY_HWND.get().copied();
        let host1 = MENU_HOST_HWND.get().copied();
        let vs = virtual_screen_rect();
        let mut to_move: Vec<u32> = Vec::new();
        let mut all_healthy = true;
        for (id, h) in s.windows.clone() {
            // 被拖栅栏拖拽期间提升到最高兄弟栅栏之上(band 内,见 handle_mousemove),
            // 自愈豁免;拖拽结束由 handle_lbuttonup 归位底带
            if matches!(&s.drag, Some(d) if d.fence_id == id) {
                s.walk_strikes.remove(&id);
                s.walk_strike_ms.remove(&id);
                s.attached.insert(id);
                continue;
            }
            // 显示桌面态 topmost 免疫:免疫栅栏(topmost 化)天然健康,
            // 走查不得把它"修复"回带内(否则与免疫模式互殴)。
            if SHOWN_TOPMOST.load(Ordering::Relaxed) && is_topmost_window(h) {
                s.walk_strikes.remove(&id);
                s.walk_strike_ms.remove(&id);
                s.last_healthy_ms.insert(id, resize_now_ms());
                s.attached.insert(id);
                continue;
            }
            let Some(fence) = s.fences.iter().find(|f| f.id == id) else {
                continue;
            };
            // 提前拷出:健康分支后半段有对 s 的可变借用(MutexGuard 不能字段分裂)
            let fence_hidden = fence.hidden;
            let Some(host) = host_for_rect(&fence.rect, &hosts) else {
                continue; // 无宿主:不动窗口,保持原 z 位
            };
            // 不变式:从宿主向上,只允许出现(可跳过的)不可见窗/自有辅助窗,
            // 然后就是本栅栏。途中撞上任何**可见且在屏内**的外来窗口还没
            // 找到栅栏 → 栅栏已离开桌面 band(浮在真实窗口上方),必须拉回。
            // 注意不是"是否与栅栏相交":不重叠的外来窗口同样说明栅栏出带
            // (2026-08-27 实测:被顶到宿主之上 215 层的栅栏因下方窗口不与
            // 它相交而被旧判定放行=持续浮窗)。
            let mut out_of_band = false;
            let mut found = false;
            let mut blocker = (0isize, 0u64);
            // 预算要能覆盖"栈内大量不可见垃圾窗垫在中间"的现实:不少软件会把
            // 辅助窗 HWND_BOTTOM 沉底,一层层垫在宿主与栅栏之间(2026-08-28
            // 实测单日累积 ~369 层隐形垃圾)。预算耗尽与到顶都
            // 是失位,但报文要区分(勿回退到 320——垃圾层只会更多)。
            let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
            let mut budget = 0usize;
            for _ in 0..1000 {
                if w.0 == 0 {
                    budget = usize::MAX; // 真到顶
                    break;
                }
                budget += 1;
                if w.0 == 0 {
                    break;
                }
                if h == w {
                    found = true;
                    break; // 栅栏就位
                }
                // 其他自家栅栏:正常(整包连续排在底带),跳过继续找自己
                if s.windows.values().any(|v| *v == w) {
                    w = unsafe { GetWindow(w, GW_HWNDPREV) };
                    continue;
                }
                let invisible = band_invisible(w, &vs);
                if invisible {
                    w = unsafe { GetWindow(w, GW_HWNDPREV) };
                    continue;
                }
                let mut cls_buf = [0u16; 32];
                let n = unsafe { GetClassNameW(w, &mut cls_buf) };
                if band_aux(w, host1, trayw) {
                    w = unsafe { GetWindow(w, GW_HWNDPREV) };
                    continue;
                }
                // 可见在屏内外来窗口先于栅栏出现:栅栏出带
                out_of_band = true;
                blocker = (w.0, class_hash(&cls_buf[..n.max(0) as usize]));
                if to_move.is_empty() {
                    let mut db = [0u16; 32];
                    let dn = unsafe { GetClassNameW(w, &mut db) };
                    let mut dr = RECT::default();
                    let _ = unsafe { GetWindowRect(w, &mut dr) };
                    // 身份点名:w 是否在我方窗口表里(排除孤儿同类窗干扰),
                    // 句柄一并打印供跨 tick 对账。
                    let own = s.windows.values().any(|v| *v == w);
                    log(&format!(
                        "walk-break: fence {id} host=0x{:x} blocked by cls={} own={} h=0x{:x} rect=({},{})-({},{}) vis={} iconic={}",
                        host.hwnd.0,
                        String::from_utf16_lossy(&db[..dn.max(0) as usize]),
                        own,
                        w.0,
                        dr.left, dr.top, dr.right, dr.bottom,
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
                // 防抖(勿回退):连续三拍**同签名**失位才动手;签名一变
                //(拦路者换窗/类型变化=过路者)立即重置。退避保持第 3、13、
                // 23…拍出手,防"每秒全链 SetWindowPos"复活成周期闪屏源。
                // 限速:同秒内的重复走查(global_tick 双调用)只推一拍。
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
                // 沉底(栈顶未找到)确定非瞬态,首拍即修;预算耗尽状态不明,
                // 只记日志;被拦截走三拍防抖。
                let attempt = match fault {
                    WalkFault::NotFoundTop => true,
                    WalkFault::NotFoundBudget => false,
                    WalkFault::Blocked { .. } => {
                        strikes_n == 3 || (strikes_n > 3 && (strikes_n - 3) % 10 == 0)
                    }
                };
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
                        host.hwnd.0,
                        budget, why, strikes_n
                    ));
                }
                if attempt {
                    to_move.push(id);
                } else if strikes_n == 1 {
                    log(&format!("walk-break: fence {id} flagged strike 1/3, waiting confirm"));
                }
            } else {
                s.walk_strikes.remove(&id);
                s.walk_strike_ms.remove(&id);
                s.last_healthy_ms.insert(id, resize_now_ms());
                s.attached.insert(id);
                // iconic 兜底:漏网的路径可能把栅栏最小化,走查找到了也
                // 不等于可渲染(repair 的纯 z SetWindowPos 取消不了最小化,
                // 必须走 ShowWindow)。
                if unsafe { IsIconic(h) }.as_bool() {
                    let _z = z_scope(ZIntent::Restore);
                    unsafe {
                        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                    }
                    log(&format!("walk: fence {id} was iconic, restored"));
                }
                // 注意:此处健康栅栏不做任何"粘底"重申(深漂移是安全位)。
                // 出带修复的落点在上方 repair 分支:band_attach_anchor=
                // 最低可见外来窗正下方(2026-08-29),同样不做带底粘底。
                //
                // churn 区晋升(2026-08-29,勿回退):显示桌面(Win+D)后,
                // 高速拉回只能锚到过渡瞬间的中途态窗口(沉底扫动未完成时
                // 某个仍 live 的窗,两秒后它自己隐掉),栅栏常被留在宿主
                // 正上方 1-10 层=菜单开合/IME/辅助窗静默沉底的扰动区
                // ("菜单关闭后点桌面空白闪屏"的根因)。应用态无需晋升
                // (恢复扫动会把栅栏一路托到最低应用窗之下);显示态无人
                // 托底,由走查在健康后一次性晋升到 band_attach_anchor 的
                // 绝缘位。仅当仍在 churn 区(≤12 层)且目标位显著更高
                // (滞后 6 层防边界抖动;曾用 20——会把"从带底送入 parked 应用
                // 窗之下十几层"的机会挡掉,2026-08-29 12:22:58 实测)才动——
                // 深位健康栅栏绝不重排
                // (重排本身=重合成闪)。
                if !fence_hidden {
                    let mut in_churn = false;
                    let mut depth = 0usize;
                    {
                        let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
                        for _ in 0..12 {
                            if w.0 == 0 {
                                break;
                            }
                            if w == h {
                                in_churn = true;
                                break;
                            }
                            w = unsafe { GetWindow(w, GW_HWNDPREV) };
                            depth += 1;
                        }
                    }
                    if in_churn {
                        // 诊断(限频 30s,显示态常驻带底会持续命中):晋升未
                        // 发生时把判定中间量留在日志里
                        static LAST_PROMOTE_DIAG: AtomicU64 = AtomicU64::new(0);
                        let now_ms = resize_now_ms();
                        let diag =
                            now_ms.saturating_sub(LAST_PROMOTE_DIAG.load(Ordering::Relaxed)) > 30000;
                        if diag {
                            LAST_PROMOTE_DIAG.store(now_ms, Ordering::Relaxed);
                        }
                        if let Some(a) = band_attach_anchor(host.hwnd, h, true) {
                            if a != h {
                                let mut d2 = 0usize;
                                let mut target_far = false;
                                let mut w2 = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
                                for _ in 0..1000 {
                                    if w2.0 == 0 {
                                        break;
                                    }
                                    if w2 == a {
                                        // 双重门:目标比当前深(滞后防抖)且目标
                                        // 自身已脱离 churn 区(>12 层)。后者防
                                        // "锚点在底部簇内穿插"的自循环——每次
                                        // 晋升都还在 churn 区里,下个 tick 又升
                                        // =每秒一次 z 移动的振荡(2026-08-29
                                        // 12:48 实测 promote 风暴)。
                                        target_far = d2 + 1 > depth + 6 && d2 + 1 > 12;
                                        break;
                                    }
                                    w2 = unsafe { GetWindow(w2, GW_HWNDPREV) };
                                    d2 += 1;
                                }
                                if diag {
                                    let mut cb = [0u16; 32];
                                    let cn = unsafe { GetClassNameW(a, &mut cb) };
                                    log(&format!(
                                        "promote-diag: fence {id} depth={} anchor=0x{:x} cls={}(n={}) inv={} aux={} target_depth={} far={}",
                                        depth + 1,
                                        a.0,
                                        String::from_utf16_lossy(&cb[..cn.max(0) as usize]),
                                        cn,
                                        band_invisible(a, &vs),
                                        band_aux(a, host1, trayw),
                                        d2 + 1,
                                        target_far
                                    ));
                                }
                                if target_far {
                                    // 锚点邻域重试:SPES ScW 等高完整性窗作锚报
                                    // 0x80070005,且 ScW 群 8 层连坐,沿链换 3 个
                                    // 穿不过去(2026-08-29 实测 promote-diag:
                                    // SetWindowPos failed on all anchors)。改为
                                    // 从期望锚点向下(更深入绝缘区)/向上各探
                                    // ±8 层找第一个可作锚的窗口——该区间由
                                    // resolver 构造保证全是隐形/辅助/自家窗,
                                    // 位置偏差不影响绝缘语义。
                                    let mut moved = false;
                                    let mut shift_used = 0i32;
                                    for delta in [
                                        0i32, -1, -2, -3, -4, 1, 2, 3, 4, -5, -6, -7, -8, 5, 6,
                                        7, 8,
                                    ] {
                                        let mut cur = a;
                                        let mut ok = true;
                                        for _ in 0..delta.unsigned_abs() {
                                            let next = unsafe {
                                                GetWindow(
                                                    cur,
                                                    if delta < 0 {
                                                        GW_HWNDNEXT
                                                    } else {
                                                        GW_HWNDPREV
                                                    },
                                                )
                                            };
                                            if next.0 == 0 || next == host.hwnd || next == h {
                                                ok = false;
                                                break;
                                            }
                                            cur = next;
                                        }
                                        if !ok || cur == h || is_topmost_window(cur) {
                                            continue;
                                        }
                                        let _z = z_scope(ZIntent::Repair);
                                        let ok2 = unsafe {
                                            SetWindowPos(
                                                h,
                                                cur,
                                                0,
                                                0,
                                                0,
                                                0,
                                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                                            )
                                            .is_ok()
                                        };
                                        if ok2 {
                                            moved = true;
                                            shift_used = delta;
                                            break;
                                        }
                                    }
                                    if moved {
                                        if shift_used != 0 {
                                            log(&format!(
                                                "promote fence {id} anchor shifted {shift_used}"
                                            ));
                                        }
                                        log(&format!(
                                            "z-guard: fence {id} promoted out of churn zone (depth {} -> below 0x{:x})",
                                            depth + 1,
                                            a.0
                                        ));
                                    } else if diag {
                                        log("promote-diag: SetWindowPos failed on all anchors");
                                    }
                                }
                            } else if diag {
                                log(&format!("promote-diag: fence {id} anchor==self",));
                            }
                        } else if diag {
                            log("promote-diag: no anchor resolved");
                        }
                    }
                }
            }
        }
        if !to_move.is_empty() {
            let mut moved: Vec<u32> = Vec::new();
            let mut culprit = String::from("none");
            for id in &to_move {
                let Some(h) = s.windows.get(id).copied() else { continue };
                let frect = s.fences.iter().find(|f| f.id == *id).map(|f| f.rect);
                let Some(frect) = frect else { continue };
                let Some(host) = host_for_rect(&frect, &hosts) else { continue };
                // 纯 z 修复(NOMOVE|NOSIZE):位置由交互/布局路径负责,z 自愈只动
                // 层叠次序。带位移的同值 SetWindowPos 会让 DWM 连无效区一起重算
                //=可感知的重排闪底。
                // 锚点重试:宿主正上方若是高完整性进程的窗口(企业安全软件
                // 钩子层),以其为锚会被拒(0x80070005;2026-08-28 实测 20 次,
                // fence4 因此失踪 3350 拍)。失败沿链向上换锚重试,最多 3 个。
                let _z = z_scope(ZIntent::Repair);
                let mut attached = false;
                let mut first_err = None;
                // 深位锚(2026-09-08):与晋升路径统一——桌面态(全部应用窗最小化)下
                // 浅位回退会把栅栏留在菜单宿主 z 邻接的扰动区,菜单开合的静默
                // 沉底会把整块压到宿主之下=用户可见的"桌面大闪后才出现栅栏"。
                let mut anchor = band_attach_anchor(host.hwnd, h, true);
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
                            // 记坏锚:锚解析(含降级路径)下一拍起绕开它,
                            // 不再撞同一堵 UIPI 墙(WeLink elevated 实测)。
                            bad_anchor_mark(after);
                            if first_err.is_none() {
                                first_err = Some((after, e));
                            }
                            let next = unsafe { GetWindow(after, GW_HWNDPREV) };
                            anchor = if next.0 == 0 { None } else { Some(next) };
                        }
                    }
                }
                if !attached && moved.is_empty() && to_move.len() <= 6 {
                    // 首个失败的实证:错误码+锚点,排查"修复静默无效"专用
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
                    // 记录肇事窗口:该栅栏与宿主之间第一个相交的可见外来窗
                    let mut w = unsafe { GetWindow(host.hwnd, GW_HWNDPREV) };
                    for _ in 0..64 {
                        if w.0 == 0 || w == h {
                            break;
                        }
                        let mut wr = RECT::default();
                        let _ = unsafe { GetWindowRect(w, &mut wr) };
                        if band_invisible(w, &vs) {
                            w = unsafe { GetWindow(w, GW_HWNDPREV) };
                            continue;
                        }
                        if band_aux(w, host1, trayw) {
                            w = unsafe { GetWindow(w, GW_HWNDPREV) };
                            continue;
                        }
                        let fx0 = frect.x.round() as i32;
                        let fy0 = frect.y.round() as i32;
                        let fx1 = fx0 + frect.w.round() as i32;
                        let fy1 = fy0 + frect.h.round() as i32;
                        let overlap =
                            wr.left < fx1 && wr.right > fx0 && wr.top < fy1 && wr.bottom > fy0;
                        if overlap {
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
    // 显示桌面态 topmost 免疫管理(锁已释放,见 shown_topmost_tick)
    shown_topmost_tick();
}

// ---------------- 显示桌面态 topmost 免疫(2026-08-29 终修,勿回退) ----------------
// 机制:ToggleDesktop/三指把栅栏纳入"停泊批"(静默沉底,无法否决),此后
// 每次菜单关闭系统都把批内成员重新停泊=栅栏被拖下再拉回=菜单后点空白
// 闪屏(60ms zwatch 实测:沉底块=栅栏簇+菜单宿主,parked 窗不被波及;
// 2026-09-09 sinkwatch 实锤:块按**线程连续段**分组,被压窗全部 tid=自家
// UI 线程,紧邻外部窗不动)。逐个最小化回桌面的路径不碰停泊批→栅栏不
// 动→不闪(用户 Case B 实测)。topmost 窗口不参与停泊(SPW ScW 钩子层与
// 隐形垃圾丛林在每次切换中纹丝不动)→显示桌面态(无真实内容窗,见
// band_has_live_foreign)给栅栏上 HWND_TOPMOST 获得同款豁免;出现真实
// 内容窗(回应用)立即 HWND_NOTOPMOST,由既有走查/下压机制送回深位。
/// 免疫模式当前是否生效
static SHOWN_TOPMOST: AtomicBool = AtomicBool::new(false);
/// 显示桌面态连续稳定拍数(防过渡期抖动)
static SHOWN_STABLE: AtomicU32 = AtomicU32::new(0);
/// 沉底检测时记录的"待激活免疫"时间戳(0=无)。快速通道不在沉底瞬间
/// 立即上 topmost——那时应用缩小动画还在播,topmost 栅栏会渲染在动画
/// 之上=用户看到"栅栏比桌面先冒出来"(2026-08-29 用户实测);改为等
/// 450ms(动画播完,而人手点开+关闭菜单至少要 1s)后由 zcheck 批量
/// 一次性激活,五个栅栏同帧同现(逐栅栏激活会出现"一个比其他慢很多")。
static SHOWN_PENDING_MS: AtomicU64 = AtomicU64::new(0);

/// WS_EX_LAYERED = 0x8_0000:走分层合成的透明层。钩子层/悬浮提示层的
/// 标志性组合(全屏可见的 epc_pxs ScW 与搜狗 SoBS_Hint 实测全部 layered;
/// 真实内容窗——包括被翻成 topmost 的会议窗——不做 layered 透明合成)。
fn is_layered_window(w: HWND) -> bool {
    (unsafe { GetWindowLongW(w, GWL_EXSTYLE) } & 0x8_0000) != 0
}

/// 大尺寸窗口判定(物理px):宽高均 ≥250。
fn is_large_window(w: HWND) -> bool {
    let mut r = RECT::default();
    let ok = unsafe { GetWindowRect(w, &mut r) }.is_ok();
    ok && (r.right - r.left) >= 250 && (r.bottom - r.top) >= 250
}

/// 真实内容窗判据(2026-09-09 方向1,勿回退):非 layered 且 ≥250×250。
/// 免疫进入/退出只认它——显示桌面态的带内"可见窗"是一群小工具窗
/// (搜狗 SoBS_Status/语言栏 CiceroUIWndFrame/硬件监控浮窗/tooltips)加
/// layered 透明钩子层,逐类名容忍是打不完的地鼠;按几何+合成特征过滤
/// 后它们统统不算"有应用窗",免疫才能在显示态正常激活。两个防护保留:
/// 真实应用窗(≥250 非 layered,含被翻成 topmost 的会议窗,2026-09-04
/// 用户指令"栅栏不允许浮在应用上")照常让免疫退出;小内容窗若真被
/// 用户缩到 <250,免疫误激活期间由退出检查自纠。
fn is_real_app_window(w: HWND) -> bool {
    !is_layered_window(w) && is_large_window(w)
}

/// 全带是否存在"真实内容窗"(=有可见的真实应用窗)。免疫(shown-topmost)
/// 的进入/退出判据。注意与 band_attach_anchor 主规则**不再同源**(2026-09-09):
/// 主规则锚点仍把小可见窗当锚候选(有锚总比无锚好),免疫只认真实内容窗。
fn band_has_live_foreign() -> bool {
    let Some(host) = desktop_shell_window() else {
        return true; // 宿主未知时保守视为有(不开免疫)
    };
    let vs = virtual_screen_rect();
    let mh = MENU_HOST_HWND.get().copied();
    let tr = TRAY_HWND.get().copied();
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
    for _ in 0..1000 {
        if w.0 == 0 {
            break;
        }
        if is_own_fence_window(w)
            || band_aux(w, mh, tr)
            || band_invisible(w, &vs)
        {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        // 只有真实内容窗(非 layered+≥250×250)才算"有应用窗":topmost 小
        // 杂层、layered 透明钩子层(ScW/SoBS_Hint 实测)、非 topmost 小工具
        // 窗(搜狗状态条/语言栏/监控浮窗)一律不算(2026-09-09 方向1)。
        // 非 layered 大窗(含被翻成 topmost 的会议窗)照常算 live,免疫退出
        // 防护保留(2026-09-04 用户指令:栅栏不允许浮在应用上)。
        if !is_real_app_window(w) {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        return true;
    }
    false
}

/// 免疫模式切换:SetWindowPos(HWND_TOPMOST/NOTOPMOST) 是唯一可靠的
/// topmost 位操作方式(SetWindowLongW 改不动,实测)。
fn fence_apply_shown_topmost(on: bool) -> usize {
    let hwnds: Vec<HWND> = state()
        .lock()
        .unwrap()
        .windows
        .values()
        .copied()
        .collect();
    let mut n = 0usize;
    for h in hwnds {
        if !on && !is_topmost_window(h) {
            continue; // 摘除模式:只动真正 topmost 的(避免把已归位栅栏再抬高)
        }
        let after = if on {
            HWND_TOPMOST
        } else {
            // 摘除=直接重归位到最低可见外来窗之下。HWND_NOTOPMOST 会先把
            // 栅栏抬到非 topmost 带顶部=浮在应用窗上再等人压(用户实测
            // "回应用偶现栅栏浮在应用上"的根源),只留作锚解析失败的兜底。
            match desktop_shell_window().and_then(|host| band_attach_anchor(host, h, true)) {
                Some(a) if a != h => a,
                _ => HWND_NOTOPMOST,
            }
        };
        let _z = z_scope(ZIntent::Repair);
        let ok = unsafe {
            SetWindowPos(
                h,
                after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .is_ok();
        if ok {
            n += 1;
        }
    }
    n
}

/// 每秒走查末尾驱动的免疫模式管理:进入需"无可见应用窗"稳定 2 拍,
/// 退出(出现可见应用窗)立即。切换本身在上一拍的过渡动画之后——
/// 首次进入的 TOPMOST 跳变若可感知,再前移到 reanchor 路径(待用户实测)。
fn shown_topmost_tick() {
    if !z_guard_setting() || desktop_state() != "normal" {
        if SHOWN_TOPMOST.swap(false, Ordering::Relaxed) {
            let _ = fence_apply_shown_topmost(false);
            log("shown-topmost: mode off (guard/state)");
        }
        return;
    }
    let shown = !band_has_live_foreign();
    let prev = SHOWN_TOPMOST.load(Ordering::Relaxed);
    if shown {
        let s = SHOWN_STABLE.fetch_add(1, Ordering::Relaxed) + 1;
        if !prev && s >= 2 {
            // 栅栏全部隐藏(zen 瞬态)时不切
            let any_visible = {
                let st = state().lock().unwrap();
                st.fences.iter().any(|f| !f.hidden)
            };
            if any_visible {
                let n = fence_apply_shown_topmost(true);
                SHOWN_TOPMOST.store(true, Ordering::Relaxed);
                SHOWN_PENDING_MS.store(0, Ordering::Relaxed);
                log(&format!("shown-topmost: mode ON ({n} fences immune)"));
            }
        }
    } else {
        SHOWN_STABLE.store(0, Ordering::Relaxed);
        if prev {
            let n = fence_apply_shown_topmost(false);
            SHOWN_TOPMOST.store(false, Ordering::Relaxed);
            log(&format!("shown-topmost: mode OFF ({n} fences back to band)"));
            // 立即触发高速自检:把摘除后高位悬浮的栅栏下压到最低可见
            // 应用窗之下(发生在恢复扫动动画内=被遮蔽;勿改为递归调用
            // ensure_all_attached——走查不可重入)。
            zcheck_fences_now();
        }
    }
}

// ---------------- z 序意图守卫 ----------------

/// 窗口定位意图:标记"自家发起的 z 序/显示操作",让 fence_wndproc 的
/// WM_WINDOWPOSCHANGING 拦截只针对外部改动。区分依据:自家 SetWindowPos/
/// ShowWindow 在 UI 线程同步触发该消息(嵌套在调用栈内);外部进程(Shell
/// 显示桌面/最小化批次)的调用经消息泵派发,到达时意图必为 None——线程
/// 局部即可精确区分,无需跨进程握手(后台线程只做文件 IO,不碰窗口)。
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

/// 外部定位变更后的自检:若窗口被压到桌面宿主之下(显示桌面批次的实际
/// 行为,且该操作不经可否决的 WM_WINDOWPOSCHANGING——2026-08-28 wdprobe
/// 实测 veto 零命中、栅栏在宿主下方 vis=1),立即重挂回宿主正上方,不等
/// 3 拍自愈。判据:从本窗口向上(GW_HWNDPREV)走能遇到宿主=自己在宿主
/// 之下;正常在带内时向上走只会到栈顶。无状态锁,可在窗口过程直接调用。
pub(crate) fn fence_reanchor_if_below_host(hwnd: HWND) {
    let Some(shell) = desktop_shell_window() else { return };
    if shell == hwnd {
        return;
    }
    let mut w = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
    for _ in 0..400 {
        if w.0 == 0 {
            return; // 到顶未遇宿主:窗口在宿主上方,无需处理
        }
        if w == shell {
            // 显示桌面态快速免疫(2026-08-29):沉底时若已无可见应用窗,
            // 记录待激活时间戳,由 zcheck 在 450ms 后批量上 topmost
            // (勿在此立即上——应用缩小动画还在播,栅栏会渲染在动画之上
            // ="栅栏比桌面先出来";误判由 zcheck 的可见窗检查自纠)。
            if z_guard_setting() && desktop_state() == "normal" && !band_has_live_foreign() {
                SHOWN_PENDING_MS.store(resize_now_ms(), Ordering::Relaxed);
            }
            // 锚点=最低可见外来窗正下方(带内绝缘位,2026-08-29;带底=菜单
            // 关闭静默沉底的扰动区,勿回退,详见 band_attach_anchor)。
            // 深位锚(2026-09-08):reanchor 与晋升统一,浅位=菜单静默沉底的扰动区
            let Some(after) = band_attach_anchor(shell, hwnd, true) else { return };
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
// 显示桌面把栅栏压到宿主之下的操作既不发 WM_WINDOWPOSCHANGING 也不发
// WM_WINDOWPOSCHANGED(2026-08-28 两轮 wdprobe 实测:两类拦截零命中),
// 进程内消息通道完全探测不到。改用全局 WinEvent 钩子做触发器:桌面切换
// 必然伴随成批的 HIDE/REORDER/MINIMIZE 事件(事件按窗口属主过滤,
// SKIPOWNPROCESS 会滤掉自家栅栏的 z 事件,所以靠"其他窗口被批量操作"
// 的事件当信号),回调只做原子标记+合并投递,实查在 UI 线程执行
// fence_reanchor_if_below_host——2026-08-28 实测切换后 <165ms 即归位,
// 走查 3 拍自愈全程零参与。
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
    // 桌面切换时该事件成批到达,回调必须 O(1):抢到标记者负责投递一条
    // 合并消息,其余事件全部被吞掉。
    if !ZCHECK_PENDING.swap(true, Ordering::Relaxed) {
        let tray = TRAY_HWND.get().copied().unwrap_or(HWND(0));
        if tray.0 != 0 {
            let _ = PostMessageW(tray, WM_DL3_ZCHECK, WPARAM(0), LPARAM(0));
        } else {
            ZCHECK_PENDING.store(false, Ordering::Relaxed);
        }
    }
}

/// 安装两组全局事件钩子(out-of-context:回调经本线程消息泵派发):
/// ① EVENT_SYSTEM_MINIMIZESTART..END(0x0016-0x0017)
/// ② EVENT_OBJECT_SHOW..REORDER(0x8002-0x8004)
/// 覆盖桌面切换双向:隐藏方向发 MINIMIZESTART/HIDE/REORDER,恢复方向的
/// 窗口重现发 SHOW(2026-08-28 补,缺它恢复过渡完全无触发)。范围取舍:
/// LOCATIONCHANGE(0x800b)正常使用中过于高频(拖动任何窗口即风暴),不采用。
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

/// 高速自检:① 栅栏在宿主之下→立即重挂;② 栅栏上方有可见外来窗→
/// 限速下压(桌面切换过渡期,应用窗被插到低位再逐个升起,栅栏会压在
/// 已渲染窗口上直到走查 3 拍修复=用户看到的"回应用后栅栏浮几秒",
/// 2026-08-28 实测)。收集 HWND 与 SetWindowPos 分两步,不持状态锁嵌套。
static LOWER_RATE_MS: u64 = 600;
static LAST_LOWER_MS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn zcheck_fences_now() {
    // 待激活免疫结算(2026-08-29):沉底后 450ms(应用缩小动画播完,而
    // 人手开+关菜单至少 1s)仍无可见应用窗 → 批量一次性上 topmost,
    // 五个栅栏同帧同现;期间出现可见应用窗则取消(误判自纠)。
    let pend = SHOWN_PENDING_MS.load(Ordering::Relaxed);
    if pend != 0 {
        if band_has_live_foreign() || SHOWN_TOPMOST.load(Ordering::Relaxed) {
            SHOWN_PENDING_MS.store(0, Ordering::Relaxed);
        } else if resize_now_ms().saturating_sub(pend) > 450 {
            SHOWN_PENDING_MS.store(0, Ordering::Relaxed);
            let n = fence_apply_shown_topmost(true);
            SHOWN_TOPMOST.store(true, Ordering::Relaxed);
            SHOWN_STABLE.store(2, Ordering::Relaxed);
            log(&format!("shown-topmost: mode ON ({n} fences immune)"));
        }
    }
    // 免疫模式快速退出(2026-08-29):恢复扫动的第一批 WinEvent 到达时
    // (毫秒级,被扫动动画遮蔽),topmost 栅栏还浮在上升的应用窗上——
    // 立即摘除+重归位,不等 1s 走查(用户实测"回应用偶现浮窗"的主潜伏期)。
    if SHOWN_TOPMOST.load(Ordering::Relaxed) && band_has_live_foreign() {
        let n = fence_apply_shown_topmost(false);
        SHOWN_TOPMOST.store(false, Ordering::Relaxed);
        SHOWN_STABLE.store(0, Ordering::Relaxed);
        log(&format!("shown-topmost: fast OFF ({n} reseated)"));
    }
    let (hwnds, menu_host, tray) = {
        let s = state().lock().unwrap();
        (
            s.windows.values().copied().collect::<Vec<HWND>>(),
            MENU_HOST_HWND.get().copied(),
            TRAY_HWND.get().copied(),
        )
    };
    // 下压限速:额度只在**真正发生下压**时消耗(每次过门都消耗会让杂散
    // 事件吃光额度,关键时刻反而被挡)。600ms:恢复过渡约 2s 内可跟手
    // 2-3 次(动作均被扫动动画遮蔽),同时把 SPES 插队类对抗封顶。
    let now = resize_now_ms();
    let last = LAST_LOWER_MS.load(Ordering::Relaxed);
    let may_lower = last == 0 || now.saturating_sub(last) >= LOWER_RATE_MS;
    let mut acted = false;
    // 免疫模式下不下压:停泊 live 窗迟到出现时把 topmost 栅栏拖下去会
    // 掉出免疫→模式抖动;1s 内走查会让位给模式退出+重归位。
    let shown_mode = SHOWN_TOPMOST.load(Ordering::Relaxed);
    for h in hwnds {
        fence_reanchor_if_below_host(h);
        if !shown_mode && may_lower && fence_lower_if_blocked(h, &menu_host, &tray) {
            acted = true;
        }
    }
    if acted {
        LAST_LOWER_MS.store(resize_now_ms(), Ordering::Relaxed);
    }
}

/// 快速下压:从宿主向上走,遇到第一个可见外来窗 B 先于本栅栏
/// (=栅栏压在已渲染窗口上面)时,把栅栏压到 B 正下方;已紧贴 B 之下则不动。
/// 判据与主走查完全同源(band_invisible/band_aux/自家栅栏),由全局限速节流。
/// 返回是否发生了下压(限速额度据此消耗)。
pub(crate) fn fence_lower_if_blocked(hwnd: HWND, menu_host: &Option<HWND>, tray: &Option<HWND>) -> bool {
    let Some(shell) = desktop_shell_window() else { return false };
    if shell == hwnd {
        return false;
    }
    let vs = virtual_screen_rect();
    let own: Vec<HWND> = state().lock().unwrap().windows.values().copied().collect();
    let mut w = unsafe { GetWindow(shell, GW_HWNDPREV) };
    for _ in 0..400 {
        if w.0 == 0 || w == hwnd {
            return false; // 到顶或先遇到自己:上方没有可见外来窗,无需处理
        }
        if own.contains(&w) || band_invisible(w, &vs) || band_aux(w, *menu_host, *tray) {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        // 坏锚(UIPI 拒锚,elevated 进程窗口):下压必败,跳过不试也不刷
        // 日志;归位由走查 repair 经 band_attach_anchor 的降级锚完成。
        if bad_anchor_recent(w) {
            return false;
        }
        let below = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
        if below != w {
            let _z = z_scope(ZIntent::Repair);
            // 必须检查返回值:曾用 let _ = 丢弃,被 UIPI 拒时也打"lowered"
            // 假成功日志+消耗限速额度,排查时无下手处(2026-08-31 教训)。
            let attempt = unsafe {
                SetWindowPos(
                    hwnd,
                    w,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
            if let Err(e) = attempt {
                bad_anchor_mark(w);
                log(&format!(
                    "z-guard: fence lower FAILED below 0x{:x} err={e:?} (anchor blacklisted)",
                    w.0
                ));
                return false;
            }
            bad_anchor_clear(w);
            log(&format!(
                "z-guard: fence lowered below visible window 0x{:x} (desktop transition)",
                w.0
            ));
            return true;
        }
        return false;
    }
    false
}

#[cfg(test)]
mod tests {
    /// 手编 UTF-16 类名常量的守门:与真实注册名逐字一致(band_aux 容忍匹配
    /// 靠它;编码错一位=容忍失效=面板一开就闪)
    #[test]
    fn cats_panel_class_encoding_matches_registered_name() {
        let expect: Vec<u16> = "DeskFenceCatsPanel".encode_utf16().collect();
        assert_eq!(super::CATS_PANEL_CLASS.to_vec(), expect);
    }
}
