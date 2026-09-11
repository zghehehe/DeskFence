//! 重命名子系统(2026-09-09 从 ui.rs 原样搬出,纯搬家不改行为):
//! 栅栏标题就地改名、文件就地重命名(EDIT 子类:多行行数自适应/IME 兜底/
//! 预选扩展名之前)、点击外部提交(WH_MOUSE_LL 钩子 + 40ms 轮询兜底,与
//! Explorer 同款)、扫描宽恕登记(mark_scan_removed)、双击打开延迟登记
//! (pending_open/last_icon_up_ms)。
//! 本模块属于 ui.rs 拆分增量;与 ui.rs 双向依赖(同 crate 内合法)。

use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetDC, GetMonitorInfoW, GetTextExtentPoint32W,
    GetTextMetricsW, MonitorFromWindow, ReleaseDC, SelectObject, HFONT, LOGFONTW, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, TEXTMETRICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::{self, Rect};
use crate::shell;
use crate::ui::*;

static RENAME_OLD_PROC: std::sync::OnceLock<isize> = std::sync::OnceLock::new();

unsafe extern "system" fn default_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ---------------- 重命名点击外部提交(与 Explorer 行为一致) ----------------
// Explorer 的就地重命名在"点击编辑框以外的任何地方"时提交。栅栏窗口是
// WS_EX_NOACTIVATE、壁纸宿主也不抢焦点，WM_KILLFOCUS 不会到来，因此用
// 三条互补路径保证真实点击一定能退出：
// 1) 栅栏窗口内的按下(handle_lbuttondown/rbuttonup 直接提交)
// 2) 重命名期间安装的 WH_MOUSE_LL 钩子：任何真实鼠标按下(含壁纸/其它应用)
// 3) 全局定时器兜底：检测到左键按下且光标在编辑框外

static RENAME_MOUSE_HOOK: Mutex<Option<HHOOK>> = Mutex::new(None);
pub(crate) const TIMER_RENAME_WATCH: usize = 4;

pub(crate) fn point_in_window_rect(hwnd: HWND, x: i32, y: i32) -> bool {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// 编辑框外的鼠标按下 → 提交。fence_title=true 提交栅栏标题编辑框。
pub(crate) fn rename_click_outside_hit(x: i32, y: i32, _src: &str) -> bool {
    let (fence_edit, file_edit) = {
        let s = state().lock().unwrap();
        (s.rename_edit, s.file_rename_edit)
    };
    let mut handled = false;
    if let Some(edit) = fence_edit {
        if point_in_window_rect(edit, x, y) {
            return false;
        }
        unsafe {
            let _ = PostMessageW(edit, RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
        }
        handled = true;
    }
    if let Some(edit) = file_edit {
        if point_in_window_rect(edit, x, y) {
            return handled;
        }
        unsafe {
            let _ = PostMessageW(edit, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
        }
        handled = true;
    }
    handled
}

unsafe extern "system" fn rename_mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if ncode as u32 == HC_ACTION {
            let down = wparam.0 as u32 == WM_LBUTTONDOWN || wparam.0 as u32 == WM_RBUTTONDOWN;
            if down {
                let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                rename_click_outside_hit(ms.pt.x, ms.pt.y, "llhook");
            }
        }
        CallNextHookEx(None, ncode, wparam, lparam)
    }
}

fn install_rename_mouse_hook() {
    unsafe {
        {
            let mut slot = RENAME_MOUSE_HOOK.lock().unwrap();
            if slot.is_some() {
                return;
            }
            if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(rename_mouse_proc), hinstance(), 0) {
                if h.0 != 0 {
                    *slot = Some(h);
                }
            }
        }
        // 高频兜底:40ms 轮询真实按键状态(钩子被系统摘除/事件被安全软件
        // 吞掉时仍能检测到"点击外部"并提交,与 Explorer 行为一致)
        if let Some(tray) = TRAY_HWND.get().copied() {
            let _ = SetTimer(tray, TIMER_RENAME_WATCH, 40, None);
        }
    }
}

fn uninstall_rename_mouse_hook() {
    {
        let mut slot = RENAME_MOUSE_HOOK.lock().unwrap();
        if let Some(h) = slot.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
    }
    // 无任何重命名编辑框时停掉轮询定时器
    let any_edit = {
        let s = state().lock().unwrap();
        s.rename_edit.is_some() || s.file_rename_edit.is_some()
    };
    if !any_edit {
        if let Some(tray) = TRAY_HWND.get().copied() {
            unsafe {
                let _ = KillTimer(tray, TIMER_RENAME_WATCH);
            }
        }
    }
}

/// 栅栏标题重命名也需要同样的钩子保护（原逻辑只靠定时器光标判断，行为偏差）
pub(crate) fn start_rename(fence_id: u32) {
    let (title, rect) = {
        let s = state().lock().unwrap();
        if s.rename_fence.is_some() {
            return;
        }
        let title = s
            .fences
            .iter()
            .find(|f| f.id == fence_id)
            .map(|f| f.title.clone())
            .unwrap_or_else(|| "栅栏".to_string());
        let rect = s
            .fences
            .iter()
            .find(|f| f.id == fence_id)
            .map(|f| f.rect)
            .unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 60.0,
            });
        (title, rect)
    };
    unsafe {
        // 用独立 popup 窗口代替子控件：分层窗口上的子控件渲染不可靠
        // 类名必须是合法的宽字符串（窄字节强转会变乱码导致找不到 EDIT 类）
        let edit_cls = shell::wide("EDIT");
        let edit = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(edit_cls.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_POPUP.0 | WS_BORDER.0 | WS_VISIBLE.0 | ES_AUTOHSCROLL as u32),
            rect.x as i32 + 14,
            rect.y as i32 + 3,
            150,
            20,
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        );
        if edit.0 == 0 {
            return;
        }
        let w = shell::wide(&title);
        let _ = SetWindowTextW(edit, PCWSTR::from_raw(w.as_ptr()));
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, rename_edit_proc as *const () as isize);
        let _ = RENAME_OLD_PROC.set(old);
        SetWindowLongPtrW(edit, GWLP_USERDATA, fence_id as isize);
        {
            let mut s = state().lock().unwrap();
            s.rename_fence = Some(fence_id);
            s.rename_edit = Some(edit);
        }
        install_rename_mouse_hook();
        let _ = SetForegroundWindow(edit);
        SetFocus(edit);
        let _ = SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
        let _ = SetWindowPos(
            edit,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

unsafe extern "system" fn rename_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let old = RENAME_OLD_PROC
        .get()
        .copied()
        .unwrap_or(default_edit_proc as *const () as isize);
    match msg {
        WM_KEYDOWN => {
            if wparam.0 == VK_RETURN.0 as usize {
                let _ = PostMessageW(hwnd, RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            if wparam.0 == VK_ESCAPE.0 as usize {
                let _ = PostMessageW(hwnd, RENAME_CANCEL_MSG, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS => {
            let _ = PostMessageW(hwnd, RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        WM_CANCELMODE => {
            let _ = PostMessageW(hwnd, RENAME_CANCEL_MSG, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            if (wparam.0 as u32 & 0xFFFF) == 0 {
                let _ = PostMessageW(hwnd, RENAME_CANCEL_MSG, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        RENAME_COMMIT_MSG => {
            commit_rename(hwnd);
            return LRESULT(0);
        }
        RENAME_CANCEL_MSG => {
            cancel_rename(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, old);
            return LRESULT(0);
        }
        _ => {}
    }
    CallWindowProcW(
        Some(std::mem::transmute::<
            isize,
            unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
        >(old)),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

fn commit_rename(edit: HWND) {
    let fence_id = unsafe { GetWindowLongPtrW(edit, GWLP_USERDATA) as u32 };
    let mut buf = [0u16; 128];
    unsafe {
        let _ = GetWindowTextW(edit, &mut buf);
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let new_title = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    let new_title = if new_title.is_empty() {
        "栅栏".to_string()
    } else {
        new_title
    };
    {
        let mut s = state().lock().unwrap();
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
            f.title = new_title;
        }
        s.rename_fence = None;
        s.rename_edit = None;
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = DestroyWindow(edit);
    }
    refresh_fence(fence_id);
}

fn cancel_rename(edit: HWND) {
    {
        let mut s = state().lock().unwrap();
        s.rename_fence = None;
        s.rename_edit = None;
    }
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = DestroyWindow(edit);
    }
}

// ---------------- 文件就地重命名 ----------------
// 系统右键菜单里的"重命名"被拦截后在这里完成:一个 EDIT 覆盖在图标名标签上,
// 字体/预选行为与 Explorer 一致,回车或失焦提交,Esc 取消。

pub(crate) const FILE_RENAME_COMMIT_MSG: u32 = WM_USER + 3;
/// 重命名框行数自适应定时器(120ms):IME 提交不产生 WM_CHAR,纯消息钩子
/// 会漏掉中文输入——定时器兜底覆盖一切改动来源。id 取高位避开 EDIT 内部
/// 小整数定时器 id(互不覆盖)
const RENAME_FIT_TIMER: usize = 0x4DF5;
const WM_IME_COMPOSITION: u32 = 0x010F;
const FILE_RENAME_CANCEL_MSG: u32 = WM_USER + 4;
static FILE_RENAME_OLD_PROC: OnceLock<isize> = OnceLock::new();
pub(crate) static FILE_RENAME_PATH: Mutex<Option<String>> = Mutex::new(None);
/// 提交防重入门闩:回车/失焦/WM_ACTIVATE/点击外部轮询可在同一帧叠加多条
/// 提交消息,重入会对同一编辑框提交两次。历史上失败路径的模态 MessageBox
/// 弹出→编辑框失焦→自动提交路径 Post 新提交→模态循环把新提交分发→重入
/// 失败→再弹框……同秒几十次 commit/failed 占死 UI 线程(2026-09-11 用户
/// 实测"回车即卡死"),模态框删除后此门闩继续兜住多源叠加。
static FILE_RENAME_COMMITTING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// 改名提交后置位:rescan 的"无变化早退"必须跳过一次。改名提交已把内存
/// 文件列表同步到新路径/新分类,磁盘扫描结果与内存全一致,任何 diff 都看
/// 不出变化——但新分类缺栅栏时 ensure_missing_category_fences 必须跑一遍,
/// 否则改名成 mp4 的文件无栅栏可归=隐身(2026-09-03 实测"改名后不再自动
/// 建分类栅栏"的根因:内存同步把变化对 rescan 藏住了)
pub(crate) static RENAME_RESCAN_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ---------------- 扫描宽恕(2026-09-03) ----------------
/// 路径→连续扫描未遇次数。刚消失的文件连续 SCAN_MISS_DROP 轮扫不到才真正
/// 移除:新建/写入中的文件元数据可能被创建方进程短暂锁住,单轮扫描漏掉
/// 就把在册文件当"消失"会引发栅栏重排、位置漂移(用户实测"文档自动移位")。
/// 应用主动删除的路径用 mark_scan_removed 立即达阈值,不拖尾巴。
pub(crate) const SCAN_MISS_DROP: u32 = 2;
pub(crate) fn scan_miss_map() -> &'static Mutex<std::collections::HashMap<String, u32>> {
    static M: OnceLock<Mutex<std::collections::HashMap<String, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn mark_scan_removed(paths: &[String]) {
    // 应用主动删除=内存状态在扫描器背后变了:作废在途异步快照,防止它把
    // 已删路径当"新增"混回内存列表(见 ui.rs SCAN_EPOCH)
    crate::ui::invalidate_pending_scans();
    let mut miss = scan_miss_map().lock().unwrap();
    for p in paths {
        miss.insert(p.clone(), SCAN_MISS_DROP);
    }
}

// ---------------- 双击打开延迟执行(2026-09-04 兼顾两种手势) ----------------
// DBLCLK 先登记"待打开"而不立即执行;随后的 UP 判定:与上一次图标 UP 的
// 间隔 > 系统双击时长 = 慢双击改名意图(取消打开、进入改名);否则执行
// 打开。快速双击=打开、慢双击=改名,互不误伤,与原生同款时序语义。
pub(crate) fn pending_open() -> &'static Mutex<Option<String>> {
    static M: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}
pub(crate) fn last_icon_up_ms() -> &'static std::sync::atomic::AtomicU64 {
    static M: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    M.get_or_init(|| std::sync::atomic::AtomicU64::new(0))
}

/// 系统菜单"重命名"拦截回调(shell.rs 在 init 时注册)
pub(crate) fn on_shell_rename_request(path: &str) {
    start_file_rename(path.to_string());
}

pub(crate) fn start_file_rename(path: String) {
    // Do not create a second editor while another rename is active.
    {
        let s = state().lock().unwrap();
        if s.rename_fence.is_some() || s.rename_edit.is_some() || s.file_rename_edit.is_some() {
            return;
        }
    }
    // 回收站虚拟条目不可重命名
    if model::is_recycle_bin(&path) {
        return;
    }
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // 定位该文件所在栅栏的格子,把编辑框盖在名字标签上;找不到就放光标旁。
    // 宽度按 Explorer 的编辑框策略处理:短名按内容收窄,长名在 cell
    // 级上限内换行;外框始终以固定 cell 中心为锚,不随当前宽度漂移。
    // 同时记录所在栅栏:改名期间该成员标签由编辑框替代(隐藏),建好编辑框
    // 后强制刷新所在栅栏立即生效。
    let (edit_x, edit_y, edit_w, edit_h, host_fence, edit_metrics) = {
        let s = state().lock().unwrap();
        let mut pos = None;
        let mut host = None;
        let mut edit_metrics = None;
        'outer: for f in &s.fences {
            let items = model::display_list(f, &s.files);
            for (i, it) in items.iter().enumerate() {
                if it.path == path {
                    let m = s
                        .metrics
                        .get(&f.id)
                        .copied()
                        .unwrap_or_else(model::DpiMetrics::system);
                    let lay = model::layout_with_metrics(f, items.len(), &m);
                    let (cx, cy) = model::cell_pos_with_metrics(&lay, i, &m);
                    let cs = m.icon_px;
                    // 编辑框两行高起步;宽度按 cell 上限对齐原生(2026-09-03 editprobe 复刻
                    // EDIT 实测 + 两轮用户截图逐像素比对):原生框外宽按 cell
                    // 上限加边框余量,水平中心与图标格一致;短名在后续调整中收窄。
                    let label_top = f.rect.y + cy + (6.5 + 2.0) * m.scale + cs;
                    let label_h = (2.0 * 24.0 + 6.0) * m.scale;
                    let edit_w = (m.cell_w - 2.0 * m.scale).round() as i32;
                    let cell_center_x = f.rect.x + cx + m.cell_w * 0.5;
                    pos = Some((
                        (cell_center_x - edit_w as f32 * 0.5).round() as i32,
                        label_top.round() as i32,
                        edit_w,
                        label_h.round() as i32,
                    ));
                    host = Some(f.id);
                    edit_metrics = Some(m);
                    break 'outer;
                }
            }
        }
        pos.map(|(x, y, w, h)| {
            (
                x,
                y,
                w,
                h,
                host,
                edit_metrics.unwrap_or_else(model::DpiMetrics::system),
            )
        })
        .unwrap_or_else(|| {
            let (sx, sy) = screen_cursor();
            let m = model::DpiMetrics::system();
            let h = ((2.0 * 24.0 + 6.0) * m.scale).round() as i32;
            (
                sx as i32 - 80,
                sy as i32 - 12,
                (model::cell_w() + 4.0 * m.scale).round() as i32,
                h,
                None,
                m,
            )
        })
    };
    // 先设 PATH 再建编辑框:绘制路径据此隐藏该成员标签(与原生一致)
    *FILE_RENAME_PATH.lock().unwrap() = Some(path.clone());
    unsafe {
        let edit_cls = shell::wide("EDIT");
        let edit = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(edit_cls.as_ptr()),
            PCWSTR::null(),
            // 多行自动换行(2026-09-02 与原生一致):长名向下换行、框随行数
            // 增高(见 adjust_rename_edit_height);不加 ES_AUTOHSCROLL——
            // 多行加它会把换行变成横向滚动。
            // ES_CENTER(2026-09-03 用户截图比对确认):原生每行水平居中,
            // 首行/末行短行明显缩进;此前把原生样式 0x540000C5 的 0x1 位
            // 误读成 ES_LEFT(其值本为 0,无效果)。样式余下部分=
            // MULTILINE|AUTOVSCROLL|NOHIDESEL;WS_POPUP 是结构必需(ULW
            // 分层窗口不能挂子窗口),原生为 WS_CHILD。
            // WS_BORDER(2026-09-04 用户对照):原生框有 1px 描边,栅栏
            // 无边框白块与原生观感差异明显
            WINDOW_STYLE(
                WS_POPUP.0
                    | WS_VISIBLE.0
                    | WS_BORDER.0
                    | ES_CENTER as u32
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_NOHIDESEL as u32,
            ),
            edit_x,
            edit_y,
            edit_w,
            edit_h,
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        );
        if edit.0 == 0 {
            *FILE_RENAME_PATH.lock().unwrap() = None;
            return;
        }
        if let Some(fid) = host_fence {
            refresh_fence(fid); // 立即重绘:标签隐去,编辑框取而代之
        }
        // 直接复用 SPI_GETICONTITLELOGFONT 的完整原生字体；查询失败时
        // 使用按目标栅栏 DPI 缩放的最小回退字体。
        let lf = shell::icon_title_logfont().unwrap_or_else(|| {
            let mut fallback: LOGFONTW = std::mem::zeroed();
            fallback.lfHeight = -((16.0 * edit_metrics.scale).round() as i32);
            fallback.lfWeight = 400;
            fallback
        });
        let font = CreateFontIndirectW(&lf);
        if !font.is_invalid() {
            let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        {
            let mut s = state().lock().unwrap();
            s.rename_metrics.insert(edit.0, edit_metrics);
            s.rename_centers.insert(
                edit.0,
                (edit_x as f32 + edit_w as f32 * 0.5).round() as i32,
            );
            if !font.is_invalid() {
                s.rename_fonts.insert(edit.0, font);
            }
        }
        let w = shell::wide(&name);
        let _ = SetWindowTextW(edit, PCWSTR::from_raw(w.as_ptr()));
        // 与 Explorer 一致:预选扩展名之前的部分。注意 EM_SETSEL 用
        // UTF-16 字符下标——旧实现直接用 UTF-8 字节下标,中文名会溢出到
        // 末尾把扩展名也选中(与原生不一致)
        let sel_end = name
            .rfind('.')
            .filter(|&p| p > 0)
            .map(|p| name[..p].encode_utf16().count() as isize)
            .unwrap_or(-1);
        let _ = SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(sel_end));
        // 初始名就是长名时(多行换行)先按行数增高;改名期间定时器兜底
        adjust_rename_edit_height(edit);
        let _ = SetTimer(edit, RENAME_FIT_TIMER, 120, None);
        *FILE_RENAME_PATH.lock().unwrap() = Some(path);
        let old = SetWindowLongPtrW(
            edit,
            GWLP_WNDPROC,
            file_rename_edit_proc as *const () as isize,
        );
        let _ = FILE_RENAME_OLD_PROC.set(old);
        SetFocus(edit);
        let _ = SetWindowPos(
            edit,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        state().lock().unwrap().file_rename_edit = Some(edit);
        install_rename_mouse_hook();
        log("file rename edit created");
        // 前台化编辑框:栅栏窗口是 WS_EX_NOACTIVATE,不抢焦点;若不前台化,
        // 真实键盘输入(Esc/回车/文字)会进到其它前台窗口,用户无法编辑
        let _ = SetForegroundWindow(edit);
        SetFocus(edit);
    }
}

unsafe extern "system" fn file_rename_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let old = FILE_RENAME_OLD_PROC
        .get()
        .copied()
        .unwrap_or(default_edit_proc as *const () as isize);
    match msg {
        WM_KEYDOWN => {
            if wparam.0 == VK_RETURN.0 as usize {
                let _ = PostMessageW(hwnd, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            if wparam.0 == VK_ESCAPE.0 as usize {
                let _ = PostMessageW(hwnd, FILE_RENAME_CANCEL_MSG, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            // 多行重命名(与原生一致):删除键可能减少行数
            let r = CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            adjust_rename_edit_height(hwnd);
            return r;
        }
        WM_IME_COMPOSITION => {
            // 中文经输入法提交,不走 WM_CHAR——这里必须兜住
            let r = CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            adjust_rename_edit_height(hwnd);
            return r;
        }
        WM_TIMER if wparam.0 == RENAME_FIT_TIMER => {
            // 120ms 兜底:任何来源(IME/粘贴/程序性)的文本变化都收敛
            adjust_rename_edit_height(hwnd);
            return LRESULT(0);
        }
        WM_CHAR => {
            if matches!(wparam.0 as u32, 0x0D | 0x0A) {
                // 多行 EDIT 的换行最终经 WM_CHAR 落进文本——回车必须在此
                // 兜住(WM_KEYDOWN 已拦,但 IME/前台转移等路径可能把回车直接
                // 以 WM_CHAR 形式送达;2026-09-11 实测漏网一次=文件名里混进
                // 换行触发后续失败循环)
                let _ = PostMessageW(hwnd, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            // 多行重命名(与原生一致):输入/粘贴后按实际换行行数增高编辑框
            let r = CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            adjust_rename_edit_height(hwnd);
            return r;
        }
        WM_PASTE => {
            // 与原生一致:粘贴文本的换行折叠成空格、其余控制字符剔除——
            // 多行 EDIT 直接接纳粘贴会把非法换行带进文件名
            if let Some(raw) = sanitized_clipboard_text() {
                let clean = collapse_for_filename(&raw);
                if !clean.is_empty() {
                    const EM_REPLACESEL: u32 = 0x00C2;
                    let w = shell::wide(&clean);
                    let _ = SendMessageW(
                        hwnd,
                        EM_REPLACESEL,
                        WPARAM(1),
                        LPARAM(w.as_ptr() as isize),
                    );
                }
                adjust_rename_edit_height(hwnd);
                return LRESULT(0);
            }
            let r = CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            adjust_rename_edit_height(hwnd);
            return r;
        }
        WM_KILLFOCUS | WM_CANCELMODE => {
            let _ = PostMessageW(hwnd, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            // 仅失活时提交;编辑框被激活(前台化)不能当作"点击外部"
            if (wparam.0 as u32 & 0xFFFF) == 0 {
                let _ = PostMessageW(hwnd, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        FILE_RENAME_COMMIT_MSG => {
            commit_file_rename(hwnd);
            return LRESULT(0);
        }
        FILE_RENAME_CANCEL_MSG => {
            cancel_file_rename(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, RENAME_FIT_TIMER);
            return CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
        }
        WM_NCDESTROY => {
            let owned = state()
                .lock()
                .unwrap()
                .file_rename_edit
                .map(|edit| edit == hwnd)
                .unwrap_or(false);
            if owned {
                *FILE_RENAME_PATH.lock().unwrap() = None;
                let ids = {
                    let mut s = state().lock().unwrap();
                    s.file_rename_edit = None;
                    s.rename_metrics.remove(&hwnd.0);
                    s.rename_centers.remove(&hwnd.0);
                    s.fences.iter().filter(|f| !f.hidden).map(|f| f.id).collect::<Vec<_>>()
                };
                uninstall_rename_mouse_hook();
                let font = state().lock().unwrap().rename_fonts.remove(&hwnd.0);
                if let Some(font) = font {
                    let _ = DeleteObject(font);
                }
                for id in ids {
                    refresh_fence(id);
                }
            }
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, old);
            return CallWindowProcW(
                Some(std::mem::transmute::<
                    isize,
                    unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                >(old)),
                hwnd,
                msg,
                wparam,
                lparam,
            );
        }
        _ => {}
    }
    CallWindowProcW(
        Some(std::mem::transmute::<
            isize,
            unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
        >(old)),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

/// 多行重命名框(与原生一致):按当前文本的实际换行行数增高编辑框。
/// 输入/粘贴/删除后由编辑框子类过程调用。
fn adjust_rename_edit_height(edit: HWND) {
    unsafe {
        const EM_GETLINECOUNT: u32 = 0x00BA;
        const EM_GETRECT: u32 = 0x00B2;
        const EM_SCROLLCARET: u32 = 0x00B7;
        let mut rc = RECT::default();
        let _ = GetWindowRect(edit, &mut rc);
        let hdc = GetDC(edit);
        if hdc.is_invalid() {
            return;
        }
        let font = SendMessageW(edit, WM_GETFONT, WPARAM(0), LPARAM(0)).0;
        let old_font = if font != 0 {
            Some(SelectObject(hdc, HFONT(font as _)))
        } else {
            None
        };
        // —— 宽度:整名宽度+原生 EDIT 外框留白,短名收窄、长名在 cell 内换行 ——
        // 宽度只由一行文字测量结果决定,再受 cell 上限约束,避免换行和宽度
        // 互相反馈造成抖动。
        let len = GetWindowTextLengthW(edit);
        let mut text_w = 0.0f32;
        if len > 0 {
            let mut buf = vec![0u16; len as usize + 1];
            let _ = GetWindowTextW(edit, &mut buf);
            let mut sz = SIZE::default();
            if GetTextExtentPoint32W(hdc, &buf[..len as usize], &mut sz).as_bool() {
                text_w = sz.cx as f32;
            }
        }
        let m = state()
            .lock()
            .unwrap()
            .rename_metrics
            .get(&edit.0)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        // Match the EDIT's own formatting rectangle instead of estimating its
        // wrapping width from the outer window alone.
        // 换行宽 = 名字宽×0.55,夹 [66, 格宽-2*scale](2026-09-04 用 GDI
        // TextRenderer 精确实测后修正:原生有效换行宽窗口 [60,72)——
        // "v4flash测"(60)留在首行、"+试"(72)换行;32字长名 6字/行(≥108);
        // 恒定宽无法同时满足,原生换行宽随名字收放。取窗口中值 66 安全。
        // 外框 = 换行宽+12(内建边距L3/R5+WS_BORDER 2px)。
        // 系数 0.69:应用内实测 text_w(v4flash测试.txt)=123,原生同名的
        // 可见换行点在"测|试"→ 原生有效换行宽 ≈ 85(窗口 [82,97)),0.69×123
        // = 85 正中;xxx.txt(43)→ 66 保单行;长名(690)→ 封顶 111 保 6 字/行。
        let fmt_w = ((text_w * 0.69).round() as i32)
            .clamp(66, (m.cell_w - 2.0 * m.scale).round() as i32);
        let new_w = fmt_w + 10;
        // 先应用宽度:换行随之更新,后续行数/高度按新宽计算(同轮收敛)
        if new_w != rc.right - rc.left {
            let _ = SetWindowPos(
                edit,
                HWND(0),
                0,
                0,
                new_w,
                0,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // —— 高度:实际换行行数×行高+贴身边距(原生单行框≈行高+8) ——
        // 旧的 行数+1 余量槽在单行名时多出整整一行空白(用户对照反馈),
        // 余量槽退役;防裁切由 EM_GETLINECOUNT 实时准确(editprobe 实测)
        // + 显示器封顶兜底承担。(行数在宽度 SetWindowPos 后取:换行已同步)
        let mut tm = TEXTMETRICW::default();
        let line_h = if GetTextMetricsW(hdc, &mut tm).as_bool() {
            (tm.tmHeight + tm.tmExternalLeading) as f32
        } else {
            20.0
        };
        if let Some(of) = old_font {
            SelectObject(hdc, of);
        }
        ReleaseDC(edit, hdc);
        let _ = GetWindowRect(edit, &mut rc); // 宽度改后刷新矩形(换行已同步)
        let mut format = RECT::default();
        let _ = SendMessageW(
            edit,
            EM_GETRECT,
            WPARAM(0),
            LPARAM((&mut format as *mut RECT) as isize),
        );
        let format_ok = format.right > format.left && format.bottom > format.top;
        let lines = SendMessageW(edit, EM_GETLINECOUNT, WPARAM(0), LPARAM(0)).0.max(1) as f32;
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let in_mon = GetMonitorInfoW(MonitorFromWindow(edit, MONITOR_DEFAULTTONEAREST), &mut mi)
            .as_bool();
        let (mon_left, mon_top, mon_right, mon_bottom) = if in_mon {
            (
                mi.rcMonitor.left as i32,
                mi.rcMonitor.top as i32,
                mi.rcMonitor.right as i32,
                mi.rcMonitor.bottom as i32,
            )
        } else {
            let (vx, vy, vw, vh) = work_area();
            (vx as i32, vy as i32, (vx + vw) as i32, (vy + vh) as i32)
        };
        // 位置:水平保持"对格居中"(创建时即格居中,宽度缩放后中心不动——
        // 2026-09-04 用户对照:原生框居中于图标正下方,左对齐=歪到格边);
        // 垂直顶在标签起点,向下生长;越界时整体收回显示器内
        let old_w = rc.right - rc.left;
        let current_center = {
            let s = state().lock().unwrap();
            s.rename_centers
                .get(&edit.0)
                .copied()
                .unwrap_or(rc.left + old_w / 2)
        };
        let mut left = current_center - new_w / 2;
        let mut top = rc.top;
        let available_h = (mon_bottom - mon_top).max(1);
        // Keep a one-line name one line tall. The EDIT's measured line height
        // already includes the native font metrics; only the border inset is extra.
        let client_h = (rc.bottom - rc.top).max(1);
        let format_h = if format_ok {
            (format.bottom - format.top).max(1)
        } else {
            client_h.saturating_sub((8.0 * m.scale).round() as i32).max(1)
        };
        let vertical_pad = (client_h - format_h).max((4.0 * m.scale).round() as i32);
        let min_h = (line_h.round() as i32).saturating_add(vertical_pad);
        let new_h = ((lines * line_h).round() as i32)
            .saturating_add(vertical_pad)
            .clamp(min_h.min(available_h), available_h);
        top = top.clamp(mon_top, (mon_bottom - new_h).max(mon_top));
        left = left.clamp(mon_left, (mon_right - new_w).max(mon_left));
        if left != rc.left || top != rc.top {
            let _ = SetWindowPos(
                edit,
                HWND(0),
                left,
                top,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if new_h != rc.bottom - rc.top || new_w != old_w {
            let _ = SetWindowPos(
                edit,
                HWND(0),
                0,
                0,
                new_w,
                new_h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // 尾部行在框内时把光标滚回可见区:改名起点就与原生一致,末尾可直达
        log(&format!(
            "rename fit: text_w={text_w} fmt_w={fmt_w} new_w={new_w} lines={lines} new_h={new_h} left={left} top={top}"
        ));
        let _ = SendMessageW(edit, EM_SCROLLCARET, WPARAM(0), LPARAM(0));
    }
}

fn commit_file_rename(edit: HWND) {
    // 防重入:同一帧叠加的多条提交只处理第一条(见 FILE_RENAME_COMMITTING)
    if FILE_RENAME_COMMITTING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let outcome = commit_file_rename_once(edit);
    FILE_RENAME_COMMITTING.store(false, std::sync::atomic::Ordering::Release);
    if outcome.renamed {
        // 内存已同步,磁盘扫描与内存一致,rescan_now 的 diff 必为空——
        // 置强制位让缺类补建跑一遍(改名成 mp4 要能冒出媒体栅栏)
        RENAME_RESCAN_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
        // 同步版 rescan:调用返回即应用完毕(异步版无法保证下面迁移动画的
        // 排队顺序:目标分类栅栏必须已就位)
        rescan_now();
        // 跨栏迁移动画必须在扫描应用之后排队:目标分类栅栏(可能新建)已
        // 就位、新栏帧已渲染,这里排队并刷新新栏把成员藏到落地显形
        if let Some((new_path, old_fid, fx, fy)) = outcome.migration {
            queue_migration_animations(&[(new_path, old_fid, fx, fy)]);
        }
    }
}

struct CommitOutcome {
    renamed: bool,
    migration: Option<(String, u32, f32, f32)>,
}

/// 失败/非法名的非阻塞处置(与原生 Explorer 同款):响系统错误音、编辑框
/// 保持打开并全选,用户改完可再提交,Esc 取消。**绝不能用模态 MessageBox**:
/// 弹框令编辑框失焦→自动提交路径 Post 新提交→模态循环分发→重入失败→
/// 再弹框的自激死循环(2026-09-11 卡死根因)。
fn rename_failure_feedback(edit: HWND) {
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    unsafe {
        let _ = MessageBeep(MB_ICONERROR);
        let _ = SetFocus(edit);
        let _ = SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
    }
}

fn destroy_file_rename_edit(edit: HWND) {
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = KillTimer(edit, RENAME_FIT_TIMER);
        let _ = DestroyWindow(edit);
    }
}

fn commit_file_rename_once(edit: HWND) -> CommitOutcome {
    log("file rename commit");
    let old_path = FILE_RENAME_PATH.lock().unwrap().clone().unwrap_or_default();
    if old_path.is_empty() {
        destroy_file_rename_edit(edit);
        return CommitOutcome { renamed: false, migration: None };
    }
    let len = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    unsafe {
        let _ = GetWindowTextW(edit, &mut buf);
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let new_name = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    let old_name = std::path::Path::new(&old_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // 空名/没改:关闭编辑框,桌面状态不动(与原生一致)
    if new_name.is_empty() || new_name == old_name {
        destroy_file_rename_edit(edit);
        return CommitOutcome { renamed: false, migration: None };
    }
    // 与 Explorer 相同的非法字符集合,外加全部控制字符(\n\r\t 等——
    // NTFS 文件名禁控制字符,漏检会走到 rename 必败路径)
    let invalid = new_name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    if invalid {
        log(&format!("file rename rejected (illegal chars): {new_name}"));
        rename_failure_feedback(edit);
        return CommitOutcome { renamed: false, migration: None };
    }
    let renamed = shell::rename_path(&old_path, &new_name);
    if !renamed {
        log(&format!("file rename failed: {old_path} -> {new_name}"));
        rename_failure_feedback(edit);
        return CommitOutcome { renamed: false, migration: None };
    }
    // 同步被拖入栅栏的 pinned 引用,指向新路径
    let new_path = std::path::Path::new(&old_path)
        .parent()
        .map(|p| p.join(&new_name).to_string_lossy().to_string())
        .unwrap_or_else(|| old_path.clone());
    let mut migration: Option<(String, u32, f32, f32)> = None;
    {
        let mut s = state().lock().unwrap();
        for f in s.fences.iter_mut() {
            for p in f.pinned.iter_mut().chain(f.item_order.iter_mut()) {
                if *p == old_path {
                    *p = new_path.clone();
                }
            }
        }
        if s.selected_paths.remove(&old_path) {
            s.selected_paths.insert(new_path.clone());
        }
        if s.focused_path.as_deref() == Some(&old_path) {
            s.focused_path = Some(new_path.clone());
        }
        if s.selection_anchor.as_deref() == Some(&old_path) {
            s.selection_anchor = Some(new_path.clone());
        }
        // 同步内存文件列表(2026-09-02,与原生一致):改名立即生效,不等
        // 异步 rescan——消除"改名后双击旧路径(已不存在)"的窗口期。
        // category 必须随名重算(2026-09-03):txt 改名 mp4 后分类仍是
        // 旧值的话文件会永远留在原分类栅栏里;且下面的 rescan 早退
        // 只比路径,内存已同步路径后它必然早退,分类永远不会再算
        let old_cat = s
            .files
            .iter()
            .find(|f| f.path == old_path)
            .map(|f| (f.category.clone(), f.is_dir));
        let new_cat = model::categorize(
            &new_name,
            old_cat.as_ref().map(|(_, d)| *d).unwrap_or(false),
        );
        let cat_changed = old_cat.as_ref().is_some_and(|(c, _)| *c != new_cat);
        if cat_changed {
            // 用户主动把文件改名进某分类=明确意图,先清该分类墓碑:
            // 墓碑的"新文件"判定只看 mtime(改名不变 mtime),不清理的话
            // 墓碑挡住缺类补建,文件无栏可归=隐身(2026-09-03 用户实测
            // md→mp3 后媒体栅栏不建、文件失踪)
            clear_category_tombstone(&new_cat);
            // 迁移动画起点:必须在分类同步前抓取旧栏旧槽位
            // (auto_scroll=false:不为起飞而滚动旧栏)
            migration = fence_slot_screen_pos(&mut s, &old_path, false)
                .map(|(fid, p)| (new_path.clone(), fid, p.0, p.1));
        }
        for f in s.files.iter_mut() {
            if f.path == old_path {
                f.path = new_path.clone();
                f.name = new_name.clone();
                f.category = new_cat.clone();
            }
        }
    }
    destroy_file_rename_edit(edit);
    CommitOutcome { renamed: true, migration }
}

/// 读取剪贴板 CF_UNICODETEXT;取不到返回 None(调用方回落默认粘贴行为)
fn sanitized_clipboard_text() -> Option<String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let text = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hg = HGLOBAL(h.0 as *mut core::ffi::c_void);
            let p = GlobalLock(hg) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0usize;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(hg);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// 粘贴清洗(与原生一致):换行/制表折叠为单个空格,其余控制字符剔除
fn collapse_for_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        let mapped = if matches!(c, '\r' | '\n' | '\t') {
            Some(' ')
        } else if c.is_control() {
            None
        } else {
            Some(c)
        };
        match mapped {
            Some(' ') => {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            Some(c) => {
                out.push(c);
                prev_space = false;
            }
            None => {}
        }
    }
    out.trim().to_string()
}

fn cancel_file_rename(edit: HWND) {
    log("file rename cancel");
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = DestroyWindow(edit);
    }
}
