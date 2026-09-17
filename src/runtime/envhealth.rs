/// 环境体检与桌面层自愈（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
/// 只读体检报告(env_health_report)、Explorer 重建(env_repair/internal)、
/// 30s 节拍的环境自稳 watchdog(env_watchdog_tick,带宽限与次数上限)。
/// 叶子模块——只依赖 std / windows crate / state / hosts / settings / shell /
/// logging,不依赖 crate::ui;由 ui 的 startup 与 global_tick 单向调用。
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hosts::*;
use crate::logging::log;
use crate::settings::*;
use crate::shell;
use crate::state::*;

// ---------------- 环境体检与自愈(2026-08-29 教训产品化) ----------------
// 长时间运行/重度使用后,Explorer 桌面层可能被弄脏(双实例互殴、僵尸窗口、
// 宿主链异常),同一份代码表现随之漂移。把排查工具的能力内建为默认配置:
// 启动时自动体检记日志;托盘提供"体检"(诊断报告)与"修复桌面环境"
// (重启 Explorer 重建桌面层,栅栏经 TaskbarCreated 路径自动重挂)。

/// 环境体检(只读)。返回 (是否健康, 中文报告)。
pub fn env_health_report() -> (bool, String) {
    let mut ok = true;
    let mut lines: Vec<String> = Vec::new();
    // 1) 多余 DeskFence 进程(自身已持锁,其余皆僵尸)
    let stale = shell::pids_by_name("deskfence.exe");
    if stale.is_empty() {
        lines.push("实例: 单实例 ✓".into());
    } else {
        ok = false;
        lines.push(format!(
            "实例: 检测到 {} 个多余 DeskFence 进程 {:?}(重启本程序可自动清理)",
            stale.len(),
            stale
        ));
    }
    // 2) 桌面宿主
    match desktop_shell_window() {
        Some(h) => lines.push(format!("桌面宿主: 就绪 0x{:x} ✓", h.0 as usize)),
        None => {
            ok = false;
            lines.push("桌面宿主: 未找到(Explorer 桌面层未就绪)".into());
        }
    }
    // 3) 孤儿 DeskFence 窗口(死去实例的遗留)
    let me = std::process::id();
    let orphans;
    // SAFETY(整块): EnumWindows 同步枚举，回调在返回前完成；ctx 是栈元组，
    // 指针经 lparam 透传（见 enum_orphan 的 Safety 段）。
    unsafe {
        /// # Safety
        /// EnumWindows 回调契约：lparam 指向调用方栈元组 (pid, count)
        /// （同步枚举期间有效）；本回调只读 pid、累加计数、读栈类名缓冲。
        unsafe extern "system" fn enum_orphan(h: HWND, l: LPARAM) -> BOOL {
            // SAFETY: l 按回调契约指向调用方栈元组（见 fn 的 Safety 段）。
            let (me, count) = unsafe {
                let p = l.0 as *mut (u32, usize);
                (&(*p).0, &mut (*p).1)
            };
            let mut buf = [0u16; 32];
            // SAFETY: buf 是栈缓冲，按返回长度截断。
            let n = unsafe { GetClassNameW(h, &mut buf) };
            let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if cls.starts_with("DeskFence") {
                let mut pid = 0u32;
                // SAFETY: pid 是栈输出槽位（GetWindowThreadProcessId 契约）。
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                        h,
                        Some(&mut pid),
                    )
                };
                if pid != *me {
                    *count += 1;
                }
            }
            BOOL(1)
        }
        let mut ctx = (me, 0usize);
        let _ = EnumWindows(Some(enum_orphan), LPARAM(&mut ctx as *mut _ as isize));
        orphans = ctx.1;
    }
    if orphans == 0 {
        lines.push("窗口: 无孤儿窗口 ✓".into());
    } else {
        ok = false;
        lines.push(format!(
            "窗口: 检测到 {orphans} 个孤儿 DeskFence 窗口(建议\"修复桌面环境\")"
        ));
    }
    // 4) 栅栏在带内(仅 normal 态判定)。zen/native 态栅栏有意全部隐藏,
    // total=0 不能构成 fault——2026-08-31 教训:zen 态被此判定恒判 fault,
    // env-watchdog 以 10 分钟限速反复重启 Explorer(一天 3 次),勿回退。
    if desktop_state() == "normal" {
        let host = desktop_shell_window();
        let in_band;
        let total;
        {
            let s = state().lock().unwrap();
            total = s.fences.iter().filter(|f| !f.hidden).count();
            let mut good = 0usize;
            if let Some(host) = host {
                // SAFETY(走查循环): GetWindow 沿 z 链同步取现存窗口（无指针
                // 参数），判空/命中即终止。
                for h in s.windows.values() {
                    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) }.unwrap_or_default();
                    for _ in 0..600 {
                        if w.0.is_null() {
                            break;
                        }
                        if w == *h {
                            good += 1;
                            break;
                        }
                        w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
                    }
                }
            }
            in_band = good;
        }
        if total > 0 && in_band == total {
            lines.push(format!("栅栏: {in_band}/{total} 在桌面层内 ✓"));
        } else {
            ok = false;
            lines.push(format!(
                "栅栏: {in_band}/{total} 在桌面层内(自愈未完成或受阻)"
            ));
        }
    } else {
        lines.push(format!("栅栏: 桌面态 {} 栅栏按状态隐藏 ✓", desktop_state()));
    }
    // 5) 原生图标与接管状态一致性(仅提示,协调器每秒会修)
    if DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) {
        if let Some(lv) = desktop_listview() {
            if unsafe { IsWindowVisible(lv).as_bool() } {
                lines.push("图标: 原生图标意外可见(将在 1 秒内自动隐藏)".into());
            } else {
                lines.push("图标: 接管正常 ✓".into());
            }
        }
    }
    (ok, lines.join("\n"))
}

/// 托盘动作:弹出环境体检报告。
#[allow(dead_code)] // 手动入口已按用户要求移出托盘菜单,保留函数作文档
fn env_health_dialog() {
    let (ok, report) = env_health_report();
    let title = if ok {
        "DeskFence 环境体检:健康"
    } else {
        "DeskFence 环境体检:发现问题"
    };
    log(&format!("env-check by user: ok={ok}\n{report}"));
    let t = shell::wide(title);
    let m = shell::wide(&format!("{report}\n\n(本报告已写入日志)"));
    // SAFETY: 两个宽串均 NUL 结尾且在同步调用期间存活；无父窗口消息框
    // 由系统创建管理。
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR::from_raw(m.as_ptr()),
            PCWSTR::from_raw(t.as_ptr()),
            MB_OK | MB_SETFOREGROUND,
        );
    }
}

/// 托盘动作:修复桌面环境——重启 Explorer 重建桌面层(垃圾层/钩子层/
/// 僵尸托盘全部清零),本程序靠 TaskbarCreated 路径自动重挂栅栏与托盘。
/// 在后台线程执行,避免阻塞 UI。
#[allow(dead_code)]
fn env_repair() {
    let t = shell::wide("DeskFence 修复桌面环境");
    let m = shell::wide(
        "将重启资源管理器以重建桌面层(已打开的文件夹窗口会关闭,\n\
         屏幕会闪黑约 1-2 秒),栅栏与图标接管将自动恢复。\n\n继续?",
    );
    // SAFETY: 同 show_env_report：NUL 宽串在同步调用期间存活。
    let choice = unsafe {
        MessageBoxW(
            None,
            PCWSTR::from_raw(m.as_ptr()),
            PCWSTR::from_raw(t.as_ptr()),
            MB_OKCANCEL | MB_ICONWARNING | MB_SETFOREGROUND,
        )
    };
    if choice != IDOK {
        return;
    }
    env_repair_internal("user");
}

/// 重建桌面层:重启 Explorer,栅栏经 TaskbarCreated 路径自动重挂,
/// 托盘图标自动重建。由托盘"修复桌面环境"与环境自稳 watchdog 共用。
fn env_repair_internal(reason: &str) {
    log(&format!(
        "env-repair({reason}): restarting explorer to rebuild desktop band"
    ));
    std::thread::spawn(|| {
        // 让 UI 先消化掉调用上下文(消息框/自检),再动 Explorer
        std::thread::sleep(std::time::Duration::from_millis(400));
        let remain = shell::terminate_by_name("explorer.exe", 5000);
        if !remain.is_empty() {
            log(&format!("env-repair: explorer pids {:?} resisted", remain));
        }
        shell::start_explorer();
        // 等新宿主就绪(TaskbarCreated 会走重挂路径,这里只做日志收尾)
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if desktop_shell_window().is_some() {
                log("env-repair: desktop host rebuilt");
                return;
            }
        }
        log("env-repair: host not seen in 15s (Explorer may still be starting)");
    });
}

// ---------------- 环境自稳 watchdog(默认保证,非用户自救) ----------------
// 运行期间持续体检(30s 节奏):宿主消失/栅栏持续无法归位/接管被破坏等
// 异常**持续超过宽限期**(自愈已有充足时间修复瞬态)即自动重建桌面层。
// 限额防风暴:两次重建至少间隔 10 分钟,每次运行最多 3 次,超限只记日志。
static WATCHDOG_FAULT_SINCE_MS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_LAST_RECOVERY_MS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_RECOVERIES: AtomicU32 = AtomicU32::new(0);
const WATCHDOG_GRACE_MS: u64 = 90_000;
const WATCHDOG_MIN_INTERVAL_MS: u64 = 600_000;
const WATCHDOG_MAX_RECOVERIES: u32 = 3;

pub(crate) fn env_watchdog_tick() {
    let (ok, report) = env_health_report();
    let now = resize_now_ms();
    if ok {
        WATCHDOG_FAULT_SINCE_MS.store(0, Ordering::Relaxed);
        return;
    }
    // 首次发现异常记起点;持续不足宽限期则等自愈工作
    let since = {
        let prev = WATCHDOG_FAULT_SINCE_MS.load(Ordering::Relaxed);
        if prev == 0 {
            WATCHDOG_FAULT_SINCE_MS.store(now, Ordering::Relaxed);
            log(&format!(
                "env-watchdog: fault started, waiting self-heal ({report})"
            ));
            now
        } else {
            prev
        }
    };
    if now.saturating_sub(since) < WATCHDOG_GRACE_MS {
        return;
    }
    let last = WATCHDOG_LAST_RECOVERY_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < WATCHDOG_MIN_INTERVAL_MS {
        return;
    }
    let count = WATCHDOG_RECOVERIES.load(Ordering::Relaxed);
    if count >= WATCHDOG_MAX_RECOVERIES {
        if count == WATCHDOG_MAX_RECOVERIES {
            log("env-watchdog: recovery cap reached, logging only");
            WATCHDOG_RECOVERIES.store(count + 1, Ordering::Relaxed);
        }
        return;
    }
    WATCHDOG_RECOVERIES.fetch_add(1, Ordering::Relaxed);
    WATCHDOG_LAST_RECOVERY_MS.store(now, Ordering::Relaxed);
    WATCHDOG_FAULT_SINCE_MS.store(0, Ordering::Relaxed);
    log(&format!(
        "env-watchdog: sustained fault beyond grace, auto-rebuilding desktop band (recovery #{})",
        count + 1
    ));
    env_repair_internal("watchdog");
}
