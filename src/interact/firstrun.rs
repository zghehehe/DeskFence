//! 首启引导窗(2026-09-11;2026-09-16 改为可重复出现):首次启动、栅栏接管
//! 桌面后弹出。内容=接管说明 + 一键还原教学 + 两个开关(常显边框线/开机
//! 自启,默认取当前实际状态)。settings.first_run_done=false 时弹;关闭时
//! 只有勾选了"不再提示"复选框才写 true,不勾则下次启动再次弹出(2026-09-16
//! 用户约定,替代早期"最多只出现一次")。
//! 普通顶层窗口:非 topmost、无模态消息循环(无重入风险),不碰渲染与
//! z 序。窗口/控件模式与 cats_panel.rs 同款;USERDATA 清理用"先取指针
//! 再清槽"的正确序(2026-09-11 批次 1 的教训,勿回退成先清后读)。

use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, COLOR_BTNFACE, HBRUSH, HFONT, HGDIOBJ};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::winids::SyncHandle;
use crate::{logging, model, monitors, present, settings, shell, winids};

/// STATIC/BUTTON 样式与状态常量(windows crate 未导出,0.62 仍缺,手写;同 EM_SETSEL 先例)
const SS_LEFT: i32 = 0x0000;
const BST_CHECKED: u32 = 0x0001;

static DIALOG_HWND: SyncHandle<Mutex<Option<HWND>>> = SyncHandle(Mutex::new(None));
static CLASS_REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// 子控件 ID(STATIC 不发命令,ID 随意但要唯一)
const IDC_INTRO1: isize = 0x310;
const IDC_INTRO2: isize = 0x311;
const IDC_HEAD: isize = 0x312;
const IDC_R1: isize = 0x313;
const IDC_R2: isize = 0x314;
const IDC_R3: isize = 0x315;
const IDC_CHROME: isize = 0x316;
const IDC_AUTOSTART: isize = 0x317;
const IDC_OK: isize = 0x318;
const IDC_OPTS_HEAD: isize = 0x319;
const IDC_NOAGAIN: isize = 0x31A;

struct Dialog {
    intro1: HWND,
    intro2: HWND,
    head: HWND,
    r1: HWND,
    r2: HWND,
    r3: HWND,
    opts_head: HWND,
    chrome_cb: HWND,
    autostart_cb: HWND,
    noagain_cb: HWND,
    ok_btn: HWND,
    font: HFONT,
    scale: f32,
}

/// 启动收尾调用(ui::startup 末尾):未勾选"不再提示"(first_run_done=false)才弹。
pub fn maybe_show() {
    let mut guard = DIALOG_HWND.lock().unwrap();
    if !should_show(guard.is_some(), model::load_settings().first_run_done) {
        return;
    }
    ensure_class();
    let scale = model::dpi_scale();
    let (w, h) = dialog_size(scale);
    // 主屏工作区水平居中、偏上三分之一处
    let (vx, vy, vw, vh) = monitors::work_area();
    let x = vx as i32 + ((vw - w as f32) / 2.0) as i32;
    let y = vy as i32 + ((vh - h as f32) / 3.0) as i32;
    let cls = shell::wide("DeskFenceFirstRun");
    let title = shell::wide(crate::lang::firstrun_title());
    // SAFETY: cls/title 为 NUL 宽串（同步创建期间存活）、类已由
    // ensure_class 注册、hinstance 是本进程模块；失败判空返回。
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR::from_raw(cls.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(winids::hinstance()),
            None,
        )
        .unwrap_or_default()
    };
    if hwnd.0.is_null() {
        logging::log("firstrun: create window failed");
        return;
    }
    *guard = Some(hwnd);
    // SAFETY: hwnd 是刚创建的本进程引导窗；纯可见性调用。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    }
}

fn ensure_class() {
    CLASS_REGISTERED.get_or_init(|| {
        let cls = shell::wide("DeskFenceFirstRun");
        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(firstrun_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: winids::hinstance(),
            hIcon: HICON(std::ptr::null_mut()),
            hCursor: HCURSOR(std::ptr::null_mut()),
            // COLOR_BTNFACE+1 = 标准对话框底色
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut std::ffi::c_void),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR::from_raw(cls.as_ptr()),
        };
        // SAFETY: firstrun_wndproc 是匹配 WNDPROC ABI 的窗口过程；cls 是
        // NUL 宽串；hbrBackground 的"系统颜色索引+1"是 WNDCLASSW 契约编码；
        // OnceLock 保证只注册一次。
        unsafe {
            let _ = RegisterClassW(&wc);
        }
    });
}

/// # Safety
/// 首启引导窗的窗口过程（ensure_class 注册，系统在 UI 线程同步回调）。
/// 体内裸 unsafe 的依据：WM_CREATE 把 Box<Dialog> 经 Box::into_raw 存入
/// GWLP_USERDATA、WM_NCDESTROY 先取指针再清槽后释放（窗口存活期间指针
/// 有效，仅 UI 线程访问）；mk 闭包创建的子控件句柄由 Dialog 持有、随父
/// 窗口销毁；WM_DPICHANGED 的 lp 按消息契约指向建议 RECT（可读）；
/// 字体句柄替换/销毁时 DeleteObject 配对。
unsafe extern "system" fn firstrun_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let scale = model::dpi_scale();
            let font = crate::cats_panel::create_dialog_font(false);
            let mk = |id: isize, text: &str, style: i32| {
                let style = WINDOW_STYLE(style as u32);
                let cls = shell::wide(match id {
                    IDC_OK | IDC_CHROME | IDC_AUTOSTART | IDC_NOAGAIN => "BUTTON",
                    _ => "STATIC",
                });
                let t = shell::wide(text);
                // SAFETY: cls/t 为 NUL 宽串；hwnd 是本对话框窗口、
                // hinstance 是本进程模块、BUTTON/STATIC 是系统类；HMENU
                // 参数承载控件 ID（WM_COMMAND 编码约定）；失败得 null 句柄。
                unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        PCWSTR::from_raw(cls.as_ptr()),
                        PCWSTR::from_raw(t.as_ptr()),
                        WS_CHILD | WS_VISIBLE | style,
                        0,
                        0,
                        10,
                        10,
                        Some(hwnd),
                        Some(HMENU(id as *mut std::ffi::c_void)),
                        Some(winids::hinstance()),
                        None,
                    )
                    .unwrap_or_default()
                }
            };
            let intro1 = mk(IDC_INTRO1, crate::lang::firstrun_intro1(), SS_LEFT);
            let intro2 = mk(IDC_INTRO2, crate::lang::firstrun_intro2(), SS_LEFT);
            let head = mk(IDC_HEAD, crate::lang::firstrun_rescue_head(), SS_LEFT);
            let r1 = mk(IDC_R1, crate::lang::firstrun_rescue1(), SS_LEFT);
            let r2 = mk(IDC_R2, crate::lang::firstrun_rescue2(), SS_LEFT);
            let r3 = mk(IDC_R3, crate::lang::firstrun_rescue3(), SS_LEFT);
            let opts_head = mk(IDC_OPTS_HEAD, crate::lang::firstrun_opts_head(), SS_LEFT);
            let chrome_cb = mk(
                IDC_CHROME,
                crate::lang::firstrun_chrome_cb(),
                BS_AUTOCHECKBOX,
            );
            let autostart_cb = mk(
                IDC_AUTOSTART,
                crate::lang::firstrun_autostart_cb(),
                BS_AUTOCHECKBOX,
            );
            // "不再提示"默认不勾:不勾=下次启动仍弹出,勾选才写 first_run_done
            let noagain_cb = mk(
                IDC_NOAGAIN,
                crate::lang::firstrun_noagain_cb(),
                BS_AUTOCHECKBOX,
            );
            let ok_btn = mk(IDC_OK, crate::lang::firstrun_ok(), BS_DEFPUSHBUTTON);
            let mut d = Box::new(Dialog {
                intro1,
                intro2,
                head,
                r1,
                r2,
                r3,
                opts_head,
                chrome_cb,
                autostart_cb,
                noagain_cb,
                ok_btn,
                font,
                scale,
            });
            // 两项默认勾选开启(2026-09-11 用户定案):新装即见边框、常驻自启,
            // 用户在窗内取消即不启用;OK 时与当前实际状态比对,有变化才落盘
            // SAFETY: 三个复选框/字体消息均作用于刚创建的子控件；WM_SETFONT
            // 只借用字体句柄（所有权在 Dialog.font）。
            unsafe {
                let _ = SendMessageW(
                    chrome_cb,
                    BM_SETCHECK,
                    Some(WPARAM(BST_CHECKED as usize)),
                    Some(LPARAM(0)),
                );
                let _ = SendMessageW(
                    autostart_cb,
                    BM_SETCHECK,
                    Some(WPARAM(BST_CHECKED as usize)),
                    Some(LPARAM(0)),
                );
                let f = WPARAM(d.font.0 as usize);
                for h in [
                    intro1,
                    intro2,
                    head,
                    r1,
                    r2,
                    r3,
                    opts_head,
                    chrome_cb,
                    autostart_cb,
                    noagain_cb,
                    ok_btn,
                ] {
                    let _ = SendMessageW(h, WM_SETFONT, Some(f), Some(LPARAM(1)));
                }
            }
            layout(hwnd, &mut d);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(d) as isize);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // 开始使用:应用两个开关的选择(有变化才动),随后关窗
            if (wp.0 & 0xFFFF) as isize == IDC_OK {
                if let Some(d) = unsafe { dialog_of(hwnd) } {
                    // SAFETY: 见 apply_choices/dialog_of 的 Safety 段
                    //（子控件句柄存活、UI 线程）。
                    unsafe { apply_choices(d) };
                    // SAFETY: hwnd 是本对话框窗口，销毁恰好一次（走
                    // WM_DESTROY/WM_NCDESTROY 清理链）。
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(d) = unsafe { dialog_of(hwnd) } {
                // 跨屏拖动:按系统建议矩形移动窗口,字体与子控件按新缩放重排
                let s = model::dpi_scale();
                if (s - d.scale).abs() > 0.01 {
                    d.scale = s;
                    // SAFETY: lp 按 WM_DPICHANGED 契约指向建议 RECT（可读）；
                    // SetWindowPos 用建议坐标+NOZORDER；旧字体 DeleteObject
                    // 后立即替换（句柄不再使用）；子控件消息作用于现存句柄。
                    unsafe {
                        let r = *(lp.0 as *const RECT);
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        let _ = DeleteObject(HGDIOBJ(d.font.0));
                        d.font = crate::cats_panel::create_dialog_font(false);
                        let f = WPARAM(d.font.0 as usize);
                        for h in [
                            d.intro1,
                            d.intro2,
                            d.head,
                            d.r1,
                            d.r2,
                            d.r3,
                            d.opts_head,
                            d.chrome_cb,
                            d.autostart_cb,
                            d.noagain_cb,
                            d.ok_btn,
                        ] {
                            let _ = SendMessageW(h, WM_SETFONT, Some(f), Some(LPARAM(1)));
                        }
                    }
                    layout(hwnd, d);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: hwnd 是本对话框窗口，销毁恰好一次（走清理链）。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 单一写点:任何关闭路径(OK/X/系统关机)都在这里收尾。只有勾选了
            // "不再提示"才写 first_run_done;不勾则保持 false,下次启动再弹
            // (2026-09-16 用户约定)。WM_DESTROY 时子控件尚未销毁,BM_GETCHECK
            // 仍可读。
            let noagain = if let Some(d) = unsafe { dialog_of(hwnd) } {
                // SAFETY: WM_DESTROY 时子控件尚未销毁（父先于子），
                // BM_GETCHECK 可读；纯消息查询。
                (unsafe {
                    SendMessageW(d.noagain_cb, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0
                        as u32
                } == BST_CHECKED)
            } else {
                false
            };
            if noagain {
                settings::update_stored_settings(|s| s.first_run_done = true);
            }
            logging::log(&format!(
                "firstrun: shown and dismissed (noagain={noagain})"
            ));
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // 先取指针、立刻清 USERDATA、再释放(勿回退成先清后读)
            let d = unsafe { dialog_of(hwnd) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            if let Some(d) = d {
                // SAFETY: d 是 dialog_of 取出的合法 Dialog 指针（清槽前取出），
                // 字体句柄随 Box 释放前 DeleteObject。
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(d.font.0));
                }
                drop(unsafe { Box::from_raw(d) });
            }
            *DIALOG_HWND.lock().unwrap() = None;
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// # Safety
/// 必须在 UI 线程、以本对话框窗口调用；GWLP_USERDATA 只由 WM_CREATE
/// （Box::into_raw 写入）与 WM_NCDESTROY（先取后清 0）触碰，窗口存活期间
/// 指向合法堆对象；返回的 &mut 只在本消息处理内使用。
unsafe fn apply_choices(d: &mut Dialog) {
    // SAFETY: 两个复选框是 Dialog 持有的现存子控件；BM_GETCHECK 纯查询。
    let chrome_on = unsafe {
        SendMessageW(d.chrome_cb, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 as u32
    } == BST_CHECKED;
    // SAFETY: 同上。
    let auto_on = unsafe {
        SendMessageW(
            d.autostart_cb,
            BM_GETCHECK,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0 as u32
    } == BST_CHECKED;
    // 有变化才落盘(差异比对纯核 diff_choices)
    let (chrome_new, auto_new) = diff_choices(
        chrome_on,
        settings::chrome_always_on(),
        auto_on,
        shell::get_autostart(),
    );
    if let Some(on) = chrome_new {
        settings::set_show_chrome_stored(on);
        present::refresh_all_fences();
    }
    if let Some(on) = auto_new {
        let _ = shell::set_autostart(on);
    }
}

/// 弹出门槛(纯):已开着或已勾选"不再提示"都不弹
pub fn should_show(already_open: bool, first_run_done: bool) -> bool {
    !already_open && !first_run_done
}

/// "有变化才落盘"差异比对(纯):返回 (chrome 新值, 自启新值),None=不动
pub fn diff_choices(
    chrome_cb: bool,
    chrome_cur: bool,
    auto_cb: bool,
    auto_cur: bool,
) -> (Option<bool>, Option<bool>) {
    (
        if chrome_cb != chrome_cur {
            Some(chrome_cb)
        } else {
            None
        },
        if auto_cb != auto_cur {
            Some(auto_cb)
        } else {
            None
        },
    )
}

pub fn dialog_size(scale: f32) -> (i32, i32) {
    let w = (500.0 * scale) as i32;
    // 2026-09-16 加"不再提示"复选框一行,高度 326→350
    let h = (350.0 * scale) as i32;
    (w, h)
}

/// 子控件布局:窗口客户区宽度随 WM_DPICHANGED 变化,每次全量重排
fn layout(hwnd: HWND, d: &mut Dialog) {
    let mut rc = RECT::default();
    // SAFETY: rc 是栈输出指针（GetClientRect 契约）。
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let s = d.scale;
    let pad = (14.0 * s) as i32;
    let w = (rc.right - rc.left).max((300.0 * s) as i32);
    let line_h = (17.0 * s) as i32;
    let wrap_h = (32.0 * s) as i32;
    let cb_h = (20.0 * s) as i32;
    let btn_h = (26.0 * s) as i32;
    let mut y = pad;
    // SAFETY: h 是 Dialog 持有的现存子控件；MoveWindow 纯定位。
    let put = |h: HWND, x: i32, y: i32, cw: i32, ch: i32| unsafe {
        let _ = MoveWindow(h, x, y, cw, ch, true);
    };
    let text_w = w - pad * 2;
    put(d.intro1, pad, y, text_w, wrap_h);
    y += wrap_h + (2.0 * s) as i32;
    put(d.intro2, pad, y, text_w, wrap_h);
    y += wrap_h + (16.0 * s) as i32;
    put(d.head, pad, y, text_w, line_h);
    y += line_h + (2.0 * s) as i32;
    put(d.r1, pad, y, text_w, line_h);
    y += line_h + (2.0 * s) as i32;
    put(d.r2, pad, y, text_w, line_h);
    y += line_h + (2.0 * s) as i32;
    put(d.r3, pad, y, text_w, line_h);
    y += line_h + (8.0 * s) as i32;
    put(d.opts_head, pad, y, text_w, line_h);
    y += line_h + (6.0 * s) as i32;
    put(d.chrome_cb, pad, y, text_w, cb_h);
    y += cb_h + (4.0 * s) as i32;
    put(d.autostart_cb, pad, y, text_w, cb_h);
    y += cb_h + (4.0 * s) as i32;
    put(d.noagain_cb, pad, y, text_w, cb_h);
    // OK 按钮右下角
    let bw = (110.0 * s) as i32;
    let by = h_of(hwnd) - btn_h - pad;
    put(d.ok_btn, w - pad - bw, by, bw, btn_h);
}

fn h_of(hwnd: HWND) -> i32 {
    let mut rc = RECT::default();
    // SAFETY: rc 是栈输出指针（GetClientRect 契约）。
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    rc.bottom - rc.top
}

/// 从 GWLP_USERDATA 取面板(裸指针还原,仅 UI 线程消息路径访问)
///
/// # Safety
/// 必须在 UI 线程、以本对话框窗口调用；GWLP_USERDATA 只由 WM_CREATE/
/// WM_NCDESTROY 成对写入/清零（见 firstrun_wndproc 的 Safety 段），窗口
/// 存活期间指向合法堆对象；返回的 &mut 只在本消息处理内使用。
unsafe fn dialog_of(hwnd: HWND) -> Option<&'static mut Dialog> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        Some(unsafe { &mut *(p as *mut Dialog) })
    }
}
