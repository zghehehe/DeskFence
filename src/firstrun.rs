//! 一次性首启引导窗(2026-09-11):首次启动、栅栏接管桌面后弹出一次。
//! 内容=接管说明 + 一键还原教学 + 两个开关(常显边框线/开机自启,默认
//! 取当前实际状态)。settings.first_run_done=false 时弹;任何关闭路径
//! (OK/X)都写 true——用户约定"最多只出现一次"。
//! 普通顶层窗口:非 topmost、无模态消息循环(无重入风险),不碰渲染与
//! z 序。窗口/控件模式与 cats_panel.rs 同款;USERDATA 清理用"先取指针
//! 再清槽"的正确序(2026-09-11 批次 1 的教训,勿回退成先清后读)。

use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, COLOR_BTNFACE, HBRUSH, HFONT, HGDIOBJ};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{model, shell, ui};

/// STATIC/BUTTON 样式与状态常量(windows 0.52 未导出,手写;同 EM_SETSEL 先例)
const SS_LEFT: i32 = 0x0000;
const BST_CHECKED: u32 = 0x0001;

static DIALOG_HWND: Mutex<Option<HWND>> = Mutex::new(None);
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
    ok_btn: HWND,
    font: HFONT,
    scale: f32,
}

/// 启动收尾调用(ui::startup 末尾):未读过引导(first_run_done=false)才弹。
pub fn maybe_show() {
    let mut guard = DIALOG_HWND.lock().unwrap();
    if guard.is_some() || model::load_settings().first_run_done {
        return;
    }
    ensure_class();
    let scale = model::dpi_scale();
    let (w, h) = dialog_size(scale);
    // 主屏工作区水平居中、偏上三分之一处
    let (vx, vy, vw, vh) = ui::work_area();
    let x = vx as i32 + ((vw - w as f32) / 2.0) as i32;
    let y = vy as i32 + ((vh - h as f32) / 3.0) as i32;
    let cls = shell::wide("DeskFenceFirstRun");
    let title = shell::wide(crate::lang::firstrun_title());
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
            HWND(0),
            HMENU(0),
            ui::hinstance(),
            None,
        )
    };
    if hwnd.0 == 0 {
        ui::log("firstrun: create window failed");
        return;
    }
    *guard = Some(hwnd);
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
            hInstance: ui::hinstance(),
            hIcon: HICON(0),
            hCursor: HCURSOR(0),
            // COLOR_BTNFACE+1 = 标准对话框底色
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR::from_raw(cls.as_ptr()),
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
    });
}

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
                    IDC_OK | IDC_CHROME | IDC_AUTOSTART => "BUTTON",
                    _ => "STATIC",
                });
                let t = shell::wide(text);
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
                        hwnd,
                        HMENU(id as _),
                        ui::hinstance(),
                        None,
                    )
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
                ok_btn,
                font,
                scale,
            });
            // 两项默认勾选开启(2026-09-11 用户定案):新装即见边框、常驻自启,
            // 用户在窗内取消即不启用;OK 时与当前实际状态比对,有变化才落盘
            unsafe {
                let _ = SendMessageW(
                    chrome_cb,
                    BM_SETCHECK,
                    WPARAM(BST_CHECKED as usize),
                    LPARAM(0),
                );
                let _ = SendMessageW(
                    autostart_cb,
                    BM_SETCHECK,
                    WPARAM(BST_CHECKED as usize),
                    LPARAM(0),
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
                    ok_btn,
                ] {
                    let _ = SendMessageW(h, WM_SETFONT, f, LPARAM(1));
                }
            }
            layout(hwnd, &mut d);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(d) as isize);
            LRESULT(0)
        }
        WM_COMMAND => {
            // 开始使用:应用两个开关的选择(有变化才动),随后关窗
            if (wp.0 & 0xFFFF) as isize == IDC_OK {
                if let Some(d) = dialog_of(hwnd) {
                    unsafe { apply_choices(d) };
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(d) = dialog_of(hwnd) {
                // 跨屏拖动:按系统建议矩形移动窗口,字体与子控件按新缩放重排
                let s = model::dpi_scale();
                if (s - d.scale).abs() > 0.01 {
                    d.scale = s;
                    unsafe {
                        let r = *(lp.0 as *const RECT);
                        let _ = SetWindowPos(
                            hwnd,
                            HWND(0),
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
                            d.ok_btn,
                        ] {
                            let _ = SendMessageW(h, WM_SETFONT, f, LPARAM(1));
                        }
                    }
                    layout(hwnd, d);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 单一写点:任何关闭路径(OK/X/系统关机)都不再弹第二次
            ui::update_stored_settings(|s| s.first_run_done = true);
            ui::log("firstrun: shown and dismissed");
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // 先取指针、立刻清 USERDATA、再释放(勿回退成先清后读)
            let d = dialog_of(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if let Some(d) = d {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(d.font.0));
                }
                drop(Box::from_raw(d));
            }
            *DIALOG_HWND.lock().unwrap() = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn apply_choices(d: &mut Dialog) {
    let chrome_on =
        unsafe { SendMessageW(d.chrome_cb, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32 }
            == BST_CHECKED;
    if chrome_on != ui::chrome_always_on() {
        ui::set_show_chrome_stored(chrome_on);
        ui::refresh_all_fences();
    }
    let auto_on =
        unsafe { SendMessageW(d.autostart_cb, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32 }
            == BST_CHECKED;
    if auto_on != shell::get_autostart() {
        let _ = shell::set_autostart(auto_on);
    }
}

fn dialog_size(scale: f32) -> (i32, i32) {
    let w = (500.0 * scale) as i32;
    let h = (326.0 * scale) as i32;
    (w, h)
}

/// 子控件布局:窗口客户区宽度随 WM_DPICHANGED 变化,每次全量重排
fn layout(hwnd: HWND, d: &mut Dialog) {
    let mut rc = RECT::default();
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
    // OK 按钮右下角
    let bw = (110.0 * s) as i32;
    let by = h_of(hwnd) - btn_h - pad;
    put(d.ok_btn, w - pad - bw, by, bw, btn_h);
}

fn h_of(hwnd: HWND) -> i32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    rc.bottom - rc.top
}

/// 从 GWLP_USERDATA 取面板(裸指针还原,仅 UI 线程消息路径访问)
unsafe fn dialog_of(hwnd: HWND) -> Option<&'static mut Dialog> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if p == 0 {
        None
    } else {
        Some(&mut *(p as *mut Dialog))
    }
}
