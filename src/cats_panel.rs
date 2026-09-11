//! 分类管理小面板(2026-09-08):托盘"自动分类▸"子菜单的承载窗口。
//! 每行 = 分类名(EDIT 点击就地改,失焦/关窗即提交) + 行尾 ×(删除);
//! 兜底"其他"行只读且无 ×;底部"＋ 新增分类"。Win32 菜单做不了行内
//! 编辑和行内按钮,故用本面板承载(用户定案的交互形态)。
//! 所有修改经 crate::menu::apply_category_* 即时生效:分类表(settings.json)、
//! 桌面栅栏(改名/删除/新建)与文件归属(改名跟随/删除落"其他")同步,
//! 不变量:任何时刻所有文件都在某个栅栏可见。
//! 本模块属于 ui.rs 拆分的增量部分:窗口/控件代码独立成模块,不进 ui.rs。

use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, CreateFontIndirectW, HBRUSH, HFONT, HGDIOBJ, COLOR_BTNFACE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{model, shell, ui};

/// EDIT 控件消息(windows 0.52 未在 WindowsAndMessaging 导出,手写常量)
const EM_SETSEL: u32 = 0x00B1;
const EM_SETREADONLY: u32 = 0x00CF;

/// 面板窗口句柄(全局唯一;已开则置前而不是开第二个)
static PANEL_HWND: Mutex<Option<HWND>> = Mutex::new(None);
static CLASS_REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// 内部命令:已开面板上执行"新增分类"(托盘重复触发时复用)
const WM_APP_ADD: u32 = WM_APP + 1;
/// 子控件 ID 分配:第 i 行 edit=0x100+2i、×=0x101+2i(奇数即删除键)
const ID_ADD: isize = 0x2FF;

struct Row {
    edit: HWND,
    exts_edit: HWND, // 规则(扩展名清单,空格分隔)
    del: HWND,       // 兜底行为 HWND(0)
    name: String,
    exts: String,
    locked_name: bool, // 兜底行名称只读
    locked_exts: bool, // 兜底/目录行规则只读
}
struct Panel {
    rows: Vec<Row>,
    font: HFONT,
    add_btn: HWND,
    scale: f32,
}

/// 托盘入口:打开面板。focus=置为编辑焦点的表内下标;create_new=打开即新增。
/// 主消息循环调用:面板获得键盘时拦截 Esc(关闭)/Enter(提交当前编辑)。
/// 返回 true=消息已处理,调用方跳过默认分发。
pub fn panel_message(msg: &MSG) -> bool {
    let Some(h) = *PANEL_HWND.lock().unwrap() else {
        return false;
    };
    if msg.message != WM_KEYDOWN || msg.hwnd.0 == 0 {
        return false;
    }
    if !unsafe { IsChild(h, msg.hwnd) }.as_bool() {
        return false;
    }
    let vk = msg.wParam.0 as u32;
    if vk == VK_ESCAPE.0 as u32 {
        let _ = unsafe { PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)) };
        true
    } else if vk == VK_RETURN.0 as u32 {
        unsafe { commit_focused_panel(h) };
        true
    } else {
        false
    }
}

/// 提交当前获得焦点的编辑(名称或规则);WM_CLOSE 复用同一入口
unsafe fn commit_focused_panel(hwnd: HWND) {
    if let Some(panel) = panel_of(hwnd) {
        let focused = unsafe { GetFocus() };
        if let Some(i) = panel.rows.iter().position(|r| r.edit == focused) {
            commit_row(panel, i);
        } else if let Some(i) = panel.rows.iter().position(|r| r.exts_edit == focused) {
            commit_exts(panel, i);
        }
    }
}

pub fn open_panel(focus: Option<usize>, create_new: bool) {
    let mut guard = PANEL_HWND.lock().unwrap();
    if let Some(h) = *guard {
        // 已开:置前即可,焦点/新增按需补发
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        if let Some(i) = focus {
            send_focus_row(h, i);
        }
        if create_new {
            let _ = unsafe { PostMessageW(h, WM_APP_ADD, WPARAM(0), LPARAM(0)) };
        }
        return;
    }
    ensure_class();
    let scale = model::dpi_scale();
    let (w, h) = panel_size_for(scale, model::category_table().len());
    // 锚在鼠标附近(托盘子菜单触发点),夹回虚拟屏内
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let vs_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vs_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vs_w = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let vs_h = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let px = (pt.x - w / 2).clamp(vs_x, (vs_x + vs_w - w).max(vs_x));
    let py = (pt.y - h - 12).clamp(vs_y, (vs_y + vs_h - h).max(vs_y));
    let cls = shell::wide("DeskFenceCatsPanel");
    let title = shell::wide(crate::lang::cats_title());
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(cls.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            px,
            py,
            w,
            h,
            HWND(0),
            HMENU(0),
            ui::hinstance(),
            None,
        )
    };
    if hwnd.0 == 0 {
        ui::log("cats panel: create window failed");
        return;
    }
    *guard = Some(hwnd);
    // 托盘菜单手势链路内的前台化(与 TrackPopupMenu 同法);守卫随作用域释放
    let _fg = unsafe { shell::menu_foreground(hwnd) };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    if let Some(i) = focus {
        send_focus_row(hwnd, i);
    }
    if create_new {
        let _ = unsafe { PostMessageW(hwnd, WM_APP_ADD, WPARAM(0), LPARAM(0)) };
    }
}

fn ensure_class() {
    CLASS_REGISTERED.get_or_init(|| {
        let cls = shell::wide("DeskFenceCatsPanel");
        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(cats_wndproc),
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

unsafe extern "system" fn cats_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let scale = model::dpi_scale();
            let font = create_dialog_font();
            let mut panel = Box::new(Panel {
                rows: Vec::new(),
                font,
                add_btn: HWND(0),
                scale,
            });
            for c in model::category_table() {
                let locked_name = c.name == model::FALLBACK_CATEGORY;
                let exts_text = if c.dirs { crate::lang::dirs_marker().to_string() } else { c.exts.join(";") };
                append_row(hwnd, &mut panel, &c.name, &exts_text, locked_name, locked_name || c.dirs);
            }
            panel.add_btn = create_add_button(hwnd, &panel);
            layout_all(hwnd, &panel);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(panel) as isize);
            LRESULT(0)
        }
        WM_COMMAND => {
            if let Some(panel) = panel_of(hwnd) {
                let id = (wp.0 & 0xFFFF) as isize;
                let code = ((wp.0 >> 16) & 0xFFFF) as u32;
                let src = HWND(lp.0 as _);
                if code == EN_KILLFOCUS {
                    // 失焦即提交(点击 × 前也会先失焦,顺序天然正确)
                    if let Some(i) = panel.rows.iter().position(|r| r.edit == src) {
                        commit_row(panel, i);
                    } else if let Some(i) = panel.rows.iter().position(|r| r.exts_edit == src) {
                        commit_exts(panel, i);
                    }
                } else if code == BN_CLICKED {
                    if id == ID_ADD {
                        do_add(hwnd, panel);
                    } else if id >= 0x102 && (id - 0x102) % 3 == 0 {
                        let i = ((id - 0x102) / 3) as usize;
                        if i < panel.rows.len() {
                            do_delete(panel, i);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_ADD => {
            if let Some(panel) = panel_of(hwnd) {
                do_add(hwnd, panel);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 关窗前提交在编辑中的行
            commit_focused_panel(hwnd);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // 跨屏拖动面板:重算缩放与字体,行内容原样保留
            if let Some(panel) = panel_of(hwnd) {
                let s = model::dpi_scale();
                if (s - panel.scale).abs() > 0.01 {
                    panel.scale = s;
                    unsafe {
                        let _ = DeleteObject(HGDIOBJ(panel.font.0));
                    }
                    panel.font = create_dialog_font();
                    let f = WPARAM(panel.font.0 as usize);
                    for r in &panel.rows {
                        let _ = SendMessageW(r.edit, WM_SETFONT, f, LPARAM(1));
                        let _ = SendMessageW(r.exts_edit, WM_SETFONT, f, LPARAM(1));
                        if r.del.0 != 0 {
                            let _ = SendMessageW(r.del, WM_SETFONT, f, LPARAM(1));
                        }
                    }
                    let _ = SendMessageW(panel.add_btn, WM_SETFONT, f, LPARAM(1));
                    layout_all(hwnd, panel);
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // 先取出面板指针、立刻清 USERDATA:此后子窗口销毁触发的 EN_KILLFOCUS
            // 经 panel_of 拿到 None 安全空转;释放放在清空之后,保证真正执行
            // (旧写法先清再读同一槽位,释放分支永不执行=每次开面板漏一个字体+一块堆)。
            let panel = panel_of(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if let Some(p) = panel {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(p.font.0));
                }
                drop(Box::from_raw(p));
            }
            *PANEL_HWND.lock().unwrap() = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 从 GWLP_USERDATA 取面板(裸指针还原,仅 UI 线程消息路径访问)
unsafe fn panel_of(hwnd: HWND) -> Option<&'static mut Panel> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if p == 0 {
        None
    } else {
        Some(&mut *(p as *mut Panel))
    }
}

fn append_row(parent: HWND, panel: &mut Panel, name: &str, exts_text: &str, locked_name: bool, locked_exts: bool) {
    let i = panel.rows.len();
    let s = panel.scale;
    let row_h = row_h(s);
    let pad = pad(s);
    let del_w = del_w(s);
    let name_w = name_w(s);
    let w = panel_w(s);
    let y = pad as i32 + (i as f32 * (row_h + row_gap(s))) as i32;
    let cls_edit = shell::wide("EDIT");
    let name_t = shell::wide(name);
    let edit = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            PCWSTR::from_raw(cls_edit.as_ptr()),
            PCWSTR::from_raw(name_t.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE((ES_LEFT | ES_AUTOHSCROLL) as u32),
            pad as i32,
            y,
            name_w as i32,
            row_h as i32,
            parent,
            HMENU(0),
            ui::hinstance(),
            None,
        )
    };
    // 规则列:扩展名清单(空格分隔),目录/兜底行为只读占位
    let exts_x = (pad + name_w) as i32 + 4;
    let exts_w = (w - pad * 2.0 - del_w - name_w - 8.0) as i32;
    let exts_t = shell::wide(exts_text);
    let exts_edit = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            PCWSTR::from_raw(cls_edit.as_ptr()),
            PCWSTR::from_raw(exts_t.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE((ES_LEFT | ES_AUTOHSCROLL) as u32),
            exts_x,
            y,
            exts_w,
            row_h as i32,
            parent,
            HMENU(0),
            ui::hinstance(),
            None,
        )
    };
    let del = if locked_name {
        HWND(0) // 兜底行不可删:不渲染 ×
    } else {
        let cls_btn = shell::wide("BUTTON");
        let xt = shell::wide("\u{d7}");
        unsafe {
            CreateWindowExW(
                WS_EX_NOPARENTNOTIFY,
                PCWSTR::from_raw(cls_btn.as_ptr()),
                PCWSTR::from_raw(xt.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                (w - pad - del_w) as i32,
                y,
                del_w as i32,
                row_h as i32,
                parent,
                HMENU(0),
                ui::hinstance(),
                None,
            )
        }
    };
    unsafe {
        let f = WPARAM(panel.font.0 as usize);
        let _ = SendMessageW(edit, WM_SETFONT, f, LPARAM(1));
        let _ = SendMessageW(exts_edit, WM_SETFONT, f, LPARAM(1));
        if del.0 != 0 {
            let _ = SendMessageW(del, WM_SETFONT, f, LPARAM(1));
        }
        if locked_name {
            let _ = SendMessageW(edit, EM_SETREADONLY, WPARAM(1), LPARAM(0));
        }
        if locked_exts {
            let _ = SendMessageW(exts_edit, EM_SETREADONLY, WPARAM(1), LPARAM(0));
        }
        // 控件 ID 承载行号:name=0x100+3i,exts=0x101+3i,del=0x102+3i
        let _ = SetWindowLongPtrW(edit, GWLP_ID, 0x100 + 3 * i as isize);
        let _ = SetWindowLongPtrW(exts_edit, GWLP_ID, 0x101 + 3 * i as isize);
        if del.0 != 0 {
            let _ = SetWindowLongPtrW(del, GWLP_ID, 0x102 + 3 * i as isize);
        }
    }
    panel.rows.push(Row {
        edit,
        exts_edit,
        del,
        name: name.to_string(),
        exts: exts_text.to_string(),
        locked_name,
        locked_exts,
    });
}

fn create_add_button(parent: HWND, panel: &Panel) -> HWND {
    let s = panel.scale;
    let cls_btn = shell::wide("BUTTON");
    let t = shell::wide(crate::lang::cats_add_btn());
    unsafe {
        let h = CreateWindowExW(
            WS_EX_NOPARENTNOTIFY,
            PCWSTR::from_raw(cls_btn.as_ptr()),
            PCWSTR::from_raw(t.as_ptr()),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            pad(s) as i32,
            0, // 落位交 layout_all
            (panel_w(s) - pad(s) * 2.0) as i32,
            row_h(s) as i32,
            parent,
            HMENU(ID_ADD),
            ui::hinstance(),
            None,
        );
        let _ = SendMessageW(h, WM_SETFONT, WPARAM(panel.font.0 as usize), LPARAM(1));
        h
    }
}

/// 统一重排:行从上往下、新增按钮沉底,窗口高度随行数变化
fn layout_all(hwnd: HWND, panel: &Panel) {
    let s = panel.scale;
    let client_h = layout_rows(panel);
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: panel_w(s) as i32,
        bottom: client_h,
    };
    unsafe {
        let _ = AdjustWindowRectEx(
            &mut rc,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOOLWINDOW,
        );
        let _ = SetWindowPos(
            hwnd,
            HWND(0),
            0,
            0,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// 重排行与新增按钮(删除行后也走这里,窗口尺寸不变),返回客户区高度
fn layout_rows(panel: &Panel) -> i32 {
    let s = panel.scale;
    let pad = pad(s) as i32;
    let row_h = row_h(s) as i32;
    let gap = row_gap(s) as i32;
    let del_w = del_w(s) as i32;
    let w = panel_w(s) as i32;
    let n = panel.rows.len();
    unsafe {
        let name_w = name_w(s) as i32;
        for (i, r) in panel.rows.iter().enumerate() {
            let y = pad + (i as i32) * (row_h + gap);
            let _ = MoveWindow(r.edit, pad, y, name_w, row_h, true);
            let _ = MoveWindow(
                r.exts_edit,
                pad + name_w + 4,
                y,
                w - pad * 2 - del_w - name_w - 8,
                row_h,
                true,
            );
            if r.del.0 != 0 {
                let _ = MoveWindow(r.del, w - pad - del_w, y, del_w, row_h, true);
            }
        }
        let _ = MoveWindow(
            panel.add_btn,
            pad,
            pad + n as i32 * (row_h + gap),
            w - pad * 2,
            row_h,
            true,
        );
    }
    pad + (n as i32 + 1) * (row_h + gap)
}

fn do_delete(panel: &mut Panel, i: usize) {
    // 先提交可能悬挂的改名(含本行自身),再删
    commit_row(panel, i);
    let name = panel.rows[i].name.clone();
    if name == model::FALLBACK_CATEGORY {
        return; // 兜底不可删(按钮未渲染,双保险)
    }
    if !crate::menu::apply_category_delete(&name) {
        return;
    }
    unsafe {
        let _ = DestroyWindow(panel.rows[i].edit);
        let _ = DestroyWindow(panel.rows[i].exts_edit);
        if panel.rows[i].del.0 != 0 {
            let _ = DestroyWindow(panel.rows[i].del);
        }
    }
    panel.rows.remove(i);
    layout_rows(panel);
}

fn do_add(hwnd: HWND, panel: &mut Panel) {
    // 控件 ID 方案上限(0x100+3i,add=0x2FF):约 136 行,实际分类远少于此;
    // 到顶拒绝并留痕,防 ID 相撞
    if panel.rows.len() >= 130 {
        ui::log("cats panel: row limit reached, add refused");
        return;
    }
    // 先提交在编辑的行,避免新增与悬挂改名竞争
    let focused = unsafe { GetFocus() };
    if let Some(i) = panel.rows.iter().position(|r| r.edit == focused) {
        commit_row(panel, i);
    }
    if let Some(name) = crate::menu::apply_category_add(crate::lang::new_category_base()) {
        append_row(hwnd, panel, &name, "", false, false);
        layout_all(hwnd, panel);
        if let Some(r) = panel.rows.last() {
            unsafe {
                let _ = SetFocus(r.edit);
                let _ = SendMessageW(r.edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            }
        }
    }
}

/// 失焦/关窗提交:空名或重名回滚显示;成功经 crate::menu::apply_category_rename 同步全部状态
fn commit_row(panel: &mut Panel, i: usize) {
    let (text, old) = {
        let r = &panel.rows[i];
        (get_text(r.edit), r.name.clone())
    };
    let text = text.trim().to_string();
    if text == old {
        return;
    }
    if panel.rows[i].locked_name {
        set_text(panel.rows[i].edit, &old);
        return;
    }
    if text.is_empty() || !crate::menu::apply_category_rename(&old, &text) {
        let r = &panel.rows[i];
        set_text(r.edit, &old);
        return;
    }
    panel.rows[i].name = text;
}

/// 规则提交:解析(逗号/空格/分号分隔,小写去点去重)后经
/// menu::apply_category_exts 生效;冲突/非法则回滚显示
fn commit_exts(panel: &mut Panel, i: usize) {
    let (text, old) = {
        let r = &panel.rows[i];
        (get_text(r.exts_edit), r.exts.clone())
    };
    if text == old {
        return;
    }
    if panel.rows[i].locked_exts {
        set_text(panel.rows[i].exts_edit, &old);
        return;
    }
    let list: Vec<String> = text
        .split([',', '，', ' ', '；', ';'])
        .map(|s| s.trim().trim_start_matches('.').to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let mut uniq: Vec<String> = Vec::new();
    for e in list {
        if !uniq.contains(&e) {
            uniq.push(e);
        }
    }
    if crate::menu::apply_category_exts(&panel.rows[i].name, uniq) {
        let joined = panel.rows[i].exts.clone();
        panel.rows[i].exts = joined.clone();
        set_text(panel.rows[i].exts_edit, &joined);
    } else {
        set_text(panel.rows[i].exts_edit, &old);
    }
}

fn send_focus_row(hwnd: HWND, idx: usize) {
    if let Some(panel) = unsafe { panel_of(hwnd) } {
        if let Some(r) = panel.rows.get(idx) {
            unsafe {
                let _ = SetFocus(r.edit);
                let _ = SendMessageW(r.edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            }
        }
    }
}

fn get_text(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        GetWindowTextW(h, &mut buf);
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }
}

fn set_text(h: HWND, s: &str) {
    let w = shell::wide(s);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR::from_raw(w.as_ptr()));
    }
}

pub(crate) fn create_dialog_font() -> HFONT {
    unsafe {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            return CreateFontIndirectW(&ncm.lfMessageFont);
        }
        HFONT(0) // 取系统字体失败时控件用默认字体,不影响功能
    }
}

// ---- 尺寸(物理像素,按主屏 DPI 缩放) ----
fn row_h(s: f32) -> f32 {
    (26.0 * s).round().max(20.0)
}
fn row_gap(s: f32) -> f32 {
    (6.0 * s).round().max(4.0)
}
fn pad(s: f32) -> f32 {
    (10.0 * s).round().max(8.0)
}
fn del_w(s: f32) -> f32 {
    (26.0 * s).round().max(22.0)
}
fn panel_w(s: f32) -> f32 {
    (430.0 * s).round().max(380.0)
}
fn name_w(s: f32) -> f32 {
    (110.0 * s).round().max(90.0)
}
fn panel_size_for(scale: f32, rows: usize) -> (i32, i32) {
    let client_h =
        pad(scale) as i32 + (rows as i32 + 1) * (row_h(scale) as i32 + row_gap(scale) as i32);
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: panel_w(scale) as i32,
        bottom: client_h,
    };
    unsafe {
        let _ = AdjustWindowRectEx(
            &mut rc,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOOLWINDOW,
        );
    }
    (rc.right - rc.left, rc.bottom - rc.top)
}
