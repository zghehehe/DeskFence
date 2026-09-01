//! 窗口管理与交互：栅栏窗口、命中测试、移动/缩放/滚动、右键菜单、重命名、刷新

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    BOOL, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateFontIndirectW, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromRect,
    HBRUSH, HDC, HMONITOR, LOGFONTW, MONITORINFO, MONITOR_DEFAULTTONEAREST, ScreenToClient,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::CoInitializeEx;
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, TRACKMOUSEEVENT_FLAGS, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
// windows 0.52 未导出的 WinEvent 标志,按 WinUser.h 补定义
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::{self, Fence, FileItem, Hit, Rect};
use crate::ole;
use crate::render;
use crate::render::{IconBuffer, Renderer, Surface};
use crate::shell;

fn class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceFence\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

fn tray_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceTray\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

fn guide_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceGuide\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

/// 菜单前台宿主专用类:历史上复用栅栏类,外部探针与自家 drag_elevate_anchor
/// 的兄弟栅栏扫描都会把它误当真栅栏(2026-08-28 wdprobe 实测数出 6 个"栅栏")。
fn menu_host_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceMenuHost\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

fn hinstance() -> HINSTANCE {
    unsafe {
        HINSTANCE(
            windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap_or_default()
                .0,
        )
    }
}

fn deskfence_icon() -> HICON {
    unsafe { LoadIconW(hinstance(), PCWSTR(1usize as *const u16)).unwrap_or_default() }
}

/// 对齐模式(三档):"auto"=固定间隔自动对齐(默认,实时挤压+等距流式);
/// "grid"=按图标格宽/高的整数倍步进停靠;"free"=完全自由移动。
static ALIGN_MODE: Mutex<String> = Mutex::new(String::new());
pub fn align_mode() -> String {
    {
        let g = ALIGN_MODE.lock().unwrap();
        if !g.is_empty() {
            return g.clone();
        }
    }
    let m = model::load_settings().align_mode;
    *ALIGN_MODE.lock().unwrap() = m.clone();
    m
}
/// 写入对齐档位并立即持久化到设置文件
fn set_align_mode_stored(mode: &str) {
    *ALIGN_MODE.lock().unwrap() = mode.to_string();
    model::save_settings(&model::Settings {
        align_mode: mode.to_string(),
        render_mode: render_mode(),
        auto_category: auto_category(),
        desktop_state: desktop_state(),
        z_guard: z_guard_setting(),
        show_chrome: chrome_always_on(),
    });
}
pub fn auto_align_on() -> bool {
    align_mode() == "auto"
}
pub fn grid_align_on() -> bool {
    align_mode() == "grid"
}

/// 渲染模式:"transparent"=透明窗口(默认);"precise"=精确模式。
/// 2026-08-26 起两模式共用同一渲染管线(透明底+seeded GDI ClearType 文字),
/// 区别仅剩启动守卫:精确模式等首帧壁纸种子就绪再呈现,透明模式立即呈现
/// (种子缺失时黑种子兜底)。菜单勾选文案保留两档供用户选择。
static RENDER_MODE: Mutex<String> = Mutex::new(String::new());
pub fn render_mode() -> String {
    {
        let g = RENDER_MODE.lock().unwrap();
        if !g.is_empty() {
            return g.clone();
        }
    }
    let m = model::load_settings().render_mode;
    *RENDER_MODE.lock().unwrap() = m.clone();
    m
}
fn set_render_mode_stored(mode: &str) {
    *RENDER_MODE.lock().unwrap() = mode.to_string();
    model::save_settings(&model::Settings {
        align_mode: align_mode(),
        render_mode: mode.to_string(),
        auto_category: auto_category(),
        desktop_state: desktop_state(),
        z_guard: z_guard_setting(),
        show_chrome: chrome_always_on(),
    });
}

/// 桌面状态(持久化):normal=栅栏显示 / zen=纯净(只剩壁纸) / native=原生图标。
/// 切换即落盘,启动按此恢复;所有 Settings 落盘点都要带上当前值。
static DESKTOP_STATE: Mutex<String> = Mutex::new(String::new());
pub fn desktop_state() -> String {
    {
        let g = DESKTOP_STATE.lock().unwrap();
        if !g.is_empty() {
            return g.clone();
        }
    }
    let m = model::load_settings().desktop_state;
    *DESKTOP_STATE.lock().unwrap() = m.clone();
    m
}
fn set_desktop_state_stored(mode: &str) {
    *DESKTOP_STATE.lock().unwrap() = mode.to_string();
    model::save_settings(&model::Settings {
        align_mode: align_mode(),
        render_mode: render_mode(),
        auto_category: auto_category(),
        desktop_state: mode.to_string(),
        z_guard: z_guard_setting(),
        show_chrome: chrome_always_on(),
    });
}

/// 自动分类开关:默认 true=按固定 8 类自动归类;false=自定义分类模式
/// (不按扩展名,文件只进被拖入的栅栏,未分配的进"未分类"栅栏)
static AUTO_CATEGORY: Mutex<Option<bool>> = Mutex::new(None);
pub fn auto_category() -> bool {
    let mut g = AUTO_CATEGORY.lock().unwrap();
    if let Some(v) = *g {
        return v;
    }
    let v = model::load_settings().auto_category;
    model::set_auto_category(v);
    *g = Some(v);
    v
}
fn set_auto_category_stored(v: bool) {
    model::set_auto_category(v);
    *AUTO_CATEGORY.lock().unwrap() = Some(v);
    model::save_settings(&model::Settings {
        align_mode: align_mode(),
        render_mode: render_mode(),
        auto_category: v,
        desktop_state: desktop_state(),
        z_guard: z_guard_setting(),
        show_chrome: chrome_always_on(),
    });
}

/// z 守卫设置(缓存读取,模式同上):菜单落盘点需要带上当前值。
fn z_guard_setting() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| model::load_settings().z_guard)
}

/// 常显栅栏边框线(托盘开关,默认关=悬停/拖拽才浮现,2026-09-01 用户新增):
/// 开=全部栅栏常显边框/标题/角手柄,便于观察布局边界;关=无边框常显基线。
static SHOW_CHROME: AtomicBool = AtomicBool::new(false);
pub fn chrome_always_on() -> bool {
    SHOW_CHROME.load(Ordering::Relaxed)
}
fn set_show_chrome_stored(on: bool) {
    SHOW_CHROME.store(on, Ordering::Relaxed);
    model::save_settings(&model::Settings {
        align_mode: align_mode(),
        render_mode: render_mode(),
        auto_category: auto_category(),
        desktop_state: desktop_state(),
        z_guard: z_guard_setting(),
        show_chrome: on,
    });
}
/// 重建"已收纳(pinned)"路径表(自定义分类模式的数据源)
fn rebuild_pins() {
    let s = state().lock().unwrap();
    model::rebuild_pinned_registry(&s.fences);
}

/// 精确模式是否生效(用户开启)。动态壁纸检测已退役(2026-08-26):ink 常驻
/// 后背景实时透出、阴影背景无关,动态壁纸不再构成降级理由;两模式共用同一
/// 条 seeded 文字管线,区别仅剩启动守卫(精确模式等首帧种子就绪再呈现)。
pub fn precise_mode_on() -> bool {
    render_mode() == "precise"
}

const TIMER_GLOBAL: usize = 1;
/// 悬停延迟提交定时器（图标高亮与原生桌面一致需悬停 ~400ms 才出现）
const TIMER_HOVER: usize = 2;
const TIMER_ANIMATION: usize = 3;
/// 壁纸追赶定时器:精确模式快照缺失时以 200ms 节奏重捕获,
/// 就绪后一次性整帧重绘,避免栅栏先出 D2D 文字帧再切换成 ClearType+阴影
const TIMER_WALLPAPER_CATCHUP: usize = 5;
/// 壁纸跟随定时器:Themes 目录事件后 250ms 防抖再捕获比对,
/// 未变化则短重试(Explorer 分多步写缓存、DWM 切换略有延迟)
const TIMER_WALLPAPER_FOLLOW: usize = 6;
/// 桌面态快速自检定时器:三指手势的窗口扫动不发任何 WinEvent,
/// 恢复过渡的检测只能靠轮询(见 zcheck_fences_now 注释)
const TIMER_DESKTOP_WATCH: usize = 7;
const EM_SETSEL: u32 = 0x00B1;
const WM_SETFONT: u32 = 0x0030;
const RENAME_COMMIT_MSG: u32 = WM_USER + 1;
const RENAME_CANCEL_MSG: u32 = WM_USER + 2;

const MENU_ADD_FENCE: u32 = 0x5101;
const MENU_RENAME: u32 = 0x5102;
const MENU_TOGGLE_COLLAPSE: u32 = 0x5103;
const MENU_LOCK: u32 = 0x5104;
const MENU_DELETE_FENCE: u32 = 0x5105;
const MENU_REFRESH: u32 = 0x5106;
const MENU_HIDE_ALL: u32 = 0x5107;
const MENU_SHOW_ALL: u32 = 0x5108;
const MENU_QUIT: u32 = 0x5109;
const MENU_RESET_LAYOUT: u32 = 0x510A;
const MENU_TOGGLE_DESKTOP_ICONS: u32 = 0x510B;
const MENU_AUTO_ALIGN: u32 = 0x510C;
const MENU_UNDO: u32 = 0x510D;
const MENU_AUTOSTART: u32 = 0x510E;
const MENU_ALIGN_GRID: u32 = 0x5114;
const MENU_ALIGN_FREE: u32 = 0x5115;
const MENU_RESTORE_DESKTOP: u32 = 0x510F;
const MENU_SORT_FREQ: u32 = 0x5110;
const MENU_SORT_TIME: u32 = 0x5111;
const MENU_SORT_NAME: u32 = 0x5112;
const MENU_SORT_MANUAL: u32 = 0x5113;
const MENU_RENDER_TRANSPARENT: u32 = 0x5116;
const MENU_RENDER_PRECISE: u32 = 0x5117;
const MENU_AUTO_CATEGORY: u32 = 0x5118;
const MENU_HELP: u32 = 0x5119;
const MENU_TOGGLE_CHROME: u32 = 0x511A;

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
/// 全局 z 序事件触发的高速自检请求(WinEvent 回调合并投递)
const WM_DL3_ZCHECK: u32 = WM_APP + 7;
/// windows 0.52 crate 未导出,按 Win32 头文件补定义
const WM_MOUSELEAVE: u32 = 0x02A3;
static TRAY_HWND: OnceLock<HWND> = OnceLock::new();
/// 菜单前台宿主窗口(1x1 隐形):菜单前台化的目标,避免提升栅栏窗口 z 序
static MENU_HOST_HWND: OnceLock<HWND> = OnceLock::new();

/// 菜单 owner 用的前台宿主;尚未创建时回退到调用方窗口
pub fn menu_host_or(fallback: HWND) -> HWND {
    MENU_HOST_HWND.get().copied().unwrap_or(fallback)
}
static TASKBAR_CREATED_MSG: OnceLock<u32> = OnceLock::new();
static TICK_COUNT: AtomicU32 = AtomicU32::new(0);
/// 箭头悬停自动弹菜单的防重触发时间戳(毫秒)

fn taskbar_created_msg() -> u32 {
    *TASKBAR_CREATED_MSG.get_or_init(|| unsafe {
        let name = shell::wide("TaskbarCreated");
        RegisterWindowMessageW(PCWSTR::from_raw(name.as_ptr()))
    })
}

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    Resize { edges: [char; 2] },
    Icon(usize),
    Marquee,
    ScrollThumb { grab: f32 },
}

/// 拖拽插入方案(2026-09-02:行内槽位模型)。几何在 model.rs
/// (rows_from_rects/row_slot_of/row_insert_layout,有单测),此处只做适配。
#[derive(Clone)]
struct InsertPlan {
    /// 全体可见栅栏的新位置(逐 start_layout 可见成员,含被拖者)
    assign: Vec<(u32, (f32, f32))>,
    /// 被拖者落点
    land: (f32, f32),
    /// 指示线 (x, y, w, h)
    line: (f32, f32, f32, f32),
}

#[derive(Clone)]
struct Drag {
    fence_id: u32,
    mode: DragMode,
    start_x: f32,
    start_y: f32,
    /// 按下时的屏幕坐标（Move/Resize 的位移基准，与窗口位置无关）
    start_sx: f32,
    start_sy: f32,
    start_rect: Rect,
    start_layout: Vec<Fence>,
    dragged_out: bool,
    /// 图标按下时该项是否已被选中(第二次点击已选中项 = Explorer 的慢双击重命名)
    icon_was_selected: bool,
    icon_path: String,
    /// Move 拖拽最后一次有插入线的方案:松手瞬间滑出容差也必须能插进去
    /// (以最后一次方案为准,2026-09-02)
    last_insert: Option<InsertPlan>,
}

/// 拖拽实时预览状态:拖动中即时重排显示,松手才生效;取消/拖出释放则回滚
#[derive(Clone)]
struct GhostPreview {
    fence_id: u32,
    /// 按下时的完整显示顺序(回滚与重排的基准)
    original: Vec<String>,
    /// 按下时的排序模式(预览期间切"手动",回滚时恢复)
    original_sort_mode: String,
    /// 被拖路径集合；重排时按它们在 original 中的相对顺序组成块
    dragged_paths: Vec<String>,
    /// 当前预览目标槽位（删除拖动块后的列表中，范围 0..=剩余项数）
    target: usize,
}

#[derive(Clone)]
struct ArrivalAnimation {
    fence_id: u32,
    path: String,
    name: String,
    from: (f32, f32),
    to: (f32, f32),
    started_ms: u64,
    duration_ms: u64,
}

struct UiState {
    pub renderer: Option<Renderer>,
    pub fences: Vec<Fence>,
    pub files: Vec<FileItem>,
    pub icon_cache: HashMap<String, IconBuffer>,
    pub windows: HashMap<u32, HWND>,
    pub metrics: HashMap<u32, model::DpiMetrics>,
    pub surfaces: HashMap<u32, Surface>,
    /// Fence ids whose most recent UpdateLayeredWindow call succeeded.
    pub presented: HashSet<u32>,
    /// Fence ids successfully inserted behind a currently valid desktop host.
    pub attached: HashSet<u32>,
    pub hover: HashMap<u32, Option<usize>>,
    /// 悬停延迟生效中的待提交悬停(原生桌面高亮有 ~400ms 悬停延迟)
    pub hover_pending: HashMap<u32, Option<usize>>,
    pub hover_hit: HashMap<u32, Hit>,
    /// Explorer 风格文件选择，以规范路径为稳定身份；跨栅栏多选也不会因排序变化丢失。
    pub selected_paths: HashSet<String>,
    pub focused_path: Option<String>,
    pub selection_anchor: Option<String>,
    pub marquee: Option<(f32, f32, f32, f32)>,
    pub active_fence: Option<u32>,
    /// 鼠标是否悬停在某个栅栏窗口上(决定是否浮现卡片/标题/滚动条)
    pub fence_hover: HashMap<u32, bool>,
    /// 栅栏卡片悬停的延迟提交标记(与图标 hover 同款 400ms 延迟)
    pub fence_hover_pending: HashMap<u32, bool>,
    pub drag: Option<Drag>,
    pub rename_fence: Option<u32>,
    pub rename_edit: Option<HWND>,
    /// 文件就地重命名的 EDIT 窗口（图标名标签上的编辑框）
    pub file_rename_edit: Option<HWND>,
    /// 拖动节流：上次真正重排时的鼠标位置（用于抑制高频 WM_MOUSEMOVE 抖动）
    pub drag_settle_x: f32,
    pub drag_settle_y: f32,
    /// resize 时间节流:上次表面重建时刻(毫秒),限制重建频率保证 1:1 跟手不卡顿
    pub last_resize_ms: u64,
    /// 栅栏移动时间节流:上次移动呈现时刻(毫秒),逐像素跟随但限频
    pub last_move_ms: u64,
    /// 拖动中内容重渲染(壁纸种子重烘焙)节拍:上次全量 refresh_fence 时刻。
    /// 位置跟随已由"已有像素重呈现"逐帧完成,内容重烘焙降到 ~30fps。
    pub last_drag_render_ms: u64,
    /// 拖动对齐参考线（overlay 绘制）：guide_x = 竖线坐标，guide_y = 横线坐标
    pub guide_x: Option<f32>,
    pub guide_y: Option<f32>,
    /// 内部图标拖拽残影:被拖图标(半透明)跟随鼠标的屏幕坐标绘制在 overlay 上
    pub drag_ghost: Option<(Vec<String>, f32, f32)>,
    /// 拖拽实时预览(松手生效,取消回滚)
    pub ghost_preview: Option<GhostPreview>,
    /// 插入指示线(屏幕坐标 x,y,w,h;w<=h 为竖线=水平邻居间,否则横线)。
    /// 拖动中被拖对象自由跟手、其余完全不动,只有此线提示松手后的插入位置。
    pub insert_line: Option<(f32, f32, f32, f32)>,
    /// 图标拖拽悬停在回收站图标上(松手=删除到回收站,不重排)
    pub trash_target: bool,
    pub guide_hwnd: Option<HWND>,
    pub guide_surface: Option<Surface>,
    pub arrival_animations: Vec<ArrivalAnimation>,
    /// 精确模式:各桌面宿主的壁纸快照(屏幕坐标)与上次捕获时刻(节流用)
    pub wallpapers: Vec<render::WallpaperPixels>,
    pub wallpaper_ms: u64,
    /// 壁纸捕获连续失败次数(≥2 触发精确模式自动回退透明)
    pub wallpaper_fails: u32,
    /// 壁纸已失效待重捕获的时刻(0=无待办)。ink 常驻后快照只作文字种子,
    /// 重捕获走"懒化"路径:淡入结束+足够安静才执行,不与用户交互赛跑
    pub wallpaper_dirty_since: u64,
    /// z 链失位防抖计数:连续两拍失位才修。菜单开合瞬间系统瞬态窗(EdgeUi
    /// 输入条/第三方软件全屏钩子窗/cloaked CoreWindow)会短暂插进宿主与栅栏之间
    /// 又立刻退出;单拍误判即整链 SetWindowPos=DWM 重合成闪屏(2026-08-27 用户
    /// 实感)。真浮出带会连续多拍命中,自愈延迟仅 ~1-2s。
    pub walk_strikes: HashMap<u32, WalkStrike>,
    /// strike 最近一次推进的墙钟时刻:global_tick 在 needs_represent 路径会
    /// 同秒二次调用 ensure_all_attached,不限速则一秒推两拍,"3 拍≈3 秒"
    /// 的防抖语义失真(2026-08-28 实测 Win+D 沉底 1.4s 即修,与设计意图不符)。
    pub walk_strike_ms: HashMap<u32, u64>,
    /// 走查最新结论:所有栅栏健康且带内无可见外来窗(=桌面态)。
    /// 桌面态下 TIMER_DESKTOP_WATCH 以 250ms 节奏跑高速自检——三指手势
    /// 恢复不发任何 WinEvent,只有轮询能及时兜住(2026-08-28 实测)。
    pub band_quiet: bool,
    /// 批量呈现抑制位(show_all_fences 置位):true 期间 refresh_fence_impl
    /// 跳过 ShowWindow/SHOWWINDOW——先把全部栅栏表面画完并向隐藏窗提交
    /// ULW,循环结束一次批量放行。否则"画完一个亮一个",首末栅栏相差
    /// 整个串行绘制时长,启动时有明显扫过感。
    pub defer_show_until_batch: bool,
    /// 每个非隐藏栅栏最近一次"全部就绪"(z 在带+已呈现+窗口可见)的时刻。
    /// attached 集合每 tick 全清重建,防抖期内失位栅栏会短暂缺席;z-chain
    /// 防抖窗口(≤3 tick≈3s)不能让 reconcile 的保底恢复误判"没有任何
    /// 就绪栅栏"而把原生桌面放出来(2026-08-27 实测 1-2s 原生闪现),
    /// 因此就绪判定对 8s 内健康的栅栏放行。
    pub last_healthy_ms: HashMap<u32, u64>,
}

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

struct WalkStrike {
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

fn state() -> &'static Mutex<UiState> {
    static S: OnceLock<Mutex<UiState>> = OnceLock::new();
    S.get_or_init(|| {        Mutex::new(UiState {
            renderer: None,
            fences: Vec::new(),
            files: Vec::new(),
            icon_cache: HashMap::new(),
            windows: HashMap::new(),
            metrics: HashMap::new(),
            surfaces: HashMap::new(),
            presented: HashSet::new(),
            attached: HashSet::new(),
            walk_strikes: HashMap::new(),
            walk_strike_ms: HashMap::new(),
            band_quiet: false,
            defer_show_until_batch: false,
            last_healthy_ms: HashMap::new(),
            hover: HashMap::new(),
            hover_pending: HashMap::new(),
            hover_hit: HashMap::new(),
            selected_paths: HashSet::new(),
            focused_path: None,
            selection_anchor: None,
            marquee: None,
            active_fence: None,
            fence_hover: HashMap::new(),
            fence_hover_pending: HashMap::new(),
            drag: None,
            rename_fence: None,
            rename_edit: None,
            file_rename_edit: None,
            drag_settle_x: 0.0,
            drag_settle_y: 0.0,
            last_resize_ms: 0,
            last_move_ms: 0,
            last_drag_render_ms: 0,
            guide_x: None,
            guide_y: None,
            drag_ghost: None,
            ghost_preview: None,
            insert_line: None,
            trash_target: false,
            guide_hwnd: None,
            guide_surface: None,
            arrival_animations: Vec::new(),
            wallpapers: Vec::new(),
            wallpaper_ms: 0,
            wallpaper_fails: 0,
            wallpaper_dirty_since: 0,
        })
    })
}

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
    s.guide_x = None;
    s.guide_y = None;
    s.arrival_animations.clear();
}

fn clear_fence_interaction(s: &mut UiState, fence_id: u32) {
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
        s.guide_x = None;
        s.guide_y = None;
    }
    // Selection is global because pinned items can appear in multiple fences.
    // A lifecycle change invalidates any visual ownership, so clear it wholesale.
    s.selected_paths.clear();
    s.focused_path = None;
    s.selection_anchor = None;
}

fn finish_interaction_cleanup() {
    unsafe {
        let _ = ReleaseCapture();
    }
    let s = state().lock().unwrap();
    if s.arrival_animations.is_empty() {
        if let Some(tray) = TRAY_HWND.get().copied() {
            unsafe {
                let _ = KillTimer(tray, TIMER_ANIMATION);
            }
        }
    }
    if s.guide_x.is_none()
        && s.guide_y.is_none()
        && s.drag_ghost.is_none()
        && s.arrival_animations.is_empty()
    {
        if let Some(hwnd) = s.guide_hwnd {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

pub fn log(line: &str) {
    let dir = model::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("run.log");
    // 轮转:超 4MB 归档为 run.log.old(覆盖旧档),防长期运行无限增长。
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.len() > 4 * 1024 * 1024 {
            let old = dir.join("run.log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&p, &old);
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        // 本地日期+时间:run.log 跨多次启动追加,只有时分秒无法区分天,
        // 排查偶发问题时对不上用户操作的时刻(2026-08-28 排查实证)。
        let st = unsafe { GetLocalTime() };
        let _ = writeln!(
            f,
            "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}] {}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond,
            line
        );
    }
}

/// 系统 DPI 缩放系数（进程已 SetProcessDPIAware，坐标系为物理像素）
fn dpi_scale() -> f32 {
    unsafe { GetDpiForSystem() as f32 / 96.0 }.max(1.0)
}

/// 与桌面图标一致的物理像素尺寸：优先实测桌面列表视图的图标格距
/// （LVM_GETITEMSPACING 返回值即格宽/格高，跨进程可用、不受注册表值过期影响）。
/// 注意:水平方向格宽可直接拆分；垂直格高包含标题带，不能把它
/// 当作纯图标留白再次相加。失败时回退到当前注册表值。
fn current_icon_size() -> f32 {
    if let Some((cell_w_px, cell_h_px)) = probe_desktop_item_spacing() {
        // 图标本体:注册表 IconSize(Explorer 在 Ctrl+滚轮时会写入;缺失=默认32)×DPI。
        // 用实测格距反推留白,保证格宽格高与原生逐像素一致
        // (垂直格高含文字区,不能用注册表 IconVerticalSpacing 直接算)
        let icon = shell::desktop_icon_size() * dpi_scale();
        if (16.0..=256.0).contains(&icon) && cell_w_px > icon && cell_h_px > icon {
            let pad_x = ((cell_w_px - icon) / dpi_scale()).clamp(16.0, 96.0);
            // Explorer's vertical spacing includes the caption band. Keep the
            // measured cell height instead of adding the icon size twice.
            let pad_y = (cell_h_px / dpi_scale() - icon / dpi_scale()).clamp(16.0, 96.0);
            model::set_cell_pads(pad_x, pad_y);
            return icon.round();
        }
        log(&format!(
            "listview spacing {cell_w_px}x{cell_h_px} vs icon {icon} implausible; fallback"
        ));
    }
    (shell::desktop_icon_size() * dpi_scale()).round()
}

/// The desktop icon preference is stored in logical pixels. WM_DPICHANGED must
/// use the target window's DPI rather than the process/system DPI so freshly
/// requested Shell icons and the model grid agree after a monitor crossing.
fn icon_size_for_dpi(scale: f32) -> f32 {
    (shell::desktop_icon_size() * scale).round()
}

/// 实测桌面 SysListView32 的图标格距（物理像素）。
/// LVM_GETITEMSPACING = LVM_FIRST(0x1000) + 51，返回值 LOWORD=cx HIWORD=cy。
fn metrics_for_window(hwnd: HWND) -> model::DpiMetrics {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    // 用初始化时实测的桌面格距（LVM_GETITEMSPACING 反推）保证栅栏格距与原生
    // 桌面逐像素一致；注册表 IconSpacing 多数机器上不反映真实格高（垂直格高
    // 含标题带），直接用会让栅栏垂直间距偏离原生桌面。
    let (pad_x, pad_y) = model::cell_pads();
    model::DpiMetrics::new(dpi, icon_size_for_dpi(dpi as f32 / 96.0), pad_x, pad_y)
}

/// 探测结果缓存:probe 要向桌面 SysListView32 跨进程发 LVM_GETITEMSPACING,
/// 会唤醒 Explorer 宿主窗口工作——菜单等前台切换后宿主的这次重绘表现为
/// 栅栏区域整面 ~4% 亮度跳变(用户看到的"点桌面关菜单闪一下")。
/// 2026-08-26 改为**粘性缓存**:键=(注册表 IconSize, 系统 DPI 缩放)——
/// 格距只在这些输入变化时才会变(Ctrl+滚轮写注册表,换显示器/DPI 改缩放),
/// 键不变就永不重发探测,把对宿主的骚扰从每 10s 一次降到"配置变化时一次"。
/// sync_icon_size 的跟随能力不受影响:用户 Ctrl+滚轮 → 注册表变化 → 键失配
/// → 恰好探测一次并重算格距。
static ITEM_SPACING_CACHE: Mutex<Option<((f32, u32), (f32, f32))>> = Mutex::new(None);

fn probe_desktop_item_spacing() -> Option<(f32, f32)> {
    let key = (shell::desktop_icon_size(), unsafe {
        (GetDpiForSystem() as u32).max(96)
    });
    {
        let cache = ITEM_SPACING_CACHE.lock().unwrap();
        if let Some((k, v)) = *cache {
            if k == key {
                return Some(v);
            }
        }
    }
    let lv = desktop_listview()?;
    unsafe {
        let mut res = LRESULT(0);
        let ok = SendMessageTimeoutW(
            lv,
            0x1033,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            200,
            Some((&mut res.0 as *mut isize).cast()),
        );
        if ok.0 == 0 || res.0 == 0 {
            return None;
        }
        let cx = (res.0 & 0xFFFF) as f32;
        let cy = ((res.0 >> 16) & 0xFFFF) as f32;
        if cx < 40.0 || cy < 40.0 || cx > 512.0 || cy > 512.0 {
            return None;
        }
        let v = (cx, cy);
        *ITEM_SPACING_CACHE.lock().unwrap() = Some((key, v));
        Some(v)
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
    unsafe {
        // Per-monitor v2 keeps physical pixels crisp when a fence moves between monitors.
        // Fall back for older Windows builds without changing any desktop setting.
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
            let _ = SetProcessDPIAware();
        }
        let _ = CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED
                | windows::Win32::System::Com::COINIT_DISABLE_OLE1DDE,
        );
    }
    // 必须在设置 DPI awareness 之后读 DPI，否则系统会按未感知返回 96。
    // 顺序：先设 DPI 与注册表留白，再实测图标尺寸（实测成功会同步覆盖留白为精确值）
    model::set_dpi_scale(dpi_scale());
    let (pad_x, pad_y) = shell::desktop_cell_pads();
    model::set_cell_pads(pad_x, pad_y);
    model::set_icon_size(current_icon_size());
    // 系统右键菜单里的"重命名"改由栅栏内就地编辑完成
    shell::set_rename_request_hook(on_shell_rename_request);
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
    log(&format!("boot init done ({}ms)", resize_now_ms() - t0));
    true
}

fn register_class() {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(fence_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance(),
            hIcon: deskfence_icon(),
            hCursor: HCURSOR(0),
            hbrBackground: HBRUSH(0),
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
            hCursor: HCURSOR(0),
            hbrBackground: HBRUSH(0),
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
            hCursor: HCURSOR(0),
            hbrBackground: HBRUSH(0),
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
            hCursor: HCURSOR(0),
            hbrBackground: HBRUSH(0),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: menu_host_class_name(),
        };
        let _ = RegisterClassW(&wc4);
    }
}

// ---------------- 桌面宿主(WorkerW 收养) ----------------

/// 可收养栅栏窗口的桌面宿主:带图标的 WorkerW/Progman(主屏)或
/// 通过 0x052C 消息生成的每显示器 WorkerW(副屏)。坐标为屏幕坐标。
#[derive(Clone, Copy)]
struct HostInfo {
    hwnd: HWND,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    primary: bool,
    /// 宿主是否可见:Explorer 重启重建期间 WorkerW 可能短暂隐藏,
    /// 收养到隐藏宿主会导致栅栏不可见,必须过滤。
    visible: bool,
}

static HOSTS_CACHE: OnceLock<Mutex<(std::time::Instant, Vec<HostInfo>)>> = OnceLock::new();

fn invalidate_hosts_cache() {
    if let Some(cache) = HOSTS_CACHE.get() {
        let mut c = cache.lock().unwrap();
        c.0 = std::time::Instant::now() - std::time::Duration::from_secs(10);
    }
}

/// 桌面宿主列表(带 500ms 缓存,拖动高频调用不重复枚举窗口)
fn desktop_hosts() -> Vec<HostInfo> {
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

unsafe extern "system" fn enum_workerw_host(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut (Vec<HostInfo>, HWND));
    let mut buf = [0u16; 256];
    if GetClassNameW(hwnd, &mut buf) > 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if String::from_utf16_lossy(&buf[..end]) == "WorkerW" {
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
    BOOL(1)
}

fn refresh_hosts() -> Vec<HostInfo> {
    let primary = desktop_shell_window().unwrap_or(HWND(0));
    let mut hosts: Vec<HostInfo> = Vec::new();
    unsafe {
        let mut ctx = (hosts, primary);
        let _ = EnumWindows(
            Some(enum_workerw_host),
            LPARAM(&mut ctx as *mut (Vec<HostInfo>, HWND) as isize),
        );
        hosts = ctx.0;
    }
    // 老路径主桌面是 Progman(非 WorkerW):补一个 primary 宿主
    if primary.0 != 0 && !hosts.iter().any(|h| h.hwnd == primary) {
        let mut r: RECT = unsafe { std::mem::zeroed() };
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
    hosts.sort_by(|a, b| b.primary.cmp(&a.primary));
    hosts
}

/// 为栅栏选择宿主:中心点落在哪个【可见】宿主就收养到哪;都不覆盖时返回 None
/// (窗口暂缓创建,由全局定时器在宿主就绪后补挂,绝不复用 HWND_TOP 回退)。
fn host_for_rect(rect: &Rect, hosts: &[HostInfo]) -> Option<HostInfo> {
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
fn window_is_cloaked(w: HWND) -> bool {
    let mut cloaked: u32 = 0;
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
fn drag_elevate_anchor(host: HWND, dragged: HWND) -> Option<HWND> {
    let mut anchor: Option<HWND> = None;
    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
    for _ in 0..64 {
        if w.0 == 0 {
            break;
        }
        if w == dragged {
            w = unsafe { GetWindow(w, GW_HWNDPREV) };
            continue;
        }
        // 只沿"连续的兄弟栅栏段"向上找;段结束(遇到非栅栏窗)即停
        let mut cls_buf = [0u16; 32];
        let n = unsafe { GetClassNameW(w, &mut cls_buf) };
        let is_fence = n == 14
            && cls_buf[..14]
                == [
                    0x44, 0x65, 0x73, 0x6B, 0x46, 0x65, 0x6E, 0x63, 0x65, 0x46, 0x65,
                    0x6E, 0x63, 0x65,
                ];
        if !is_fence {
            break;
        }
        anchor = Some(w);
        w = unsafe { GetWindow(w, GW_HWNDPREV) };
    }
    anchor
}

/// SetWindowPos places a window *behind* hWndInsertAfter. Passing WorkerW directly
/// therefore puts the fence below the desktop host and can produce a fully blank
/// desktop after Show Desktop changes WorkerW ordering. Use the window immediately
/// above the host so the fence sits between desktop and normal application windows.
/// 宿主之上没有任何窗口时返回 None(不移动):绝不能回退 HWND_TOP——那会把
/// 栅栏顶到整个 z 栈顶端(2026-08-27 实测三个栅栏被顶到宿主之上 215 层,
/// 即用户看到的"栅栏浮在别的窗口上方")。
fn desktop_insert_after(host: HWND) -> Option<HWND> {
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
fn is_own_fence_window(w: HWND) -> bool {
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
fn band_attach_anchor(host: HWND, skip: HWND, deep: bool) -> Option<HWND> {
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
    // 深位回退(deep=true):主规则无"可见且非 topmost"外来窗时,锚到"最低
    // 可见或 topmost 外来窗"之下、紧贴它的最高**非 topmost 隐形**外来窗。
    // 锚必须自身非 topmost:插到 topmost 窗正下方会把栅栏并入 topmost band
    // (2026-08-29 实测 5 栅栏全变 topmost=True;且 SetWindowLongW 清不掉
    // 该位,HWND_NOTOPMOST 又会把窗口移到非 topmost 带顶部=位置不可控,
    // 此路不通,勿再试)。无可垫垃圾则继续兄弟归队/带底。
    if deep {
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
            break; // 首个可见外来窗(含 topmost)到顶
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

/// 当前鼠标屏幕坐标（拖动位移必须用屏幕坐标，
/// 因为窗口移动后 WM_MOUSEMOVE 的客户区坐标会随之变化，造成抖动/拖不动）。
fn screen_cursor() -> (f32, f32) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        (pt.x as f32, pt.y as f32)
    }
}

/// 刷新所有栅栏窗口（位置/尺寸/内容），用于自动对齐重排后
fn refresh_all_fences() {
    let ids: Vec<u32> = state()
        .lock()
        .unwrap()
        .fences
        .iter()
        .map(|f| f.id)
        .collect();
    for id in ids {
        refresh_fence(id);
    }
}

fn create_fence_window(s: &mut UiState, fence_id: u32, hosts: &[HostInfo]) -> bool {
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
        return false;
    };
    if s.windows.contains_key(&fence_id) {
        return true;
    }
    let hinstance = hinstance();
    let w = fence.rect.w.round() as i32;
    let h = fence.rect.h.round() as i32;
    // 顶层分层窗口 + z 序插到桌面宿主(WorkerW/Progman)之后 ——
    // 位于壁纸/桌面图标层之上、所有普通窗口之下,常驻桌面且不浮窗。
    // 注意:绝不能做成桌面子窗口(分层子窗口挂在 Progman 下不会绘制),
    // 也绝不能回退 HWND_TOP(会浮到应用之上);找不到宿主就延迟创建,
    // 由全局定时器每秒重试自愈。
    let host = host_for_rect(&fence.rect, hosts);
    if host.is_none() {
        log(&format!("no desktop host yet, defer fence {}", fence_id));
        return false;
    }
    let _zcreate = z_scope(ZIntent::Create);
    let hwnd = unsafe {
        CreateWindowExW(
            // 栅栏始终不参与前台激活；这样点击菜单外的桌面空白只会关闭
            // TrackPopupMenu，不会在 Explorer 与分层栅栏之间切换激活层导致闪屏。
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name(),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            w,
            h,
            HWND(0),
            HMENU(0),
            hinstance,
            None,
        )
    };
    if hwnd.0 == 0 {
        log(&format!("create window failed id={}", fence_id));
        return false;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, fence_id as isize);
        ole::register_drop_target(hwnd, fence_id);
        // 就位目标:最低可见外来窗正下方(带内绝缘位,见 band_attach_anchor;
        // 勿回退到"宿主正上方"——带底是菜单开合的扰动区,2026-08-29 闪屏
        // 根因)。取不到锚点时不动 z——初始位置由全局 tick 的自愈在宿主
        // 就绪后校正;HWND_TOP 回退曾把栅栏顶到栈顶。
        let insert_after = match host.map(|h| band_attach_anchor(h.hwnd, HWND(0), false)).flatten() {
            Some(a) => Some(a),
            None => desktop_shell_window().and_then(|s| band_attach_anchor(s, HWND(0), false)),
        };
        let mut attached = false;
        if let Some(after) = insert_after {
            attached = SetWindowPos(
                hwnd,
                after,
                fence.rect.x.round() as i32,
                fence.rect.y.round() as i32,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .is_ok();
        }
        if attached {
            s.attached.insert(fence_id);
        }
    }
    s.windows.insert(fence_id, hwnd);
    s.metrics.insert(fence_id, metrics_for_window(hwnd));
    true
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
                let all_dark = px
                    .chunks_exact(4)
                    .take(4096)
                    .all(|c| c[0] == 0 && c[1] == 0 && c[2] == 0 && c[3] == 255);
                if all_dark && w > 64 && ph > 64 {
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
                    host.hwnd.0, host.w as i32, host.h as i32, reason
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
            if n <= 2 || n % 25 == 0 {
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
static EMPTY_CAPTURE_LOG_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// 最近一次用户交互(菜单开/关、桌面点击)的时刻。壁纸捕获在交互后
/// 2.5s 内主动推迟:宿主在前台切换后的未稳定态下被 PrintWindow 强制
/// 重绘会闪 ±4% 亮度,稳态则无感。
pub static LAST_INTERACTION_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub fn mark_interaction() {
    LAST_INTERACTION_MS.store(resize_now_ms(), Ordering::Relaxed);
}

/// 比较新旧快照内容。**带每通道 8 的容差**:PrintWindow 捕获的壁纸亮度
/// 存在 ~4% 的时序波动(ICC/伽马路径),逐字节严格比较会把波动当成
/// "壁纸变了",触发无谓的全量重绘——栅栏区域整面 4% 亮度先跳再回,
/// 正是用户看到的"闪"。真换壁纸是整图替换,容差不影响判别。
/// 比较新旧快照在"栅栏覆盖区域"内是否有实质变化(每通道 8 容差,理由:
/// PrintWindow 捕获亮度存在 ~4% 时序波动,严格比较会把波动当成变化)。
/// 栅栏区域之外的变化(如动态时钟壁纸的分钟跳动)不影响渲染——ink 常驻
/// 下快照只作标签种子,栅栏外的壁纸像素从不参与任何绘制——因此不触发
/// 重绘与缓存落盘,避免时钟壁纸下的每分钟空转(全量重绘+9MB 落盘+闪风险)。
/// 宿主几何(数量/尺寸/原点)变化仍视为整体变化;无栅栏时退化为全图比较。
fn wallpaper_changed_under_fences(
    old: &[render::WallpaperPixels],
    new: &[render::WallpaperPixels],
    fences: &[Fence],
) -> bool {
    if old.len() != new.len() {
        return true;
    }
    for (a, b) in old.iter().zip(new.iter()) {
        if a.w != b.w || a.h != b.h || a.origin_x != b.origin_x || a.origin_y != b.origin_y {
            return true;
        }
    }
    if fences.is_empty() {
        return old
            .iter()
            .zip(new.iter())
            .any(|(a, b)| px_differs(&a.px, &b.px));
    }
    for f in fences {
        let fl = f.rect.x.round() as i32;
        let ft = f.rect.y.round() as i32;
        let fr = fl + f.rect.w.round() as i32;
        let fb = ft + f.rect.h.round() as i32;
        for (a, b) in old.iter().zip(new.iter()) {
            let x0 = (fl - a.origin_x).max(0);
            let y0 = (ft - a.origin_y).max(0);
            let x1 = (fr - a.origin_x).min(a.w as i32);
            let y1 = (fb - a.origin_y).min(a.h as i32);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            for y in y0..y1 {
                let row_a = ((y as u32 * a.w + x0 as u32) as usize) * 4;
                let row_b = ((y as u32 * b.w + x0 as u32) as usize) * 4;
                let len = (x1 - x0) as usize * 4;
                if px_differs(&a.px[row_a..row_a + len], &b.px[row_b..row_b + len]) {
                    return true;
                }
            }
        }
    }
    false
}

fn px_differs(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return true;
    }
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        if pa[0].abs_diff(pb[0]) > 8
            || pa[1].abs_diff(pb[1]) > 8
            || pa[2].abs_diff(pb[2]) > 8
        {
            return true;
        }
    }
    false
}

/// 壁纸快照持久化缓存(二进制):启动直接加载,免去"原生图标还可见时的
/// 现场捕获"——捕获要么拍到图标残影,要么得闪烁隐藏图标;缓存让首帧
/// 立即可用且干净,新鲜捕获在栅栏接管桌面(图标已隐藏)后由例行重捕获完成。
/// 格式: "DFWP" u32 ver | u32 count | 每项 { i32 x, i32 y, u32 w, u32 h, u64 len, BGRA }
fn wallpaper_cache_path() -> std::path::PathBuf {
    model::config_dir().join("wallpaper.bin")
}

fn icon_cache_path() -> std::path::PathBuf {
    model::config_dir().join("iconcache.bin")
}

/// 图标像素字节必须是 size×size×4(DIB 32bpp,见 render::icon_pixels),
/// 加载时逐条校验,不符即丢弃该条(防御旧版/损坏文件)。
const ICON_ENTRY_MAX_BYTES: usize = 4 * 256 * 256;

/// 持久化图标/显示名缓存——冷启动加速核心。此前每次启动都对全部桌面
/// 条目跑 SHGFI 显示名解析 + 图标提取(.lnk/exe 冷盘+杀软扫描单个可达
/// 数百 ms),这是"开机后栅栏比原生桌面晚好几秒"的主要可控来源。
///
/// 键与内存缓存一致:{path}\0{px};校验:mtime 与 raw 扫描一致 + 长度
/// ==4*px*px。返回 (图标命中表, 显示名命中表)。
fn load_icon_cache_file(
    raw: &[model::FileItem],
) -> (std::collections::HashMap<String, Vec<u8>>, std::collections::HashMap<String, String>) {
    use std::io::Read;
    let mut f = match std::fs::File::open(icon_cache_path()) {
        Ok(f) => f,
        Err(_) => return Default::default(),
    };
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() || buf.len() < 12 || &buf[0..4] != b"DFIC" {
        return Default::default();
    }
    let ver = u32::from_le_bytes(buf[4..8].try_into().unwrap_or([0; 4]));
    if ver != 1 {
        return Default::default();
    }
    let px = u32::from_le_bytes(buf[8..12].try_into().unwrap_or([0; 4]));
    // px 由调用方条目键的后缀再核一次;这里只挡住荒谬值
    if px < 16 || px > 256 {
        return Default::default();
    }
    let expected_len = (px as usize) * (px as usize) * 4;
    let count = u32::from_le_bytes(
        buf.get(12..16)
            .map(|s| s.try_into().unwrap_or([0; 4]))
            .unwrap_or([0; 4]),
    ) as usize;
    let mut off = 16usize;
    let mut icons: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();
    let expect_mtime: std::collections::HashMap<&str, u64> =
        raw.iter().map(|f| (f.path.as_str(), f.mtime_ms)).collect();
    for _ in 0..count.min(8192) {
        if off + 2 > buf.len() {
            break;
        }
        let klen = u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
        off += 2;
        if klen == 0 || klen > 1024 || off + klen + 12 > buf.len() {
            break;
        }
        let key = String::from_utf8_lossy(&buf[off..off + klen]).to_string();
        off += klen;
        let mtime = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]));
        off += 8;
        let blen = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4])) as usize;
        off += 4;
        if blen > ICON_ENTRY_MAX_BYTES || off + blen > buf.len() {
            break;
        }
        // 键的路径部分必须存在于本次扫描且 mtime 一致(px 后缀也须匹配当前
        // DPI);单条不合规只跳过该条,不再中断整表。
        let path_part = key.split('\0').next().unwrap_or("");
        if blen == expected_len
            && key.ends_with(&format!("\0{px}"))
            && expect_mtime.get(path_part).copied() == Some(mtime)
            && mtime != 0
        {
            icons.insert(key, buf[off..off + blen].to_vec());
        }
        off += blen;
    }
    // 第二段:显示名表(path→display)。段头 magic 缺失不算错误(纯图标版兼容)。
    let mut names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if off + 4 <= buf.len() && &buf[off..off + 4] == b"DFNM" {
        off += 4;
        if off + 4 <= buf.len() {
            let ncnt = u32::from_le_bytes(
                buf[off..off + 4].try_into().unwrap_or([0; 4]),
            ) as usize;
            off += 4;
            for _ in 0..ncnt.min(8192) {
                if off + 2 > buf.len() {
                    break;
                }
                let plen = u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
                off += 2;
                if plen == 0 || plen > 1024 || off + plen > buf.len() {
                    break;
                }
                let p = String::from_utf8_lossy(&buf[off..off + plen]).to_string();
                off += plen;
                if off + 2 > buf.len() {
                    break;
                }
                let dlen = u16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2])) as usize;
                off += 2;
                if dlen > 512 || off + dlen > buf.len() {
                    break;
                }
                let d = String::from_utf8_lossy(&buf[off..off + dlen]).to_string();
                off += dlen;
                if !p.is_empty() && !d.is_empty() {
                    names.insert(p, d);
                }
            }
        }
    }
    log(&format!(
        "boot icon cache loaded: icons={} names={}",
        icons.len(),
        names.len()
    ));
    (icons, names)
}

/// 把当前 icon_cache 与 files 的显示名快照落盘(tmp+rename 原子替换)。
/// 只收 px==当前系统图标像素 的条目(文件头单值 px,保证与加载端逐条
/// 长度校验一致);字节流恒为 px×px×4(render::icon_pixels 契约)。
/// ~56 项 ≈ 0.5MB,后台线程序列化无感知。由全局 tick 检测到提取计数
/// 变化后延迟调用——运行期懒提取(DPI 切换/新文件/残影预览)自动覆盖。
fn save_icon_cache_file_now(px_expected: u32) {
    // 1) 短暂持锁克隆快照
    let mut entries: Vec<(String, u64, std::sync::Arc<Vec<u8>>)> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    {
        let s = state().lock().unwrap();
        let by_path: HashMap<&str, &model::FileItem> =
            s.files.iter().map(|f| (f.path.as_str(), f)).collect();
        for (key, buf) in s.icon_cache.iter() {
            let Some((p, pxs)) = key.split_once('\0') else {
                continue;
            };
            if pxs.parse::<u32>().ok() != Some(px_expected) {
                continue;
            }
            let blen = (px_expected as usize) * (px_expected as usize) * 4;
            if buf.len() != blen || blen > ICON_ENTRY_MAX_BYTES {
                continue;
            }
            let Some(fi) = by_path.get(p) else { continue };
            if fi.mtime_ms == 0 {
                continue;
            }
            entries.push((
                key.clone(),
                fi.mtime_ms,
                std::sync::Arc::new(buf.clone()),
            ));
        }
        for f in s.files.iter() {
            names.push((f.path.clone(), f.name.clone()));
        }
    }
    if entries.is_empty() {
        return;
    }
    if entries.len() > 512 {
        entries.sort_by_key(|(_, mt, _)| *mt);
        entries.drain(..entries.len() - 512);
    }
    // 2) 后台序列化+写盘
    std::thread::spawn(move || {
        use std::io::Write;
        let total: usize = entries.iter().map(|e| e.2.len()).sum();
        let mut buf: Vec<u8> = Vec::with_capacity(total + 4096);
        buf.extend_from_slice(b"DFIC");
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&px_expected.to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (key, mtime, bytes) in &entries {
            buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
            buf.extend_from_slice(key.as_bytes());
            buf.extend_from_slice(&mtime.to_le_bytes());
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        buf.extend_from_slice(b"DFNM");
        buf.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for (p, d) in &names {
            buf.extend_from_slice(&(p.len() as u16).to_le_bytes());
            buf.extend_from_slice(p.as_bytes());
            buf.extend_from_slice(&(d.len() as u16).to_le_bytes());
            buf.extend_from_slice(d.as_bytes());
        }
        let path = icon_cache_path();
        let tmp = path.with_extension("bin.tmp");
        let ok = std::fs::File::create(&tmp)
            .and_then(|mut f| {
                f.write_all(&buf)?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, &path))
            .is_ok();
        if !ok {
            log("icon cache save failed");
        }
    });
}


fn save_wallpaper_cache(caps: &[render::WallpaperPixels]) {
    use std::io::Write;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    buf.extend_from_slice(b"DFWP");
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&(caps.len() as u32).to_le_bytes());
    for c in caps {
        buf.extend_from_slice(&c.origin_x.to_le_bytes());
        buf.extend_from_slice(&c.origin_y.to_le_bytes());
        buf.extend_from_slice(&c.w.to_le_bytes());
        buf.extend_from_slice(&c.h.to_le_bytes());
        buf.extend_from_slice(&(c.px.len() as u64).to_le_bytes());
        buf.extend_from_slice(&c.px);
    }
    let path = wallpaper_cache_path();
    let tmp = path.with_extension("bin.tmp");
    let ok = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(&buf)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, &path))
        .is_ok();
    if !ok {
        log("wallpaper cache save failed");
    }
}

fn load_wallpaper_cache() -> Option<Vec<render::WallpaperPixels>> {
    use std::io::Read;
    let mut f = std::fs::File::open(wallpaper_cache_path()).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    if buf.len() < 12 || &buf[0..4] != b"DFWP" {
        return None;
    }
    let ver = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    if ver != 1 {
        return None;
    }
    let count = u32::from_le_bytes(buf[8..12].try_into().ok()?) as usize;
    let mut caps = Vec::with_capacity(count.min(8));
    let mut off = 12usize;
    for _ in 0..count {
        if off + 24 > buf.len() {
            return None;
        }
        let origin_x = i32::from_le_bytes(buf[off..off + 4].try_into().ok()?);
        let origin_y = i32::from_le_bytes(buf[off + 4..off + 8].try_into().ok()?);
        let w = u32::from_le_bytes(buf[off + 8..off + 12].try_into().ok()?);
        let h = u32::from_le_bytes(buf[off + 12..off + 16].try_into().ok()?);
        let len = u64::from_le_bytes(buf[off + 16..off + 24].try_into().ok()?) as usize;
        off += 24;
        if w == 0 || h == 0 || w > 16384 || h > 16384 || len != (w as usize) * (h as usize) * 4 {
            return None;
        }
        if off + len > buf.len() {
            return None;
        }
        caps.push(render::WallpaperPixels {
            px: buf[off..off + len].to_vec(),
            w,
            h,
            origin_x,
            origin_y,
        });
        off += len;
    }
    if caps.is_empty() {
        None
    } else {
        Some(caps)
    }
}

/// 壁纸变化(手动换壁纸/主题切换/幻灯片轮换)时请求重捕获。保留旧快照
/// (栅栏不呈现退化帧),清零节流哨兵,并同时武装追赶与跟随定时器:
/// 跟随定时器做"捕获-比对-变了才重绘",捕获到过渡黑帧时自动重试。
fn invalidate_wallpaper() {
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
        unsafe {
            let _ = SetTimer(tray, TIMER_WALLPAPER_FOLLOW, 250, None);
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
        unsafe {
            let _ = KillTimer(hwnd, TIMER_WALLPAPER_FOLLOW);
        }
        WALLPAPER_FOLLOW_RETRIES.store(0, Ordering::Relaxed);
        if changed {
            log("wallpaper follow: content changed -> redraw fences");
            refresh_all_fences();
        }
    }
}

/// 启动阶段首帧呈现日志只打一次的闸门
static BOOT_FIRST_PRESENT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
static WALLPAPER_CATCHUP_ARMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static WALLPAPER_CATCHUP_TRIES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// 武装壁纸追赶定时器(200ms)。幂等:已武装时直接返回,避免每次刷新
/// 重置计时周期导致永不触发。定时器到点在托盘窗口线程回调,与所有
/// 调用方同线程,无需加锁。
fn arm_wallpaper_catchup() {
    if WALLPAPER_CATCHUP_ARMED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(&tray) = TRAY_HWND.get() {
        if unsafe { SetTimer(tray, TIMER_WALLPAPER_CATCHUP, 200, None) } != 0 {
            WALLPAPER_CATCHUP_ARMED.store(true, Ordering::Relaxed);
            WALLPAPER_CATCHUP_TRIES.store(0, Ordering::Relaxed);
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
        unsafe {
            let _ = KillTimer(hwnd, TIMER_WALLPAPER_CATCHUP);
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
        unsafe {
            let _ = KillTimer(hwnd, TIMER_WALLPAPER_CATCHUP);
        }
        WALLPAPER_CATCHUP_ARMED.store(false, Ordering::Relaxed);
        log("wallpaper catch-up gave up; steady tick takes over");
    }
}

fn refresh_fence_impl(s: &mut UiState, fence_id: u32) {
    // 启动守卫标记:精确模式在首帧种子就绪前不呈现新帧(借用 fence 前先取出)
    let precise = precise_mode_on();
    // 注意:此处不再 ensure_wallpaper。PrintWindow(RENDERFULLCONTENT) 抓桌面宿主
    // 会强制桌面重绘,点击/菜单收尾触发的栅栏刷新会因此闪整个桌面。
    // 壁纸快照只由全局定时器(3s 节流+内容和校验)更新,刷新栅栏永远用现有快照。
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
        return;
    };
    let Some(hwnd) = s.windows.get(&fence_id).copied() else {
        return;
    };
    let Some(renderer) = &s.renderer else { return };
    if fence.hidden {
        // 我们主动隐藏时标记，避免被 WM_WINDOWPOSCHANGING 的"防最小化"拦截误伤
        INTENTIONAL_HIDE.store(true, Ordering::SeqCst);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        INTENTIONAL_HIDE.store(false, Ordering::SeqCst);
        return;
    }
    // 精确模式下壁纸快照未就绪时绝不呈现新帧:
    // - 首帧未出:保持原生桌面(原生文字本就带阴影,观感无缝),快照就绪后
    //   一次到位,消除"启动 1-3 秒后阴影才出现"的中间态;
    // - 已有旧帧(壁纸刚失效/重捕获暂败):保留旧帧,等快照跟上再整帧重绘,
    //   避免文字在 ClearType 与 D2D 之间来回切换。
    // 拖拽中不适用(交互连续性优先)。
    if precise && s.wallpapers.is_empty() && s.drag.is_none() {
        arm_wallpaper_catchup();
        return;
    }
    // 顶层窗口:直接使用屏幕坐标。defer_show_until_batch 批量呈现期间跳过
    // 显示动作:先对所有栅栏完成绘制+向隐藏窗提交 ULW(UpdateLayeredWindow
    // 对隐藏窗口同样有效,像素暂存),由调用方循环结束后一并放行。
    if !s.defer_show_until_batch {
        let _z = z_scope(ZIntent::Show);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // 强制置前显示:仅 SW_SHOW 有时不足以让分层窗口重新可见,
            // 这里显式 SWP_SHOWWINDOW 兜底(问题「显示全部不生效」)。
            let _ = SetWindowPos(
                hwnd,
                None,
                fence.rect.x.round() as i32,
                fence.rect.y.round() as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }
    }
    let w = fence.rect.w.ceil() as u32;
    let h = fence.rect.h.ceil() as u32;
    if w < 2 || h < 2 {
        return;
    }
    let needs_new = match s.surfaces.get(&fence_id) {
        Some(sf) => sf.w != w || sf.h != h,
        None => true,
    };
    if needs_new {
        s.presented.remove(&fence_id);
        if let Some(old) = s.surfaces.remove(&fence_id) {
            render::release_surface(old);
        }
        if let Some(sf) = render::create_surface(&renderer.factory, w, h) {
            s.surfaces.insert(fence_id, sf);
        } else {
            log(&format!("surface create failed fence {}", fence_id));
            return;
        }
    }
    let metrics = s
        .windows
        .get(&fence_id)
        .copied()
        .map(metrics_for_window)
        .unwrap_or_else(model::DpiMetrics::system);
    s.metrics.insert(fence_id, metrics);
    let items = model::display_list(fence, &s.files);
    let lay = model::layout_with_metrics(fence, items.len(), &metrics);
    let hover = *s.hover.get(&fence_id).unwrap_or(&None);
    // fence_hovered 在渲染侧仅控制 chrome 显隐;托盘"显示栅栏边框线"打开时
    // 全部栅栏常显边框(无边框常显基线的可观察模式)
    let fence_hovered =
        *s.fence_hover.get(&fence_id).unwrap_or(&false) || chrome_always_on();
    let active = matches!(&s.drag, Some(d) if d.fence_id == fence_id
        && matches!(d.mode, DragMode::Move | DragMode::Resize { .. }));
    let marquee = s.marquee;
    // 种子快照:选出覆盖本栅栏中心的那块壁纸(精确模式必有;透明模式有则
    // 边缘色更准,无则黑种子兜底——两种渲染模式共用同一条 seeded 文字管线)
    let cx = fence.rect.x + fence.rect.w * 0.5;
    let cy = fence.rect.y + fence.rect.h * 0.5;
    let wp_for_fence = s.wallpapers.iter().find(|wp| {
        cx >= wp.origin_x as f32
            && cx < (wp.origin_x + wp.w as i32) as f32
            && cy >= wp.origin_y as f32
            && cy < (wp.origin_y + wp.h as i32) as f32
    });
    let surf_ref = s.surfaces.get(&fence_id).unwrap();
    let t_draw0 = resize_now_ms();
    let jobs = render::draw_fence(
        &surf_ref.target,
        renderer,
        fence,
        &metrics,
        &lay,
        &items,
        &mut s.icon_cache,
        hover,
        &s.selected_paths,
        s.focused_path.as_deref(),
        fence_hovered,
        active,
        marquee,
    );
    let t_draw = resize_now_ms() - t_draw0;
    let pos = POINT {
        x: fence.rect.x.round() as i32,
        y: fence.rect.y.round() as i32,
    };
    let t_gdi0 = resize_now_ms();
    // ink 常驻:统一 seeded GDI ClearType 文字。有快照=真实底色种子(逐位
    // 同原生),无=黑种子兜底;墨水外像素保持透明,背景透出实时壁纸
    render::gdi_draw_labels_seeded(surf_ref, &jobs, wp_for_fence, pos.x, pos.y);
    let t_gdi = resize_now_ms() - t_gdi0;
    let t_pres0 = resize_now_ms();
    let ok = render::present_surface(surf_ref, hwnd, pos.x, pos.y);
    let t_pres = resize_now_ms() - t_pres0;
    if BOOT_VERBOSE.load(Ordering::Relaxed) && (t_draw > 100 || t_gdi > 100 || t_pres > 100) {
        log(&format!(
            "boot slow draw fence {}: draw={}ms gdi={}ms present={}ms items={}",
            fence_id, t_draw, t_gdi, t_pres, items.len()
        ));
    }
    if ok {
        let first_ever = BOOT_FIRST_PRESENT.swap(false, Ordering::Relaxed);
        if first_ever {
            log(&format!(
                "boot first fence presented (id={}, seeded={}, {}ms)",
                fence_id,
                wp_for_fence.is_some(),
                resize_now_ms()
            ));
        }
        s.presented.insert(fence_id);
    } else {
        s.presented.remove(&fence_id);
        log(&format!("present failed fence {}", fence_id));
    }
}

/// 刷新单个栅栏
pub fn present_fence_only(fence_id: u32) {
    let s = state().lock().unwrap();
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
        return;
    };
    let Some(hwnd) = s.windows.get(&fence_id).copied() else {
        return;
    };
    let Some(surface) = s.surfaces.get(&fence_id) else {
        return;
    };
    let _ = render::present_existing_surface(
        surface,
        hwnd,
        fence.rect.x.round() as i32,
        fence.rect.y.round() as i32,
    );
}

fn refresh_fence(fence_id: u32) {
    let t0 = resize_now_ms();
    let hosts = desktop_hosts();
    let mut s = state().lock().unwrap();
    if !s.windows.contains_key(&fence_id) {
        create_fence_window(&mut s, fence_id, &hosts);
    }
    if s.windows.contains_key(&fence_id) {
        refresh_fence_impl(&mut s, fence_id);
    }
    if BOOT_VERBOSE.load(Ordering::Relaxed) {
        log(&format!(
            "boot refresh_fence {} took {}ms (icons_cum={}ms/{}miss)",
            fence_id,
            resize_now_ms() - t0,
            render::ICON_EXTRACT_MS.load(Ordering::Relaxed),
            render::ICON_EXTRACT_COUNT.load(Ordering::Relaxed)
        ));
    }
}

fn ensure_fence_window(id: u32) {
    let hosts = desktop_hosts();
    let mut s = state().lock().unwrap();
    if !s.windows.contains_key(&id) {
        create_fence_window(&mut s, id, &hosts);
    }
}

/// 启动阶段第一次全量刷新的逐栅栏计时开关(启动结束关闭,避免常态刷日志)
static BOOT_VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 用一次性小表面预热渲染管线:D2D 设备/画刷/壁纸位图创建、GDI 字体与
/// DrawShadowText 动态加载的首次使用合计可达 300-900ms。趁主线程等待
/// 后台 shell 预热 join 的空闲窗口先烧掉,真正首帧只剩纯绘制成本。
fn warm_renderer_scratch() {
    let t0 = resize_now_ms();
    let mut guard = state().lock().unwrap();
    // 经 Deref 的 MutexGuard 无法做字段级分裂借用,先重借用为 &mut UiState
    let s: &mut UiState = &mut guard;
    let Some(renderer) = &s.renderer else { return };
    let Some(sf) = render::create_surface(&renderer.factory, 96, 96) else {
        return;
    };
    let dummy = Fence {
        id: 0,
        title: String::new(),
        category: String::new(),
        pinned: Vec::new(),
        item_order: Vec::new(),
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 90.0,
            h: 90.0,
        },
        collapsed: false,
        scroll_rows: 0,
        locked: false,
        hidden: false,
        manual_size: false,
        sort_mode: String::new(),
    };
    let metrics = model::DpiMetrics::system();
    let lay = model::layout_with_metrics(&dummy, 0, &metrics);
    let wp = s.wallpapers.first();
    let _ = render::draw_fence(
        &sf.target,
        renderer,
        &dummy,
        &metrics,
        &lay,
        &[],
        &mut s.icon_cache,
        None,
        &std::collections::HashSet::new(),
        None,
        false,
        false,
        None,
    );
    // 空作业时标签绘制会早退,补一个 1 字符作业触发
    // DrawShadowText 加载 + 字体创建 + ClearType 首次栅格化(含种子路径)
    let warm_job = [render::GdiLabelJob {
        text: "W".into(),
        x: 2.0,
        y: 2.0,
        w: 60.0,
        h: 30.0,
    }];
    render::gdi_draw_labels_seeded(&sf, &warm_job, wp, 0, 0);
    render::release_surface(sf);
    log(&format!("boot renderer warm-up took {}ms", resize_now_ms() - t0));
}

/// 完整恢复 DeskFence 桌面：重新发现 Explorer 宿主、重新挂接/创建窗口、
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
fn ensure_missing_category_fences(s: &mut UiState) -> Vec<String> {
    let mut have: std::collections::HashSet<String> = s
        .fences
        .iter()
        .filter(|f| !f.category.is_empty())
        .map(|f| f.category.clone())
        .collect();
    let mut added = Vec::new();
    if auto_category() {
        for cat in model::CATEGORIES {
            if have.contains(cat) {
                continue;
            }
            if s.files.iter().any(|f| f.category == cat) {
                added.push(cat.to_string());
                have.insert(cat.to_string());
            }
        }
    } else if !have.contains(model::UNCATEGORIZED) {
        // 自定义分类模式:未分配文件都进"未分类",保证没有任何文件隐身
        added.push(model::UNCATEGORIZED.to_string());
    }
    for cat in &added {
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        s.fences.push(Fence {
            id: max_id,
            title: cat.clone(),
            category: cat.clone(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: Rect {
                x: 60.0 + max_id as f32 * 40.0,
                y: 60.0,
                ..{
                    let (dw, dh) = default_fence_size();
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w: dw,
                        h: dh,
                    }
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
    added
}

/// 桌面文件变更刷新
pub fn rescan() {
    let files = with_recycle_bin(shell::scan_desktop());
    let (added_paths, removed_any) = {
        let s = state().lock().unwrap();
        let added = model::newly_added_paths(&s.files, &files);
        let new_set: std::collections::HashSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();
        let removed = s.files.iter().any(|f| !new_set.contains(f.path.as_str()));
        (added, removed)
    };
    if added_paths.is_empty() && !removed_any {
        // 文件集合没有任何变化:桌面目录的文件系统事件(Explorer 的元数据
        // 触碰、菜单交互的伴生事件)不值得做任何重绘。此前的无条件
        // show_all_fences 让每次 watcher dirty 都全量重绘 5 个栅栏,
        // 表现为点桌面/关菜单后栅栏区域闪一下。
        return;
    }
    let new_cats: Vec<String> = {
        let mut s = state().lock().unwrap();
        s.files = files;
        // 桌面变更可能同时改变快捷方式目标、文件关联或 Shell overlay 状态。
        // 缓存键还包含像素尺寸，因此扫描时统一失效可避免保留陈旧图标。
        let keep: std::collections::HashSet<String> =
            s.files.iter().map(|f| f.path.clone()).collect();
        s.icon_cache.clear();
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
        ensure_missing_category_fences(&mut s)
    };
    {
        let added_any = !new_cats.is_empty();
        let mut s = state().lock().unwrap();
        for cat in new_cats {
            let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
            s.fences.push(Fence {
                id: max_id,
                title: cat.clone(),
                category: cat,
                pinned: Vec::new(),
                item_order: Vec::new(),
                rect: Rect {
                    x: 60.0 + max_id as f32 * 40.0,
                    y: 60.0,
                    ..{
                        let (dw, dh) = default_fence_size();
                        Rect {
                            x: 0.0,
                            y: 0.0,
                            w: dw,
                            h: dh,
                        }
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
        // 只有真的新增了分类栅栏才收敛；周期 rescan 不应把用户手动摆放的
        // 位置重排回左上角（推挤式保留相对位置，而不是流式重排）。
        drop(s);
        if added_any {
            settle_preserve_positions();
        }
    }
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    rebuild_pins();
    refit_auto_fence_heights();
    show_all_fences();
    start_arrival_animations(&added_paths);
}

/// 默认栅栏尺寸:2 列宽 × 5 行高(用户指定;内容超出自动滚动)
fn default_fence_size() -> (f32, f32) {
    // 默认高度 4 行:一屏可上下放两排栅栏(build_global_config 的兜底同规则)
    let (title_h, pad) = model::chrome(model::dpi_scale());
    (
        model::cell_w() * 2.0 + pad * 2.0 + 2.0,
        title_h + model::cell_h() * 4.0 + pad * 2.0 + 2.0,
    )
}

/// 扫描结果注入回收站虚拟条目(固定显示在"软件"栅栏第一位,可拖拽文件进去删除)
fn with_recycle_bin(mut files: Vec<FileItem>) -> Vec<FileItem> {
    if !files.iter().any(|f| model::is_recycle_bin(&f.path)) {
        files.push(model::recycle_bin_item());
    }
    files
}

fn client_area_animations_enabled() -> bool {
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
        let Some(fence) = s
            .fences
            .iter()
            .find(|fence| {
                fence.pinned.contains(path)
                    || fence.category.is_empty()
                    || fence.category == item.category
            })
            .cloned()
        else {
            continue;
        };
        if fence.hidden || fence.collapsed {
            continue;
        }
        let items = model::display_list(&fence, &s.files);
        let Some(index) = items
            .iter()
            .position(|candidate| candidate.path == item.path)
        else {
            continue;
        };
        let metrics = s
            .metrics
            .get(&fence.id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        let layout = model::layout_with_metrics(&fence, items.len(), &metrics);
        if index < layout.first_index || index >= layout.first_index + layout.visible {
            continue;
        }
        let (cell_x, cell_y) = model::cell_pos_with_metrics(&layout, index, &metrics);
        let icon_offset = (metrics.cell_w - metrics.icon_px) / 2.0;
        let to = (
            fence.rect.x + cell_x + icon_offset,
            fence.rect.y + cell_y + 4.0,
        );
        // 新文件先"落在桌面空白处"(围栏外的原生网格位),停留片刻再飞入栅栏;
        // 找不到围栏外空位时回退为从栅栏标题中心飞出
        let from = desktop_free_slot(&s, &mut used_slots).unwrap_or((
            fence.rect.x + fence.rect.w * 0.5 - metrics.icon_px * 0.5,
            (fence.rect.y + metrics.title_h * 0.5 - metrics.icon_px * 0.5).max(0.0),
        ));
        pending.push(ArrivalAnimation {
            fence_id: fence.id,
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
    s.arrival_animations.extend(pending);
    ensure_guide_window(&mut s);
    if let Some(tray) = TRAY_HWND.get().copied() {
        unsafe {
            let _ = SetTimer(tray, TIMER_ANIMATION, 16, None);
        }
    }
    refresh_guide(&mut s);
}

fn tick_arrival_animations() {
    let mut s = state().lock().unwrap();
    if s.arrival_animations.is_empty() {
        return;
    }
    ensure_guide_window(&mut s);
    refresh_guide(&mut s);
    if s.arrival_animations.is_empty() {
        if let Some(tray) = TRAY_HWND.get().copied() {
            unsafe {
                let _ = KillTimer(tray, TIMER_ANIMATION);
            }
        }
        if s.drag_ghost.is_none() && s.guide_x.is_none() && s.guide_y.is_none() {
            if let Some(hwnd) = s.guide_hwnd {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }
}

/// 启动初始化
pub fn startup() {
    log(&format!("boot begin ({}ms)", resize_now_ms()));
    // 崩溃恢复:上次运行隐藏了桌面图标但进程已死 → 先恢复原生图标
    if let Some(pid) = model::load_icons_marker() {
        if pid != std::process::id() {
            let _ = set_desktop_icons_visible(true);
            model::clear_icons_marker();
            log("restored desktop icons from previous dead session");
        }
    }
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
                log(&format!(
                    "boot restores desktop_state={}",
                    desktop_state()
                ));
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
        let _ = auto_category(); // 预热开关(读设置文件)
        SHOW_CHROME.store(model::load_settings().show_chrome, Ordering::Relaxed);
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
    let (icon_prewarm, t_icons) =
        bg_icons.join().unwrap_or_else(|_| (Default::default(), 0));
    let mut created_cats: Vec<String> = Vec::new();
    {
        let mut s = state().lock().unwrap();
        let n_files = files.len();
        s.files = files;
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
    if !created_cats.is_empty() {
        // 与 rescan 一致:补建后立即持久化(settle 之后的矩形才是最终位置)
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    refit_auto_fence_heights();
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
}

/// 全局自愈:定时器与显示变化时调用。
/// 1) 修复窗口与桌面宿主的挂接(启动竞态/Explorer 重启后自动补挂);
/// 2) 协调原生图标可见性;3) 图标尺寸/主题跟随;4) 桌面文件刷新。
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
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    if let Some(edit) = fence_edit {
        if !point_in_window_rect(edit, pt.x, pt.y) {
            unsafe {
                let _ = PostMessageW(edit, RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
        }
    }
    if let Some(edit) = file_edit {
        if !point_in_window_rect(edit, pt.x, pt.y) {
            log("COMMIT via timer fallback");
            unsafe {
                let _ = PostMessageW(edit, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn global_tick() {
    finish_rename_if_clicked_outside();

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
        if dirty != 0 && resize_now_ms().saturating_sub(dirty) > 4000 && t % 4 == 0 {
            ICON_SAVE_DIRTY_MS.store(0, Ordering::Relaxed);
            let px = model::DpiMetrics::system().icon_px.round().clamp(16.0, 256.0) as u32;
            save_icon_cache_file_now(px);
        }
    }
    // 注意:不要用 0x052C 消息生成 WorkerW —— 每次调用都会让 Win11 桌面层
    // 在 Progman/WorkerW 之间切换宿主,导致桌面反复重建(栅栏消失、桌面空白)。
    // 只用现有宿主,缺失时等待 Explorer 自然重建,由 ensure_all_attached 自愈。
    ensure_all_attached();
    let needs_represent = {
        let s = state().lock().unwrap();
        let expected = s.fences.iter().filter(|f| !f.hidden).count();
        expected > 0 && (s.attached.len() < expected || s.presented.len() < expected)
    };
    if needs_represent {
        // Win+D/three-finger/desktop-host rebuilds can preserve HWNDs while discarding
        // their layered presentation. 用现有表面立即重呈现(ULW 同一张位图,毫秒级)
        // 代替全量重绘——精确模式全量重绘 5 个栅栏要 1-2 秒,用户会看到桌面空白。
        invalidate_hosts_cache();
        ensure_all_attached();
        let ids: Vec<u32> = {
            let s = state().lock().unwrap();
            s.fences
                .iter()
                .filter(|f| !f.hidden)
                .map(|f| f.id)
                .collect()
        };
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
                s.wallpapers.is_empty()
                    && s.wallpaper_fails >= 2
                    && resize_now_ms() > 10_000
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
    if t % 3 == 0 {
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
    if t % 10 == 0 {
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
    if shell::take_desktop_dirty() || t % 30 == 0 {
        rescan();
    }
}

/// 确保所有栅栏窗口存在、存活,并且 z 序紧贴桌面宿主之后(自愈):
/// 顶层分层窗口 + 每秒重申插入位置 —— Explorer 重启、z 序漂移、启动竞态
/// 都能在 1 秒内自动修复;找不到宿主时窗口保持原 z 位(新桌面在其下,不浮窗),
/// 缺失窗口延迟到宿主就绪后创建。
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
}

fn ensure_all_attached() {
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
            if !s.windows.contains_key(&id) {
                if create_fence_window(&mut s, id, &hosts) {
                    created.push(id);
                }
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
                let mut anchor = band_attach_anchor(host.hwnd, h, false);
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

// ---------------- 托盘图标 ----------------

static DESKTOP_ICONS_HIDDEN: AtomicBool = AtomicBool::new(false);
/// User explicitly requested native desktop icons to remain visible.
static NATIVE_DESKTOP_OVERRIDE: AtomicBool = AtomicBool::new(false);
/// 纯净态:用户主动"隐藏全部栅栏"——栅栏与原生图标都隐藏,桌面只剩壁纸。
/// 图标协调逻辑在此状态下不因"无栅栏呈现"而恢复原生图标(那正是旧的
/// "隐藏栅栏=回到原生桌面"重复感的来源)。仅在本次运行内生效,重启回正常态。
static ZEN_MODE: AtomicBool = AtomicBool::new(false);

/// 图标缓存落盘调度状态(见 global_tick 内说明)
static ICON_EXTRACT_SEEN: AtomicU64 = AtomicU64::new(0);
static ICON_SAVE_DIRTY_MS: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn find_workerw_lv(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let slot: &mut Option<HWND> = &mut *(lparam.0 as *mut Option<HWND>);
    if slot.is_some() {
        return BOOL(0);
    }
    let mut buf = [0u16; 256];
    if GetClassNameW(hwnd, &mut buf) > 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let cls = String::from_utf16_lossy(&buf[..end]);
        // DefView 可能挂在 Progman 直下,也可能挂在任一 WorkerW 下(壁纸切换后),
        // 两种都接受 —— 实测某些环境下 FindWindowW("Progman") 会失败,必须靠枚举兜底
        if cls == "WorkerW" || cls == "Progman" {
            // 局部 Vec 保持字符串存活，避免临时指针悬垂（use-after-free）
            let defview_cls = shell::wide("SHELLDLL_DefView");
            let defview = FindWindowExW(hwnd, None, PCWSTR::from_raw(defview_cls.as_ptr()), None);
            if defview.0 != 0 {
                let lv_cls = shell::wide("SysListView32");
                let lv = FindWindowExW(defview, None, PCWSTR::from_raw(lv_cls.as_ptr()), None);
                if lv.0 != 0 {
                    *slot = Some(lv);
                    return BOOL(0);
                }
            }
        }
    }
    BOOL(1)
}

/// 查找桌面图标列表（SysListView32，Progman 或 WorkerW）
fn desktop_listview() -> Option<HWND> {
    unsafe {
        let progman_cls = shell::wide("Progman");
        let defview_cls = shell::wide("SHELLDLL_DefView");
        let lv_cls = shell::wide("SysListView32");
        let progman = FindWindowW(PCWSTR::from_raw(progman_cls.as_ptr()), None);
        if progman.0 != 0 {
            let defview =
                FindWindowExW(progman, None, PCWSTR::from_raw(defview_cls.as_ptr()), None);
            if defview.0 != 0 {
                let lv = FindWindowExW(defview, None, PCWSTR::from_raw(lv_cls.as_ptr()), None);
                if lv.0 != 0 {
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

/// 桌面壳窗口（WorkerW 或 Progman）：栅栏 SetWindowPos 插到它之后，
/// 即 z-order 位于桌面图标层之上、普通窗口之下（常驻桌面且不遮挡窗口）。
fn desktop_shell_window() -> Option<HWND> {
    let lv = desktop_listview()?;
    unsafe {
        let defview = GetParent(lv);
        if defview.0 != 0 {
            let parent = GetParent(defview);
            if parent.0 != 0 {
                return Some(parent);
            }
            return Some(defview);
        }
        None
    }
}

fn set_desktop_icons_visible(visible: bool) -> bool {
    let Some(lv) = desktop_listview() else {
        log("desktop listview not found");
        return false;
    };
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
        Some(h) => lines.push(format!("桌面宿主: 就绪 0x{:x} ✓", h.0)),
        None => {
            ok = false;
            lines.push("桌面宿主: 未找到(Explorer 桌面层未就绪)".into());
        }
    }
    // 3) 孤儿 DeskFence 窗口(死去实例的遗留)
    let me = std::process::id();
    let mut orphans = 0usize;
    unsafe {
        unsafe extern "system" fn enum_orphan(h: HWND, l: LPARAM) -> BOOL {
            let (me, count) = unsafe {
                let p = l.0 as *mut (u32, usize);
                (&(*p).0, &mut (*p).1)
            };
            let mut buf = [0u16; 32];
            let n = unsafe { GetClassNameW(h, &mut buf) };
            let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if cls.starts_with("DeskFence") {
                let mut pid = 0u32;
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
        lines.push(format!("窗口: 检测到 {orphans} 个孤儿 DeskFence 窗口(建议\"修复桌面环境\")"));
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
                for h in s.windows.values() {
                    let mut w = unsafe { GetWindow(host, GW_HWNDPREV) };
                    for _ in 0..600 {
                        if w.0 == 0 {
                            break;
                        }
                        if w == *h {
                            good += 1;
                            break;
                        }
                        w = unsafe { GetWindow(w, GW_HWNDPREV) };
                    }
                }
            }
            in_band = good;
        }
        if total > 0 && in_band == total {
            lines.push(format!("栅栏: {in_band}/{total} 在桌面层内 ✓"));
        } else {
            ok = false;
            lines.push(format!("栅栏: {in_band}/{total} 在桌面层内(自愈未完成或受阻)"));
        }
    } else {
        lines.push(format!(
            "栅栏: 桌面态 {} 栅栏按状态隐藏 ✓",
            desktop_state()
        ));
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
    std::thread::spawn(|| unsafe {
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

fn env_watchdog_tick() {
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
            log(&format!("env-watchdog: fault started, waiting self-heal ({report})"));
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

/// 协调原生桌面图标可见性。任何栅栏宿主/呈现状态异常都优先恢复原生图标，
/// 以保证用户绝不会得到空白桌面。
fn reconcile_desktop_icons() {
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
        let lv_vis =
            desktop_listview().is_some_and(|lv| unsafe { IsWindowVisible(lv).as_bool() });
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
        let re_showing = desktop_listview()
            .is_some_and(|lv| unsafe { IsWindowVisible(lv).as_bool() });
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
                let vis = s.windows.get(&f.id).is_some_and(|h| unsafe {
                    IsWindowVisible(*h).as_bool()
                });
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

fn toggle_desktop_icons() {
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

fn restore_desktop_icons() {
    if DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) {
        set_desktop_icons_visible(true);
        DESKTOP_ICONS_HIDDEN.store(false, Ordering::Relaxed);
    }
    model::clear_icons_marker();
}

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
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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
        if msg == WM_SETTINGCHANGE {
            rebuild_render_resources();
            invalidate_hosts_cache();
            // 壁纸可能变化:作废旧快照,重捕获(精确模式随之更新底图)
            invalidate_wallpaper();
            show_all_fences();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_GLOBAL as usize {
            global_tick();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_DESKTOP_WATCH as usize {
            // 桌面态快速自检:三指手势的窗口扫动不发任何 WinEvent(两轮
            // 实测零触发),恢复过渡只能靠 250ms 轮询兜住;band_quiet 由
            // 1s 走查维护,正常使用时这里什么都不做。
            if state().lock().unwrap().band_quiet {
                zcheck_fences_now();
            }
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_ANIMATION as usize {
            tick_arrival_animations();
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_WALLPAPER_CATCHUP as usize {
            wallpaper_catchup_tick(hwnd);
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_WALLPAPER_FOLLOW as usize {
            wallpaper_follow_tick(hwnd);
            return LRESULT(0);
        }
        if msg == WM_TIMER && wparam.0 == TIMER_RENAME_WATCH as usize {
            finish_rename_if_clicked_outside();
            let any_edit = state()
                .lock()
                .map(|s| s.rename_edit.is_some() || s.file_rename_edit.is_some())
                .unwrap_or(false);
            if !any_edit {
                let _ = KillTimer(hwnd, TIMER_RENAME_WATCH);
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
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        );
        if hwnd.0 == 0 {
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
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        );
        if menu_host.0 != 0 {
            let _ = ShowWindow(menu_host, SW_SHOWNOACTIVATE);
            let _ = MENU_HOST_HWND.set(menu_host);
        }
        // 全局低频自愈定时器：窗口挂接/图标协调/主题跟随/文件刷新。
        // 目录变化由 watcher 置位，避免在拖动期间以 100ms 频率扫描和重挂窗口。
        let _ = SetTimer(hwnd, TIMER_GLOBAL, 1000, None);
        // 桌面态快速自检:仅当走查判定 band_quiet(桌面态)时才做实事,
        // 正常使用(带内有可见外来窗)空转,零成本。
        let _ = SetTimer(hwnd, TIMER_DESKTOP_WATCH, 250, None);
        // 全局 z 序事件钩子:显示桌面等批量重排的毫秒级触发器(详见
        // zorder_event_cb 注释),高速自检走 WM_DL3_ZCHECK 合并投递。
        install_zorder_hooks();
        add_tray_icon(hwnd);
    }
}

fn show_tray_menu(x: i32, y: i32) {
    let hwnd = TRAY_HWND.get().copied().unwrap_or(HWND(0));
    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    // 两个状态感知切换项(用户约定):
    // 按钮1 栅栏可见性:正常态"隐藏全部栅栏"(→纯净态:只剩壁纸),
    //                 栅栏隐藏时"显示全部栅栏"(→回正常态);
    // 按钮2 桌面归属:正常/纯净态"恢复原始桌面"(→原生图标接管),
    //                原生态"恢复栅栏桌面"(→栅栏回归,图标重新隐藏)。
    let (all_hidden, icons_hidden) = {
        let s = state().lock().unwrap();
        (
            s.fences.iter().all(|f| f.hidden),
            DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed),
        )
    };
    if all_hidden {
        shell::append_menu(menu, MENU_SHOW_ALL, "显示全部栅栏");
    } else {
        shell::append_menu(menu, MENU_HIDE_ALL, "隐藏全部栅栏");
    }
    shell::append_separator(menu);
    shell::append_menu(menu, MENU_UNDO, "撤销上次布局调整");
    shell::append_menu(menu, MENU_RESET_LAYOUT, "恢复默认布局");
    shell::append_separator(menu);
    shell::append_menu(
        menu,
        MENU_TOGGLE_DESKTOP_ICONS,
        if DESKTOP_ICONS_HIDDEN.load(Ordering::Relaxed) {
            "显示桌面图标"
        } else {
            "隐藏桌面图标"
        },
    );
    // 原生图标可见且栅栏全部隐藏 = 原生桌面态,翻转为恢复栅栏
    let native_mode = all_hidden && !icons_hidden;
    if native_mode {
        shell::append_menu(menu, MENU_SHOW_ALL, "恢复栅栏桌面");
    } else {
        shell::append_menu(menu, MENU_RESTORE_DESKTOP, "恢复原始桌面");
    }
    let align = unsafe { CreatePopupMenu().unwrap_or_default() };
    let mode = align_mode();
    let modes = [
        (MENU_AUTO_ALIGN, "auto", "自动对齐(固定间隔)"),
        (MENU_ALIGN_GRID, "grid", "网格对齐(图标格倍数)"),
        (MENU_ALIGN_FREE, "free", "自由移动(不受限)"),
    ];
    for (id, key, label) in modes {
        if mode == key {
            shell::append_menu_checked(align, id, label);
        } else {
            shell::append_menu(align, id, label);
        }
    }
    shell::append_submenu(menu, "对齐方式", align);
    // 渲染模式:精确(默认,壁纸底+ClearType 与原生一致)在上;
    // 透明为兜底(动态壁纸不兼容时使用)
    let render = unsafe { CreatePopupMenu().unwrap_or_default() };
    let rmode = render_mode();
    let rmodes = [
        (MENU_RENDER_PRECISE, "precise", "精确(与原生逐像素一致)"),
        (
            MENU_RENDER_TRANSPARENT,
            "transparent",
            "透明(兜底:动态壁纸不兼容时)",
        ),
    ];
    for (id, key, label) in rmodes {
        if rmode == key {
            shell::append_menu_checked(render, id, label);
        } else {
            shell::append_menu(render, id, label);
        }
    }
    shell::append_submenu(menu, "渲染模式", render);
    if auto_category() {
        shell::append_menu_checked(menu, MENU_AUTO_CATEGORY, "自动分类(默认8类)");
    } else {
        shell::append_menu(menu, MENU_AUTO_CATEGORY, "自动分类(默认8类)");
    }
    if chrome_always_on() {
        shell::append_menu_checked(menu, MENU_TOGGLE_CHROME, "显示栅栏边框线");
    } else {
        shell::append_menu(menu, MENU_TOGGLE_CHROME, "显示栅栏边框线");
    }
    shell::append_menu(menu, MENU_HELP, "使用说明");
    // 桌面环境体检/修复:全自动机制(boot 体检 + 30s watchdog),不提供
    // 手动入口(用户要求,2026-08-29)。
    if shell::get_autostart() {
        shell::append_menu_checked(menu, MENU_AUTOSTART, "开机自启");
    } else {
        shell::append_menu(menu, MENU_AUTOSTART, "开机自启");
    }
    shell::append_separator(menu);
    shell::append_menu(menu, MENU_QUIT, "退出");
    let id = track(menu, hwnd, x, y);
    unsafe {
        let _ = DestroyMenu(align);
        let _ = DestroyMenu(render);
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    }
    dispatch_tray_command(id);
}

fn dispatch_tray_command(id: u32) {
    match id {
        MENU_SHOW_ALL => {
            // "显示全部栅栏"/"恢复栅栏桌面"共用:回到正常态,栅栏回归,
            // 图标协调随栅栏呈现自动重新隐藏原生图标
            ZEN_MODE.store(false, Ordering::Relaxed);
            set_desktop_state_stored("normal");
            show_all_fences();
        }
        MENU_HIDE_ALL => {
            // 纯净态:栅栏全部隐藏且原生图标保持隐藏(桌面只剩壁纸)
            ZEN_MODE.store(true, Ordering::Relaxed);
            set_desktop_state_stored("zen");
            set_all_hidden(true);
            log("zen mode: all fences hidden, native icons stay hidden");
        }
        MENU_UNDO => undo_layout(),
        MENU_RESET_LAYOUT => reset_fence_layout(),
        MENU_TOGGLE_DESKTOP_ICONS => toggle_desktop_icons(),
        MENU_RESTORE_DESKTOP => restore_original_desktop(),
        MENU_AUTO_ALIGN => set_align_mode("auto"),
        MENU_ALIGN_GRID => set_align_mode("grid"),
        MENU_ALIGN_FREE => set_align_mode("free"),
        MENU_RENDER_TRANSPARENT => set_render_mode("transparent"),
        MENU_RENDER_PRECISE => set_render_mode("precise"),
        MENU_AUTO_CATEGORY => toggle_auto_category(),
        MENU_TOGGLE_CHROME => {
            let on = !chrome_always_on();
            set_show_chrome_stored(on);
            refresh_all_fences();
            log(&format!("show_chrome={on}"));
        }
        MENU_HELP => show_help(),
        MENU_AUTOSTART => toggle_autostart(),
        MENU_QUIT => quit_app(),
        _ => log(&format!("unknown tray command: {}", id)),
    }
}

/// 设置渲染模式并立即生效(作废壁纸快照,全部栅栏重绘)
fn set_render_mode(mode: &str) {
    set_render_mode_stored(mode);
    invalidate_wallpaper();
    refresh_all_fences();
    log(&format!("render_mode={mode}"));
}

/// 软件内使用说明(托盘/桌面右键菜单"使用说明")
fn show_help() {
    let text = "DeskFence 桌面整理 · 使用说明

【默认自动分类(8类)】
桌面文件按类型自动进入对应栅栏:
· 软件:exe/msi/lnk/bat 等程序与快捷方式
· 文件夹:所有目录
· 文档:txt/word/excel/ppt/pdf 等
· 图片:jpg/png/gif/svg 等
· 媒体:mp3/wav/mp4/mkv 等音视频
· 代码:py/js/ts/rs/go/c/cpp/html/json/md 等
· 压缩包:zip/rar/7z/tar/gz 等
· 其他:未识别的类型
某类栅栏不存在时,首次出现该类文件会自动新建。

【自定义分类模式】
托盘菜单取消勾选\"自动分类(默认8类)\"即切换为自定义模式:
· 不再按文件类型归类,文件只属于你拖它进去的栅栏
· 用\"新建栅栏\"自由创建并命名(右键标题可重命名/删除)
· 把图标从一个栅栏拖到另一个栅栏上松手即完成分配
· 未分配的文件集中在\"未分类\"栅栏,不会丢失
· 重新勾选\"自动分类\"即恢复 8 类默认模式

【其他】
拖动栅栏经过两个栅栏之间出现插入线,松手即插入;靠近屏幕边/角自动吸附;
拖到其他栅栏正上/下方自动保持固定间距;Esc 取消拖动;拖图标到\"回收站\"删除;
右键栅栏标题可折叠/锁定/重命名/删除。
托盘菜单勾选\"显示栅栏边框线\"可常显全部栅栏边框(默认隐藏,悬停浮现)。";
    let t = shell::wide(text);
    let cap = shell::wide("DeskFence 使用说明");
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            None,
            PCWSTR::from_raw(t.as_ptr()),
            PCWSTR::from_raw(cap.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MB_OK
                | windows::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION,
        );
    }
}

/// 切换自动分类:开=固定8类自动归类;关=自定义分类(新建栅栏自由命名,
/// 文件拖进哪个栅栏就属于它,未分配的集中在"未分类"栅栏)
fn toggle_auto_category() {
    let v = !auto_category();
    set_auto_category_stored(v);
    log(&format!("auto_category={v}"));
    if !v {
        // 自定义模式必须有"未分类"兜底,保证没有文件隐身
        let need = {
            let s = state().lock().unwrap();
            !s.fences.iter().any(|f| f.category == model::UNCATEGORIZED)
        };
        if need {
            let mut s = state().lock().unwrap();
            let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
            let (dw, dh) = default_fence_size();
            s.fences.push(Fence {
                id: max_id,
                title: model::UNCATEGORIZED.to_string(),
                category: model::UNCATEGORIZED.to_string(),
                pinned: Vec::new(),
                item_order: Vec::new(),
                rect: Rect {
                    x: 60.0 + max_id as f32 * 40.0,
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
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
    }
    rebuild_pins();
    settle_preserve_positions();
    refresh_all_fences();
}

/// 切换开机自启(HKCU Run)
fn toggle_autostart() {
    let on = !shell::get_autostart();
    let ok = shell::set_autostart(on);
    log(&format!("autostart={} ok={}", on, ok));
}

/// 设置对齐模式并立即生效;切到"自动"时按固定间隔重排整组
fn set_align_mode(mode: &str) {
    set_align_mode_stored(mode);
    log(&format!("align_mode={}", mode));
    if mode == "auto" {
        let areas = all_work_areas();
        let mut s = state().lock().unwrap();
        let mut rects: Vec<Rect> = s.fences.iter().map(|f| f.rect).collect();
        let (vx, vy, vw, vh) = work_area();
        // 全组依次链式对齐(以最左为主锚,逐个向右排)实现等距整理
        let n = rects.len();
        for anchor in 0..n {
            model::align_local_chain(&mut rects, anchor, vx, vy, vw, vh);
        }
        model::fit_to_monitors(&mut rects, &areas);
        for (f, r) in s.fences.iter_mut().zip(rects) {
            f.rect = r;
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    refresh_all_fences();
}

#[allow(dead_code)]
/// 一键退出：删除托盘图标、恢复桌面图标、结束消息循环，栅栏窗口随之消失，桌面恢复原样
pub fn quit_app() {
    log("quit requested");
    model::save_usage();
    // 退出前持久化壁纸快照(此刻栅栏还接管着桌面、原生图标隐藏,快照干净);
    // 下次启动直接加载,首帧立即可用
    if let Ok(s) = state().try_lock() {
        if !s.wallpapers.is_empty() {
            save_wallpaper_cache(&s.wallpapers);
        }
    }
    restore_desktop_icons();
    uninstall_keyboard_hook();
    uninstall_mouse_hook();
    {
        let mut s = state().lock().unwrap();
        if let Some(sf) = s.guide_surface.take() {
            render::release_surface(sf);
        }
        if let Some(h) = s.guide_hwnd.take() {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
    }
    unsafe {
        if let Some(&hwnd) = TRAY_HWND.get() {
            let mut n: NOTIFYICONDATAW = std::mem::zeroed();
            n.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            n.hWnd = hwnd;
            n.uID = 1;
            let _ = Shell_NotifyIconW(NIM_DELETE, &n);
        }
        PostQuitMessage(0);
    }
}

/// 恢复默认布局：删除所有栅栏并按当前桌面文件重新生成初始布局（文件本身不动）
fn reset_fence_layout() {
    push_undo();
    let ids: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences.iter().map(|f| f.id).collect()
    };
    for id in ids {
        delete_fence(id);
    }
    {
        let mut s = state().lock().unwrap();
        let base = model::build_global_config(&s.files);
        let mut fences = if base.is_empty() {
            let (dw, dh) = default_fence_size();
            vec![Fence {
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
            }]
        } else {
            base
        };
        // 一键恢复：流式左/上对齐排列（自动排列）
        let (vx, vy, vw, vh) = work_area();
        let mut rects: Vec<Rect> = fences.iter().map(|f| f.rect).collect();
        model::auto_layout(&mut rects, vx, vy, vw, vh);
        for (f, r) in fences.iter_mut().zip(rects) {
            f.rect = r;
        }
        s.fences = fences;
    }
    settle_all_fences();
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    show_all_fences();
}

pub fn run_message_loop() -> i32 {
    loop {
        let mut msg = MSG::default();
        unsafe {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                break;
            }
            if r.0 == -1 {
                log("getmessage error");
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

// ---------------- 撤销 / 键盘微调 / 键盘钩子 ----------------

static UNDO_STACK: OnceLock<Mutex<Vec<Vec<Fence>>>> = OnceLock::new();

fn undo_stack() -> &'static Mutex<Vec<Vec<Fence>>> {
    UNDO_STACK.get_or_init(|| Mutex::new(Vec::new()))
}

/// 压入当前布局快照(布局类操作前调用,支持撤销)
fn push_undo() {
    let snap = state().lock().unwrap().fences.clone();
    let mut u = undo_stack().lock().unwrap();
    if u.last().map(|l| *l == snap).unwrap_or(false) {
        return;
    }
    u.push(snap);
    if u.len() > 20 {
        u.remove(0);
    }
}

/// 拖动/缩放前压入"拖动前"快照(拖动过程中矩形已被实时更新)。
/// 调用方已持有 state 锁时传入其克隆的快照,避免同线程重复加锁死锁。
fn push_undo_snapshot(mut snap: Vec<Fence>, fence_id: u32, rect: Rect) {
    if let Some(f) = snap.iter_mut().find(|f| f.id == fence_id) {
        f.rect = rect;
    }
    let mut u = undo_stack().lock().unwrap();
    if u.last().map(|l| *l == snap).unwrap_or(false) {
        return;
    }
    u.push(snap);
    if u.len() > 20 {
        u.remove(0);
    }
}

fn undo_layout() {
    let snap = undo_stack().lock().unwrap().pop();
    if let Some(fences) = snap {
        if fences.is_empty() {
            log("ignored empty undo snapshot to prevent blank desktop");
            return;
        }
        {
            let mut s = state().lock().unwrap();
            s.fences = fences;
        }
        // Snapshots are already valid layouts; do not "settle" them again or
        // the restored positions cease to be the exact previous operation.
        {
            let s = state().lock().unwrap();
            let _ = model::save_config(&s.fences);
        }
        show_all_fences();
        log("layout undo applied");
    }
}

/// 方向键微调栅栏位置(光标悬停在栅栏上时生效;Ctrl = 1px 微调,否则按图标网格步进)
fn nudge_fence(fence_id: u32, vk: u32) {
    let ctrl = (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
    let step_x = if ctrl { 1.0 } else { model::grid_x() };
    let step_y = if ctrl { 1.0 } else { model::grid_y() };
    let (dx, dy) = if vk == VK_LEFT.0 as u32 {
        (-step_x, 0.0)
    } else if vk == VK_RIGHT.0 as u32 {
        (step_x, 0.0)
    } else if vk == VK_UP.0 as u32 {
        (0.0, -step_y)
    } else if vk == VK_DOWN.0 as u32 {
        (0.0, step_y)
    } else {
        return;
    };
    push_undo();
    {
        let mut s = state().lock().unwrap();
        let nr = {
            let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) else {
                return;
            };
            if f.locked {
                return;
            }
            f.rect.x += dx;
            f.rect.y += dy;
            f.rect
        };
        let others: Vec<Rect> = s
            .fences
            .iter()
            .filter(|x| x.id != fence_id)
            .map(|x| x.rect)
            .collect();
        let (vx, vy, vw, vh) = work_area_for_rect(&nr);
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
            f.rect = model::avoid_overlap(&nr, &others, vx, vy, vw, vh);
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    refresh_fence(fence_id);
}

fn dispatch_file_key(fence_id: u32, packed: usize) {
    let vk = (packed & 0xffff) as u32;
    let ctrl = packed & (1 << 16) != 0;
    let shift = packed & (1 << 24) != 0;
    // Esc 取消拖拽预览:回滚原始顺序并结束拖拽(与原生桌面一致)
    if vk == VK_ESCAPE.0 as u32 {
        let mut s = state().lock().unwrap();
        if s.drag_ghost.is_some() {
            s.drag_ghost = None;
            s.insert_line = None;
            rollback_ghost_preview(&mut s);
            drop(s);
            refresh_fence(fence_id);
            update_guides(None, None);
            unsafe {
                let _ = ReleaseCapture();
            }
            log("icon drag cancelled by Esc: rolled back");
            return;
        }
        // Esc 取消栅栏拖动/缩放:恢复按下快照的全体位置(未保存配置,天然回退)
        let geom_drag = matches!(s.drag.as_ref(), Some(d) if d.fence_id == fence_id
            && matches!(d.mode, DragMode::Move | DragMode::Resize { .. }));
        if geom_drag {
            s.insert_line = None;
            if let Some(drag) = s.drag.take() {
                let snaps: HashMap<u32, Rect> =
                    drag.start_layout.iter().map(|f| (f.id, f.rect)).collect();
                for f in s.fences.iter_mut() {
                    if let Some(r) = snaps.get(&f.id) {
                        f.rect = *r;
                    }
                }
                s.marquee = None;
                drop(s);
                unsafe {
                    let _ = ReleaseCapture();
                }
                refresh_all_fences();
                update_guides(None, None);
                log("fence drag cancelled by Esc: restored snapshot");
            }
        }
        return;
    }
    let mut open_paths = Vec::new();
    let mut rename = None;
    let mut delete_paths = Vec::new();
    let mut redraw = false;
    {
        let mut s = state().lock().unwrap();
        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id).cloned() else {
            return;
        };
        let items = model::display_list(&fence, &s.files);
        let focused_idx = s
            .focused_path
            .as_ref()
            .and_then(|p| items.iter().position(|it| &it.path == p));
        if ctrl && vk == 'A' as u32 {
            s.selected_paths = items.iter().map(|it| it.path.clone()).collect();
            s.focused_path = items.first().map(|it| it.path.clone());
            s.selection_anchor = s.focused_path.clone();
            redraw = true;
        } else if vk == VK_RETURN.0 as u32 {
            open_paths = items
                .iter()
                .filter(|it| s.selected_paths.contains(&it.path))
                .map(|it| it.path.clone())
                .collect();
        } else if vk == 0x71 {
            // VK_F2
            if s.selected_paths.len() == 1 {
                rename = s
                    .selected_paths
                    .iter()
                    .next()
                    .filter(|p| !model::is_recycle_bin(p))
                    .cloned();
            }
        } else if vk == 0x2E {
            // VK_DELETE(回收站虚拟条目不可删除)
            delete_paths = items
                .iter()
                .filter(|it| {
                    s.selected_paths.contains(&it.path) && !model::is_recycle_bin(&it.path)
                })
                .map(|it| it.path.clone())
                .collect();
        } else if vk == VK_LEFT.0 as u32
            || vk == VK_RIGHT.0 as u32
            || vk == VK_UP.0 as u32
            || vk == VK_DOWN.0 as u32
        {
            if let Some(current) = focused_idx {
                let lay = model::layout(&fence, items.len());
                let target = if vk == VK_LEFT.0 as u32 {
                    current.saturating_sub(1)
                } else if vk == VK_RIGHT.0 as u32 {
                    (current + 1).min(items.len().saturating_sub(1))
                } else if vk == VK_UP.0 as u32 {
                    current.saturating_sub(lay.cols)
                } else {
                    (current + lay.cols).min(items.len().saturating_sub(1))
                };
                if let Some(next) = items.get(target) {
                    let path = next.path.clone();
                    if shift {
                        let anchor = s
                            .selection_anchor
                            .as_ref()
                            .and_then(|p| items.iter().position(|it| &it.path == p))
                            .unwrap_or(current);
                        s.selected_paths.clear();
                        for idx in model::indices_between(&lay, anchor, target, items.len()) {
                            if let Some(it) = items.get(idx) {
                                s.selected_paths.insert(it.path.clone());
                            }
                        }
                    } else {
                        s.selected_paths.clear();
                        s.selected_paths.insert(path.clone());
                        s.selection_anchor = Some(path.clone());
                    }
                    s.focused_path = Some(path);
                    redraw = true;
                }
            }
        }
    }
    if let Some(path) = rename {
        log("TRIGGER f2");
        start_file_rename(path);
    }
    if ctrl && matches!(vk, 0x43 | 0x58) {
        let paths: Vec<String> = {
            let s = state().lock().unwrap();
            s.selected_paths.iter().cloned().collect()
        };
        let _ = ole::clipboard_set_files(&paths);
    }
    if ctrl && vk == 0x56 {
        let paths = ole::clipboard_get_files();
        let hwnd = state()
            .lock()
            .unwrap()
            .windows
            .get(&fence_id)
            .copied()
            .unwrap_or(HWND(0));
        if shell::copy_files_to_desktop(hwnd, &paths) {
            ole::clipboard_clear();
            rescan();
        }
    }
    if !delete_paths.is_empty() {
        let hwnd = state()
            .lock()
            .unwrap()
            .windows
            .get(&fence_id)
            .copied()
            .unwrap_or(HWND(0));
        shell::delete_to_recycle_bin_many(hwnd, &delete_paths);
        rescan();
    }
    for path in open_paths.into_iter().take(32) {
        open_item(&path);
    }
    if redraw {
        refresh_fence(fence_id);
    }
}

static LL_HOOK: OnceLock<HHOOK> = OnceLock::new();

fn fence_id_for_hwnd(hwnd: HWND) -> Option<u32> {
    let s = state().try_lock().ok()?;
    s.windows.iter().find(|(_, h)| **h == hwnd).map(|(k, _)| *k)
}

unsafe extern "system" fn ll_keyboard_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
                    if under.0 != 0 {
                        if let Some(id) = fence_id_for_hwnd(under) {
                            if let Some(&th) = TRAY_HWND.get() {
                                let has_selection = state()
                                    .try_lock()
                                    .map(|s| !s.selected_paths.is_empty())
                                    .unwrap_or(false);
                                // 仅无选择时保留旧的方向键移动栅栏行为；选中图标后方向键导航。
                                let _ = PostMessageW(
                                    th,
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
    unsafe {
        if LL_HOOK.get().is_none() {
            if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hinstance(), 0)
            {
                if h.0 != 0 {
                    let _ = LL_HOOK.set(h);
                }
            }
        }
    }
}

fn uninstall_keyboard_hook() {
    if let Some(h) = LL_HOOK.get() {
        unsafe {
            let _ = UnhookWindowsHookEx(*h);
        }
    }
}

// ---------------- 全局鼠标钩子(桌面空白点击清除选择态) ----------------

static LL_MOUSE_HOOK: OnceLock<HHOOK> = OnceLock::new();

/// 栅栏窗口是 WS_EX_NOACTIVATE 的独立 HWND,点击桌面空白时事件直接进入
/// Explorer 的 WorkerW/Progman,本程序收不到任何消息,于是被选中的图标
/// 高亮会一直残留。用 WH_MOUSE_LL 监听左键按下:落点不在任何栅栏内且命中
/// 桌面宿主窗口时,通知托盘窗口清除选择(与原生 Explorer 行为一致)。
unsafe extern "system" fn ll_mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if ncode as u32 == HC_ACTION && wparam.0 as u32 == WM_LBUTTONDOWN {
            crate::ui::mark_interaction();
            let mm = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if let Some(&tray) = TRAY_HWND.get() {
                let _ = PostMessageW(
                    tray,
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
    unsafe {
        if LL_MOUSE_HOOK.get().is_none() {
            if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), hinstance(), 0) {
                if h.0 != 0 {
                    let _ = LL_MOUSE_HOOK.set(h);
                }
            }
        }
    }
}

fn uninstall_mouse_hook() {
    if let Some(h) = LL_MOUSE_HOOK.get() {
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
    unsafe {
        let pt = POINT { x, y };
        let mut hwnd = WindowFromPoint(pt);
        if hwnd.0 == 0 {
            return false;
        }
        // 命中的可能是桌面的 SysListView32 子窗口,取根窗口再判类名
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.0 != 0 {
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

fn track(menu: HMENU, hwnd: HWND, x: i32, y: i32) -> u32 {
    unsafe {
        mark_interaction();
        // 菜单模态循环期间不会可靠投递 WM_MOUSELEAVE。清状态之外必须
        // 立即熄灭悬停卡片:否则卡片(整面淡色背景)残留到菜单关闭后,
        // 才被模态循环延迟的 MOUSELEAVE 熄灭——那次大面积重绘表现为
        // "点桌面关闭菜单时栅栏闪一下"。打开菜单瞬间熄灭=点击反馈,
        // 与原生一致;菜单关闭后 leave 到达时 was=false,零重绘。
        let mut lit: Vec<u32> = Vec::new();
        if let Ok(mut s) = state().try_lock() {
            for v in s.hover.values_mut() {
                *v = None;
            }
            s.hover_hit.clear();
            s.hover_pending.clear();
            s.fence_hover_pending.clear();
            for (id, was) in s.fence_hover.iter_mut() {
                if *was {
                    *was = false;
                    lit.push(*id);
                }
            }
        }
        for id in lit {
            refresh_fence(id);
        }
        // 默认左键选择 + 返回命令 id。
        // TPM_RIGHTBUTTON 与 Explorer 一致：右键菜单允许左键或右键选择菜单项
        // （原生桌面右键菜单支持右键点选项目）。
        // 菜单模态循环期间持有前台权,防止立即退出变僵尸菜单。
        // owner 用隐形菜单宿主而不是栅栏窗口:前台化栅栏会把它提到前台
        // z-band,自愈定时器拉回桌面层时分层窗口跨 band 移动引发整体
        // 重合成闪屏(表面内容并没有变)。
        let _guard = shell::menu_foreground(menu_host_or(hwnd));
        // 前台权诊断:TrackPopupMenu 无前台会立即返回 0(zombie 菜单)。正常应
        // 打印 host match=true;出现其他类名即可定位是谁抢的前台。
        {
            let fg = unsafe { GetForegroundWindow() };
            let mut fb = [0u16; 32];
            let fn_ = unsafe { GetClassNameW(fg, &mut fb) };
            log(&format!(
                "menu open: foreground={} (host match={})",
                String::from_utf16_lossy(&fb[..fn_.max(0) as usize]),
                menu_host_or(hwnd) == fg
            ));
        }
        let r = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, 0, menu_host_or(hwnd), None);
        if r.0 == 0 {
            log("track: menu dismissed without selection");
        }
        // 说明:菜单遮挡期间 DWM 会丢弃分层窗口被遮区域的颜色转换缓存,
        // 菜单移走后的重转换存在 ~4% 舍入差——实测低于人眼感知阈值
        // (并排对比不可辨),且 ULW 重呈现也无法合并它,故不做任何处理。
        if r.0 != 0 {
            r.0 as u32
        } else {
            0
        }
    }
}

fn fence_menu(hwnd: HWND, fence_id: u32, x: i32, y: i32) {
    let (locked, collapsed, sort_mode) = {
        let s = state().lock().unwrap();
        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
            return;
        };
        (fence.locked, fence.collapsed, fence.sort_mode.clone())
    };
    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    let sort = unsafe { CreatePopupMenu().unwrap_or_default() };
    let pairs = [
        (MENU_SORT_FREQ, "常用", "常用(默认)"),
        (MENU_SORT_TIME, "时间", "时间(最近修改)"),
        (MENU_SORT_NAME, "名称", "名称"),
        (MENU_SORT_MANUAL, "手动", "手动(拖拽自定义)"),
    ];
    for (id, key, label) in pairs {
        if sort_mode == key {
            shell::append_menu_checked(sort, id, label);
        } else {
            shell::append_menu(sort, id, label);
        }
    }
    shell::append_menu(menu, MENU_ADD_FENCE, "新建栅栏");
    shell::append_menu(menu, MENU_RENAME, "重命名");
    shell::append_menu(
        menu,
        MENU_TOGGLE_COLLAPSE,
        if collapsed { "展开" } else { "折叠" },
    );
    shell::append_menu(
        menu,
        MENU_LOCK,
        if locked {
            "解除锁定位置与大小"
        } else {
            "锁定位置与大小"
        },
    );
    shell::append_submenu(menu, "排序方式", sort);
    shell::append_separator(menu);
    shell::append_menu(menu, MENU_DELETE_FENCE, "删除栅栏");
    shell::append_separator(menu);
    shell::append_menu(menu, MENU_REFRESH, "刷新");
    let id = track(menu, hwnd, x, y);
    unsafe {
        let _ = DestroyMenu(sort);
        let _ = DestroyMenu(menu);
    }
    match id {
        MENU_SORT_FREQ => set_fence_sort(fence_id, "常用"),
        MENU_SORT_TIME => set_fence_sort(fence_id, "时间"),
        MENU_SORT_NAME => set_fence_sort(fence_id, "名称"),
        MENU_SORT_MANUAL => set_fence_sort(fence_id, "手动"),
        MENU_ADD_FENCE => {
            let _ = add_fence_after(fence_id);
        }
        MENU_RENAME => start_rename(fence_id),
        MENU_TOGGLE_COLLAPSE => toggle_collapse(fence_id),
        MENU_LOCK => toggle_lock(fence_id),
        MENU_DELETE_FENCE => delete_fence(fence_id),
        MENU_REFRESH => rescan(),
        _ => {}
    }
}

pub fn add_fence_after(base_id: u32) -> u32 {
    push_undo();
    let max_id = {
        let mut s = state().lock().unwrap();
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        let (x, y) = match s.fences.iter().find(|f| f.id == base_id) {
            Some(b) => (b.rect.x + 40.0, b.rect.y + 40.0),
            None => (200.0, 200.0),
        };
        s.fences.push(Fence {
            id: max_id,
            title: "新栅栏".into(),
            category: String::new(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: {
                let (dw, dh) = default_fence_size();
                Rect { x, y, w: dw, h: dh }
            },
            collapsed: false,
            scroll_rows: 0,
            locked: false,
            hidden: false,
            manual_size: false,
            sort_mode: model::default_sort_mode(),
        });
        max_id
    };
    settle_all_fences();
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    ensure_fence_window(max_id);
    refresh_all_fences();
    max_id
}

fn toggle_collapse(fence_id: u32) {
    let collapsed = {
        let mut s = state().lock().unwrap();
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
            f.collapsed = !f.collapsed;
        }
        let collapsed = s
            .fences
            .iter()
            .find(|f| f.id == fence_id)
            .is_some_and(|f| f.collapsed);
        if collapsed {
            clear_fence_interaction(&mut s, fence_id);
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
        collapsed
    };
    if collapsed {
        finish_interaction_cleanup();
    }
    refresh_fence(fence_id);
}

/// 切换栏内排序模式并持久化;"手动"模式沿用用户拖拽出的 item_order
fn set_fence_sort(fence_id: u32, mode: &str) {
    {
        let mut s = state().lock().unwrap();
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
            f.sort_mode = mode.to_string();
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    log(&format!("fence {} sort mode = {}", fence_id, mode));
    refresh_fence(fence_id);
}

fn toggle_lock(fence_id: u32) {
    let mut s = state().lock().unwrap();
    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
        f.locked = !f.locked;
    }
    let cfg = s.fences.clone();
    let _ = model::save_config(&cfg);
}

fn delete_fence(fence_id: u32) {
    if !state()
        .lock()
        .unwrap()
        .fences
        .iter()
        .any(|f| f.id == fence_id)
    {
        return;
    }
    push_undo();
    let hwnd = {
        let mut s = state().lock().unwrap();
        clear_fence_interaction(&mut s, fence_id);
        let removed = s.windows.remove(&fence_id);
        s.metrics.remove(&fence_id);
        s.presented.remove(&fence_id);
        s.attached.remove(&fence_id);
        if let Some(sf) = s.surfaces.remove(&fence_id) {
            render::release_surface(sf);
        }
        s.fences.retain(|f| f.id != fence_id);
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
        removed
    };
    if let Some(h) = hwnd {
        unsafe {
            let _ = RevokeDragDrop(h);
            let _ = DestroyWindow(h);
        }
    }
    finish_interaction_cleanup();
    reconcile_desktop_icons();
}

/// 离开 DeskFence 桌面模式：先恢复 Explorer 原生图标，再隐藏本程序窗口。
/// 不修改原始图标位置、文件或 Explorer 布局；用户可通过“显示全部栅栏”再次接管。
fn restore_original_desktop() {
    ZEN_MODE.store(false, Ordering::Relaxed);
    set_desktop_state_stored("native");
    let _ = restore_desktop_now();
    set_all_hidden(true);
    log("returned to original desktop without changing files or icon layout");
}

fn set_all_hidden(hidden: bool) {
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

// ---------------- 显示桌面态 topmost 免疫(2026-08-29 终修,勿回退) ----------------
// 机制:ToggleDesktop/三指把栅栏纳入"停泊批"(静默沉底,无法否决),此后
// 每次菜单关闭系统都把批内成员重新停泊=栅栏被拖下再拉回=菜单后点空白
// 闪屏(60ms zwatch 实测:沉底块=栅栏簇+菜单宿主,parked 窗不被波及)。
// 逐个最小化回桌面的路径不碰停泊批→栅栏不动→不闪(用户 Case B 实测)。
// topmost 窗口不参与停泊(SPW ScW 钩子层与隐形垃圾丛林在每次切换中
// 纹丝不动)→显示桌面态(无任何可见非 topmost 外来窗=应用全部停泊/
// 最小化)给栅栏上 HWND_TOPMOST 获得同款豁免;出现可见应用窗(回应用)
// 立即 HWND_NOTOPMOST,由既有走查/下压机制送回最低应用窗之下的深位。
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

/// 全带是否存在"可见且非 topmost 的外来窗"(=有可见应用窗)。
/// 与 band_attach_anchor 主规则同源判定。
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
            || is_topmost_window(w)
            || band_aux(w, mh, tr)
            || band_invisible(w, &vs)
        {
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
            match desktop_shell_window().and_then(|host| band_attach_anchor(host, h, false)) {
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

// ---------------- 重命名 ----------------

static RENAME_OLD_PROC: std::sync::OnceLock<isize> = std::sync::OnceLock::new();
static INTENTIONAL_HIDE: AtomicBool = AtomicBool::new(false);

// ---------------- z 序意图守卫 ----------------

/// 窗口定位意图:标记"自家发起的 z 序/显示操作",让 fence_wndproc 的
/// WM_WINDOWPOSCHANGING 拦截只针对外部改动。区分依据:自家 SetWindowPos/
/// ShowWindow 在 UI 线程同步触发该消息(嵌套在调用栈内);外部进程(Shell
/// 显示桌面/最小化批次)的调用经消息泵派发,到达时意图必为 None——线程
/// 局部即可精确区分,无需跨进程握手(后台线程只做文件 IO,不碰窗口)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ZIntent {
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
struct ZScope(Option<ZIntent>);

fn z_scope(intent: ZIntent) -> ZScope {
    let prev = Z_INTENT.with(|c| c.replace(Some(intent)));
    ZScope(prev)
}

impl Drop for ZScope {
    fn drop(&mut self) {
        Z_INTENT.with(|c| c.set(self.0));
    }
}

fn z_intent_active() -> bool {
    Z_INTENT.with(|c| c.get().is_some())
}

/// 外部定位变更后的自检:若窗口被压到桌面宿主之下(显示桌面批次的实际
/// 行为,且该操作不经可否决的 WM_WINDOWPOSCHANGING——2026-08-28 wdprobe
/// 实测 veto 零命中、栅栏在宿主下方 vis=1),立即重挂回宿主正上方,不等
/// 3 拍自愈。判据:从本窗口向上(GW_HWNDPREV)走能遇到宿主=自己在宿主
/// 之下;正常在带内时向上走只会到栈顶。无状态锁,可在窗口过程直接调用。
fn fence_reanchor_if_below_host(hwnd: HWND) {
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
            let Some(after) = band_attach_anchor(shell, hwnd, false) else { return };
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
static ZCHECK_PENDING: AtomicBool = AtomicBool::new(false);
static ZORDER_HOOKS: std::sync::OnceLock<(HWINEVENTHOOK, HWINEVENTHOOK)> =
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
fn install_zorder_hooks() {
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

fn zcheck_fences_now() {
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
fn fence_lower_if_blocked(hwnd: HWND, menu_host: &Option<HWND>, tray: &Option<HWND>) -> bool {
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
const TIMER_RENAME_WATCH: usize = 4;

fn point_in_window_rect(hwnd: HWND, x: i32, y: i32) -> bool {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// 编辑框外的鼠标按下 → 提交。fence_title=true 提交栅栏标题编辑框。
fn rename_click_outside_hit(x: i32, y: i32, _src: &str) -> bool {
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
fn start_rename(fence_id: u32) {
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

const FILE_RENAME_COMMIT_MSG: u32 = WM_USER + 3;
const FILE_RENAME_CANCEL_MSG: u32 = WM_USER + 4;
static FILE_RENAME_OLD_PROC: OnceLock<isize> = OnceLock::new();
static FILE_RENAME_PATH: Mutex<Option<String>> = Mutex::new(None);

/// 系统菜单"重命名"拦截回调(shell.rs 在 init 时注册)
fn on_shell_rename_request(path: &str) {
    start_file_rename(path.to_string());
}

fn start_file_rename(path: String) {
    // 回收站虚拟条目不可重命名
    if model::is_recycle_bin(&path) {
        return;
    }
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // 定位该文件所在栅栏的格子,把编辑框盖在名字标签上;找不到就放光标旁。
    // 宽度与 Explorer 一致:随文件名文本自适应增长(不小于一个图标格宽)。
    let (edit_x, edit_y, edit_w) = {
        let s = state().lock().unwrap();
        let text_w = s.renderer.as_ref().and_then(|r| {
            let w16 = shell::wide(&name);
            unsafe {
                r.dw.CreateTextLayout(
                    &w16[..w16.len().saturating_sub(1)],
                    &r.name_fmt,
                    4096.0,
                    64.0,
                )
                .ok()
                .map(|lay| {
                    let mut m = Default::default();
                    let _ = lay.GetMetrics(&mut m);
                    m.width
                })
            }
        });
        let text_w = text_w.unwrap_or(0.0);
        let mut pos = None;
        'outer: for f in &s.fences {
            let items = model::display_list(f, &s.files);
            for (i, it) in items.iter().enumerate() {
                if it.path == path {
                    let lay = model::layout(f, items.len());
                    let (cx, cy) = model::cell_pos(&lay, i);
                    let cs = model::icon_size();
                    let cw = model::cell_w();
                    pos = Some((
                        (f.rect.x + cx - 6.0).round() as i32,
                        (f.rect.y + cy + cs + 4.0).round() as i32,
                        (cw + 12.0).max(text_w + 24.0).round() as i32,
                    ));
                    break 'outer;
                }
            }
        }
        pos.unwrap_or_else(|| {
            let (sx, sy) = screen_cursor();
            (sx as i32 - 80, sy as i32 - 12, 180)
        })
    };
    unsafe {
        let edit_cls = shell::wide("EDIT");
        let edit = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR::from_raw(edit_cls.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_POPUP.0 | WS_BORDER.0 | WS_VISIBLE.0 | (ES_CENTER as u32)),
            edit_x,
            edit_y,
            edit_w,
            24,
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        );
        if edit.0 == 0 {
            return;
        }
        // desktop_icon_font 已返回按系统 DPI 换算后的像素高度，这里只应用一次。
        let (family, px, weight) = shell::desktop_icon_font();
        let mut lf: LOGFONTW = std::mem::zeroed();
        lf.lfHeight = -(px.round() as i32);
        lf.lfWeight = weight;
        for (i, c) in family.encode_utf16().take(31).enumerate() {
            lf.lfFaceName[i] = c;
        }
        let font = CreateFontIndirectW(&lf);
        if !font.is_invalid() {
            let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        let w = shell::wide(&name);
        let _ = SetWindowTextW(edit, PCWSTR::from_raw(w.as_ptr()));
        // 与 Explorer 一致:预选扩展名之前的部分
        let sel_end = name
            .rfind('.')
            .filter(|&p| p > 0)
            .map(|p| p as isize)
            .unwrap_or(-1);
        let _ = SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(sel_end));
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

fn commit_file_rename(edit: HWND) {
    log("file rename commit");
    let old_path = FILE_RENAME_PATH.lock().unwrap().clone().unwrap_or_default();
    if !old_path.is_empty() {
        let mut buf = [0u16; 512];
        unsafe {
            let _ = GetWindowTextW(edit, &mut buf);
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let new_name = String::from_utf16_lossy(&buf[..end]).trim().to_string();
        let old_name = std::path::Path::new(&old_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // 与 Explorer 相同的非法字符集合;空名/原名不动
        let invalid = new_name.is_empty()
            || new_name == old_name
            || new_name
                .chars()
                .any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
        let mut renamed = false;
        if !invalid {
            renamed = shell::rename_path(&old_path, &new_name);
        }
        if renamed {
            // 同步被拖入栅栏的 pinned 引用,指向新路径
            let new_path = std::path::Path::new(&old_path)
                .parent()
                .map(|p| p.join(&new_name).to_string_lossy().to_string())
                .unwrap_or_else(|| old_path.clone());
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
                s.selection_anchor = Some(new_path);
            }
        } else if !invalid {
            log(&format!("file rename failed: {} -> {}", old_path, new_name));
            let title = shell::wide("重命名失败");
            let text = shell::wide("无法重命名该项目。目标名称可能已存在，或者文件正在使用中。");
            unsafe {
                let _ = MessageBoxW(
                    edit,
                    PCWSTR::from_raw(text.as_ptr()),
                    PCWSTR::from_raw(title.as_ptr()),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }
    *FILE_RENAME_PATH.lock().unwrap() = None;
    {
        let mut s = state().lock().unwrap();
        s.file_rename_edit = None;
    }
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = DestroyWindow(edit);
    }
    rescan();
}

fn cancel_file_rename(edit: HWND) {
    log("file rename cancel");
    *FILE_RENAME_PATH.lock().unwrap() = None;
    {
        let mut s = state().lock().unwrap();
        s.file_rename_edit = None;
    }
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = DestroyWindow(edit);
    }
}

// ---------------- WndProc ----------------

unsafe extern "system" fn fence_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let fence_id = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32;
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
            unsafe {
                let _ = KillTimer(hwnd, TIMER_HOVER);
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
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_HOVER);
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
            // 阻止 Win+D / Win+M 对栅栏的摆布:栅栏常驻桌面,不参与窗口管理。
            // 我们自己主动隐藏(隐藏全部栅栏)时 INTENTIONAL_HIDE 为真,放行;
            // 自家定位操作(创建/拖拽/修复/呈现)以 Z_INTENT 标记放行。
            // z 否决只作用于真栅栏(GWLP_USERDATA=fence_id):菜单宿主等辅助窗
            // 的 z 无关紧要,却会被 IME 子系统周期性重排——否决它只会招来
            // 无限重试的对抗循环(2026-08-28 实测 0xf05d6 每 3-5s 一次)。
            if !INTENTIONAL_HIDE.load(Ordering::SeqCst) && !z_intent_active() {
                let wp = &mut *(lparam.0 as *mut WINDOWPOS);
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
                        hwnd.0, wp.hwndInsertAfter.0, wp.flags.0
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
                let wp = &*(lparam.0 as *const WINDOWPOS);
                log(&format!(
                    "z-guard: external pos-changed h=0x{:x} after=0x{:x} flags=0x{:x}",
                    hwnd.0, wp.hwndInsertAfter.0, wp.flags.0
                ));
                fence_reanchor_if_below_host(hwnd);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
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
            unsafe {
                let _ = RevokeDragDrop(hwnd);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ---------------- 输入处理 ----------------

/// 屏幕工作区（不含任务栏）
fn work_area() -> (f32, f32, f32, f32) {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    (
        r.left as f32,
        r.top as f32,
        (r.right - r.left) as f32,
        (r.bottom - r.top) as f32,
    )
}

/// 矩形所在显示器的工作区(多显示器:磁吸/含屏按各自屏幕进行)
fn work_area_for_rect(r: &Rect) -> (f32, f32, f32, f32) {
    unsafe {
        let rc = RECT {
            left: r.x.round() as i32,
            top: r.y.round() as i32,
            right: (r.x + r.w).round() as i32,
            bottom: (r.y + r.h).round() as i32,
        };
        let mon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let w = mi.rcWork;
            return (
                w.left as f32,
                w.top as f32,
                (w.right - w.left) as f32,
                (w.bottom - w.top) as f32,
            );
        }
    }
    work_area()
}

unsafe extern "system" fn enum_monitor_cb(
    mon: HMONITOR,
    _dc: HDC,
    _rc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let areas = &mut *(lparam.0 as *mut Vec<(f32, f32, f32, f32)>);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(mon, &mut mi).as_bool() {
        let w = mi.rcWork;
        areas.push((
            w.left as f32,
            w.top as f32,
            (w.right - w.left) as f32,
            (w.bottom - w.top) as f32,
        ));
    }
    BOOL(1)
}

/// 所有显示器的工作区(settle/含屏用)
fn all_work_areas() -> Vec<(f32, f32, f32, f32)> {
    let mut areas: Vec<(f32, f32, f32, f32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut areas as *mut Vec<(f32, f32, f32, f32)> as isize),
        );
    }
    if areas.is_empty() {
        areas.push(work_area());
    }
    areas
}

/// 拖动/缩放目标吸附：自动对齐开启时，先把目标矩形磁吸到其它栅栏的边
/// 与屏幕边缘（保持 GAP），再吸附到网格；关闭时原样返回（纯自由拖动）。
/// 返回 (吸附后矩形, 竖参考线坐标, 横参考线坐标)。
fn compact_neighbors_after_resize(fences: &mut [Fence], anchor_id: u32) {
    let Some(anchor) = fences.iter().find(|f| f.id == anchor_id).map(|f| f.rect) else {
        return;
    };
    let (vx, vy, vw, vh) = work_area_for_rect(&anchor);
    let max_x = (vx + vw).max(0.0);
    let max_y = (vy + vh).max(0.0);
    let mut right: Vec<usize> = fences
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.id != anchor_id
                && f.rect.x >= anchor.x
                && f.rect.y < anchor.y + anchor.h
                && f.rect.y + f.rect.h > anchor.y
        })
        .map(|(i, _)| i)
        .collect();
    right.sort_by(|a, b| {
        fences[*a]
            .rect
            .x
            .partial_cmp(&fences[*b].rect.x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut cursor = (anchor.x + anchor.w + model::GAP).min(max_x - model::min_w());
    for i in right {
        fences[i].rect.x = cursor.min(max_x - fences[i].rect.w);
        cursor = fences[i].rect.x + fences[i].rect.w + model::GAP;
    }
    let mut below: Vec<usize> = fences
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.id != anchor_id
                && f.rect.y >= anchor.y
                && f.rect.x < anchor.x + anchor.w
                && f.rect.x + f.rect.w > anchor.x
        })
        .map(|(i, _)| i)
        .collect();
    below.sort_by(|a, b| {
        fences[*a]
            .rect
            .y
            .partial_cmp(&fences[*b].rect.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut cursor = (anchor.y + anchor.h + model::GAP).min(max_y - model::min_h());
    for i in below {
        fences[i].rect.y = cursor.min(max_y - fences[i].rect.h);
        cursor = fences[i].rect.y + fences[i].rect.h + model::GAP;
    }
    // Clamp all displaced fences back into the work area.
    for f in fences.iter_mut() {
        if f.id == anchor_id {
            continue;
        }
        let mut tmp = [f.rect];
        model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
        f.rect = tmp[0];
    }
}

/// 从按下快照推演「锚点跟随鼠标」后的全体栅栏布局(纯函数:快照+光标 → 布局)。
/// 每帧都从快照重算,拖回即还原,可逆性不依赖算法性质;
/// 拖动预览与松手提交共用同一管线,保证所见即所得(松手不再二次跳变)。
/// 栅栏插入计划(2026-09-02 第2-6/11项:行内槽位模型)。行聚类把被拖者也
/// 计入,插入点=(目标行,行内位置)二维定位——横向中心越过邻居中心换槽,
/// 纵向跨过行间中线换层,任意方向/任意层数/任意宽度组合都成立;指示线恒
/// 为目标行整行高竖线。落位分配走 model::row_insert_layout(有单测):
/// 尺寸保持各自,只动受影响两行,其余成员取到自己原位(不乱桌)。
fn fence_insertion_plan(drag: &Drag, cx: f32, cy: f32) -> Option<InsertPlan> {
    let all: Vec<(u32, Rect)> = drag
        .start_layout
        .iter()
        .filter(|f| !f.hidden && !f.collapsed)
        .map(|f| (f.id, f.rect))
        .collect();
    if all.len() < 2 {
        return None;
    }
    let a_idx = all.iter().position(|(id, _)| *id == drag.fence_id)?;
    let rects: Vec<Rect> = all.iter().map(|(_, r)| *r).collect();
    let rows = model::rows_from_rects(&rects);
    // 被拖者原位(行,槽)
    let (hr, hj) = rows
        .iter()
        .enumerate()
        .find_map(|(ri, row)| row.iter().position(|&i| i == a_idx).map(|j| (ri, j)))?;
    // 被拖栅栏实时中心(随光标移动):插入判定用它而非光标本身
    let lx = drag.start_rect.x + drag.start_rect.w * 0.5 + (cx - drag.start_sx);
    let ly = drag.start_rect.y + drag.start_rect.h * 0.5 + (cy - drag.start_sy);
    // 自由放置区:实时中心距最近行中心超过半高容差(至少 48)→ 无槽位
    let half = rows
        .iter()
        .flat_map(|row| row.iter())
        .map(|&i| rects[i].h * 0.5)
        .fold(0f32, f32::max)
        .max(48.0);
    if model::nearest_row_distance(&rects, &rows, ly) > half {
        return None;
    }
    let (tr, tk) = model::row_slot_of(&rects, &rows, (lx, ly));
    // 原位抑制:(行,槽)全都没变 = 刚拿起/原地
    if (tr, tk) == (hr, hj) {
        return None;
    }
    let (assign_pos, land) = model::row_insert_layout(&rects, a_idx, (tr, tk));
    let assign = all
        .iter()
        .enumerate()
        .map(|(t, (id, _))| (*id, assign_pos[t]))
        .collect();
    // 指示线=目标行整行高竖线:tk<行长度画在第 tk 成员左缘,否则行尾右缘
    let row_t = &rows[tr];
    let (top, bottom) = row_t.iter().fold((f32::MAX, f32::MIN), |(t, b), &i| {
        (t.min(rects[i].y), b.max(rects[i].y + rects[i].h))
    });
    let line = if tk < row_t.len() {
        let m = rects[row_t[tk]];
        (m.x - model::GAP * 0.5 - 1.25, top, 2.5, bottom - top)
    } else {
        let m = rects[row_t[row_t.len() - 1]];
        (m.x + m.w + model::GAP * 0.5 - 1.25, top, 2.5, bottom - top)
    };
    Some(InsertPlan {
        assign,
        land,
        line,
    })
}

/// 邻居等距吸附(上下左右对称):左右贴齐/紧邻保持 GAP,上下同理。
/// 只在对应方向有重叠时生效,取距离最近的候选一次应用。
fn snap_rect_to_neighbors(nr: &mut Rect, others: &[Rect]) {
    const SNAP: f32 = 16.0;
    let mut best: Option<(f32, f32, f32)> = None; // (总距离, dx, dy)
    for r in others {
        let x_ov = nr.x < r.x + r.w && nr.x + nr.w > r.x;
        let y_ov = nr.y < r.y + r.h && nr.y + nr.h > r.y;
        let mut cands: Vec<(f32, f32)> = Vec::new();
        if y_ov {
            cands.push(((r.x + r.w + model::GAP) - nr.x, 0.0)); // 紧贴右侧
            cands.push((r.x - nr.x, 0.0)); // 左缘对齐
            cands.push(((r.x - nr.w) - nr.x, 0.0)); // 紧贴左侧
        }
        if x_ov {
            cands.push((0.0, (r.y + r.h + model::GAP) - nr.y)); // 紧贴下方
            cands.push((0.0, r.y - nr.y)); // 顶边对齐
            cands.push((0.0, (r.y - nr.h) - nr.y)); // 紧贴上方
        }
        for (dx, dy) in cands {
            let d = dx.abs() + dy.abs();
            if d < SNAP && best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, dx, dy));
            }
        }
    }
    if let Some((_, dx, dy)) = best {
        nr.x += dx;
        nr.y += dy;
    }
}

fn snap_drag(s: &UiState, fence_id: u32, nr: Rect) -> (Rect, Option<f32>, Option<f32>) {
    // 三档对齐的拖动吸附:
    // 网格档 = 图标格整数倍 + 靠近邻居磁吸到固定间距;
    // 自由档 = 完全跟手,仅靠近邻居时磁吸到固定间距;
    // 自动档 = 不吸附(链式对齐实时保证间距)。
    if auto_align_on() {
        return (nr, None, None);
    }
    let others: Vec<Rect> = s
        .fences
        .iter()
        .filter(|f| f.id != fence_id)
        .map(|f| f.rect)
        .collect();
    let mut x = nr.x;
    let mut y = nr.y;
    if grid_align_on() {
        let (vx, vy, _, _) = work_area_for_rect(&nr);
        x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
        y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
    }
    // 靠近邻居 -> 磁吸到恰好 GAP 间距(优先于网格格点)
    let probe = Rect { x, y, ..nr };
    let (sx, snapped) = model::snap_gap_to_neighbors(&probe, &others, model::SNAP_THRESHOLD * 1.5);
    if snapped {
        x = sx;
    }
    (Rect { x, y, ..nr }, None, None)
}

/// 虚拟桌面(所有显示器的包围盒,屏幕坐标)
fn virtual_screen() -> (f32, f32, f32, f32) {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN) as f32;
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN) as f32;
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN) as f32;
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN) as f32;
        if w > 0.0 && h > 0.0 {
            (x, y, w, h)
        } else {
            work_area()
        }
    }
}

/// 确保全屏对齐参考线 overlay 窗口存在（懒创建）。
fn ensure_guide_window(s: &mut UiState) {
    if s.guide_hwnd
        .is_some_and(|hwnd| unsafe { IsWindow(hwnd).as_bool() })
    {
        return;
    }
    s.guide_hwnd = None;
    let (vx, vy, vw, vh) = virtual_screen();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            guide_class_name(),
            PCWSTR::null(),
            WS_POPUP,
            vx as i32,
            vy as i32,
            vw as i32,
            vh as i32,
            HWND(0),
            HMENU(0),
            hinstance(),
            None,
        )
    };
    if hwnd.0 != 0 {
        s.guide_hwnd = Some(hwnd);
    }
}

/// 绘制并显示对齐参考线 overlay（s.guide_x / s.guide_y 为当前参考线）。
fn refresh_guide(s: &mut UiState) {
    let Some(hwnd) = s.guide_hwnd else { return };
    let factory = match &s.renderer {
        Some(r) => r.factory.clone(),
        None => return,
    };
    let (vx, vy, vw, vh) = virtual_screen();
    let w = vw as u32;
    let h = vh as u32;
    if w < 2 || h < 2 {
        return;
    }
    let needs_new = match &s.guide_surface {
        Some(sf) => sf.w != w || sf.h != h,
        None => true,
    };
    if needs_new {
        if let Some(old) = s.guide_surface.take() {
            render::release_surface(old);
        }
        match render::create_surface(&factory, w, h) {
            Some(sf) => s.guide_surface = Some(sf),
            None => return,
        }
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            vx as i32,
            vy as i32,
            vw as i32,
            vh as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    // 先取出残影数据(需要 &mut icon_cache 取图标,不能与 surface 借用并存)
    // 截断/换行与静态 draw_item 完全同源:同一 trim_to_lines、同一标签宽
    // (cell_w-2*scale)、同一 2 行上限——两行的名字拖动中依旧两行,一行依旧一行。
    let trim_ctx = s
        .renderer
        .as_ref()
        .map(|r| (r.dw.clone(), r.name_fmt.clone()));
    let guide_metrics = s
        .active_fence
        .and_then(|id| s.metrics.get(&id))
        .cloned()
        .unwrap_or_else(model::DpiMetrics::system);
    let ghost = {
        let g = s.drag_ghost.clone();
        g.and_then(|(paths, gx, gy)| {
            let path = paths.first()?.clone();
            let name = s
                .files
                .iter()
                .find(|f| f.path == path)
                .map(|f| render::display_name(&f.name))
                .unwrap_or_default();
            let name = match &trim_ctx {
                Some((dw, fmt)) => render::trim_to_lines(
                    dw,
                    &name,
                    fmt,
                    guide_metrics.cell_w - 2.0 * guide_metrics.scale,
                    (2.0 * 24.0 + 6.0) * guide_metrics.scale,
                    2,
                ),
                None => name,
            };
            let icon = render::get_icon_buffer(&mut s.icon_cache, &path, guide_metrics.icon_px);
            Some((icon, name, gx - vx, gy - vy))
        })
    };
    let now = resize_now_ms();
    s.arrival_animations.retain(|animation| {
        now.saturating_sub(animation.started_ms) <= animation.duration_ms + 180
    });
    let animation_meta = s.arrival_animations.clone();
    let mut animation_icons = Vec::with_capacity(animation_meta.len());
    for animation in &animation_meta {
        let target_px = s
            .metrics
            .get(&animation.fence_id)
            .map(|m| m.icon_px)
            .unwrap_or_else(model::icon_size);
        animation_icons.push(render::get_icon_buffer(
            &mut s.icon_cache,
            &animation.path,
            target_px,
        ));
    }
    let mut frames = Vec::with_capacity(animation_meta.len());
    for (animation, icon) in animation_meta.iter().zip(animation_icons.into_iter()) {
        let elapsed = now.saturating_sub(animation.started_ms);
        let progress = (elapsed as f32 / animation.duration_ms.max(1) as f32).clamp(0.0, 1.0);
        let point = model::interpolate_point(animation.from, animation.to, progress);
        let mut trail = Vec::new();
        // 拖尾 7 帧渐隐(比 4 帧更长,轨迹"尾流"更明显)
        for step in 1..=7 {
            let previous = (progress - step as f32 * 0.055).max(0.0);
            let p = model::interpolate_point(animation.from, animation.to, previous);
            trail.push((p.0 - vx, p.1 - vy, 0.22 / step as f32));
        }
        let name = match (&trim_ctx, s.metrics.get(&animation.fence_id)) {
            (Some((dw, fmt)), Some(m)) => render::trim_to_lines(
                dw,
                &animation.name,
                fmt,
                m.cell_w - 2.0 * m.scale,
                (2.0 * 24.0 + 6.0) * m.scale,
                2,
            ),
            _ => animation.name.clone(),
        };
        frames.push(render::ArrivalFrame {
            icon,
            name,
            x: point.0 - vx,
            y: point.1 - vy,
            target_x: animation.to.0 - vx,
            target_y: animation.to.1 - vy,
            trail,
        });
    }
    let insert_line_local = s.insert_line.map(|(x, y, w, h)| (x - vx, y - vy, w, h));
    if let Some(surf) = s.guide_surface.as_ref() {
        // 参考线是屏幕坐标,换算到虚拟桌面原点
        // 残影(内部图标拖拽):同样换算到 overlay 本地坐标
        let label_scale = frames
            .first()
            .and_then(|_| animation_meta.first())
            .and_then(|a| s.metrics.get(&a.fence_id))
            .or_else(|| s.active_fence.and_then(|id| s.metrics.get(&id)))
            .map(|m| m.scale)
            .unwrap_or_else(|| model::DpiMetrics::system().scale);
        let jobs = render::draw_guides(
            &surf.target,
            vw,
            vh,
            frames
                .first()
                .and_then(|_| animation_meta.first())
                .and_then(|a| s.metrics.get(&a.fence_id))
                .map(|m| m.icon_px)
                .or_else(|| {
                    s.active_fence
                        .and_then(|id| s.metrics.get(&id))
                        .map(|m| m.icon_px)
                })
                .unwrap_or_else(model::icon_size),
            label_scale,
            guide_metrics.cell_w - 2.0 * guide_metrics.scale,
            s.guide_x.map(|g| g - vx),
            s.guide_y.map(|g| g - vy),
            insert_line_local,
            ghost
                .as_ref()
                .map(|(a, b, x, y)| (a.as_slice(), b.as_str(), *x, *y)),
            &frames,
        );
        // 残影/入场图标名走与静态一致的 GDI ClearType(几何=图标底+2逻辑px)
        render::gdi_draw_labels_transparent(surf, &jobs);
        let pos = POINT {
            x: vx as i32,
            y: vy as i32,
        };
        let _ = render::present_surface(surf, hwnd, pos.x, pos.y);
    }
}

/// 更新参考线状态并刷新 overlay；拖动结束后传入 (None, None) 隐藏。
/// 内部图标拖拽残影存在时 overlay 保持显示。
fn update_guides(gx: Option<f32>, gy: Option<f32>) {
    let mut s = state().lock().unwrap();
    s.guide_x = gx;
    s.guide_y = gy;
    if gx.is_none()
        && gy.is_none()
        && s.drag_ghost.is_none()
        && s.arrival_animations.is_empty()
        && s.insert_line.is_none()
    {
        if let Some(h) = s.guide_hwnd {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        return;
    }
    ensure_guide_window(&mut s);
    refresh_guide(&mut s);
}

/// 更新内部图标拖拽残影位置(屏幕坐标)并重绘 overlay
fn update_ghost(x: f32, y: f32) {
    let mut s = state().lock().unwrap();
    match s.drag_ghost.as_mut() {
        Some((_, gx, gy)) => {
            *gx = x;
            *gy = y;
        }
        None => return,
    }
    ensure_guide_window(&mut s);
    refresh_guide(&mut s);
}

/// 让所有栅栏相互保持间距且全部落在屏幕内（新建/加载/恢复布局后调用）。
/// 统一为链式推挤 + 夹回屏幕(多显示器感知),保留栅栏的相对位置/顺序(自由组合模型)。

/// 高度自适应内容：未手动缩放过的栅栏，高度收敛到内容所需行数
/// （空栅栏至少 2 行，保证拖放目标可见），上限为所在工作区可容纳的最大
/// 整行数（超出保持滚动）。只调高度，位置由随后的 settle 夹回并解重叠。
fn refit_auto_fence_heights() {
    let areas = all_work_areas();
    let mut changed = false;
    {
        let mut s = state().lock().unwrap();
        // 尺寸策略:默认 2列×5行,高度不随内容自适应(超出滚动);
        // 这里只算"屏幕可容纳的最大高度"用于把超屏栅栏夹回。
        let targets: Vec<(u32, f32)> = s
            .fences
            .iter()
            .filter(|f| !f.collapsed && !f.hidden)
            .map(|f| {
                let metrics = s
                    .metrics
                    .get(&f.id)
                    .copied()
                    .unwrap_or_else(model::DpiMetrics::system);
                let (_, _, _, vh) = work_area_for_rect(&f.rect);
                let max_rows = (((vh - metrics.title_h - metrics.pad * 2.0) / metrics.cell_h)
                    .floor() as usize)
                    .max(1);
                let target_h =
                    metrics.title_h + max_rows as f32 * metrics.cell_h + metrics.pad * 2.0 + 2.0;
                (f.id, target_h)
            })
            .collect();
        for (id, target_h) in targets {
            if let Some(f) = s.fences.iter_mut().find(|f| f.id == id) {
                // 只把超出屏幕的夹回,不放大、不随内容变化(默认 5 行,用户可调)
                if f.rect.h > target_h + 0.5 {
                    f.rect.h = target_h;
                    changed = true;
                }
            }
        }
        if changed {
            let mut rects: Vec<Rect> = s.fences.iter().map(|f| f.rect).collect();
            model::fit_to_monitors(&mut rects, &areas);
            for (f, r) in s.fences.iter_mut().zip(rects) {
                f.rect = r;
            }
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
    }
    if changed {
        refresh_all_fences();
    }
}

fn settle_all_fences() {
    let areas = all_work_areas();
    let mut s = state().lock().unwrap();
    push_settle(&mut s.fences, &areas);
}

/// 仅推挤解除重叠 + 夹回屏幕，不改变栅栏顺序/相对位置。
/// 两两收敛：反复检查每一对栅栏，按最小位移推开重叠。
fn push_settle(fences: &mut [Fence], areas: &[(f32, f32, f32, f32)]) {
    let n = fences.len();
    if n == 0 {
        return;
    }
    // Normalize every fence to the icon-cell grid so fences always fit an
    // integer number of icon columns/rows without wasted padding.
    for f in fences.iter_mut() {
        let (w, h) = model::snap_fence_size(f.rect.w, f.rect.h);
        f.rect.w = w;
        f.rect.h = h;
    }
    let mut rects: Vec<Rect> = fences.iter().map(|f| f.rect).collect();
    for _ in 0..24 {
        let mut moved = false;
        for i in 0..n {
            for j in (i + 1)..n {
                let old = rects[j];
                rects[j] = model::push_away(&rects[i], &old);
                if rects[j] != old {
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    model::fit_to_monitors(&mut rects, areas);
    for (f, r) in fences.iter_mut().zip(rects) {
        f.rect = r;
    }
}

/// 周期性 rescan 用的收敛：只推挤 + 夹回屏幕，保留用户手动摆放的相对位置，
/// 避免自动对齐模式下每 30 秒把所有栅栏流式重排回左上角。
fn settle_preserve_positions() {
    let areas = all_work_areas();
    let mut s = state().lock().unwrap();
    push_settle(&mut s.fences, &areas);
}

/// 追踪鼠标离开(用于隐藏悬停卡片)
fn track_mouse_leave(hwnd: HWND) {
    unsafe {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TRACKMOUSEEVENT_FLAGS(TME_LEAVE.0),
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tme);
    }
}

fn handle_mousemove(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
    let mut s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(drag) = s.drag.clone() {
        if drag.fence_id == fence_id {
            let dx = x - drag.start_x;
            let dy = y - drag.start_y;
            match drag.mode {
                DragMode::Move => {
                    // 屏幕坐标算位移,客户区坐标随窗口移动会来回震荡(抖动/拖不动)
                    let (cx, cy) = screen_cursor();
                    // 时间节流(~120fps):逐像素跟手,同时避免事件风暴下
                    // 同步 GDI/D2D 工作堆积;布局每帧从按下快照重算,天然可逆。
                    let now = resize_now_ms();
                    if now.saturating_sub(s.last_move_ms) < 8 {
                        return;
                    }
                    s.last_move_ms = now;
                    // 插入式拖动(原生/启动器风格):被拖栅栏自由跟手并置顶,
                    // 其余栅栏完全不动;插入点只以指示线提示,松手才拼接重排。
                    let mut nr = Rect {
                        x: drag.start_rect.x + (cx - drag.start_sx),
                        y: drag.start_rect.y + (cy - drag.start_sy),
                        w: drag.start_rect.w,
                        h: drag.start_rect.h,
                    };
                    // 自由/网格档保留原吸附手感;自动/网格档同时启用插入线
                    if !auto_align_on() {
                        let others: Vec<Rect> = drag
                            .start_layout
                            .iter()
                            .filter(|f| f.id != fence_id)
                            .map(|f| f.rect)
                            .collect();
                        let mut x = nr.x;
                        let mut y = nr.y;
                        if grid_align_on() {
                            let (vx, vy, _, _) = work_area_for_rect(&nr);
                            x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
                            y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
                        }
                        let probe = Rect { x, y, ..nr };
                        let (sx, snapped) = model::snap_gap_to_neighbors(
                            &probe,
                            &others,
                            model::SNAP_THRESHOLD * 1.5,
                        );
                        if snapped {
                            x = sx;
                        }
                        nr = Rect { x, y, ..nr };
                    }
                    let (vx, vy, vw, vh) = work_area_for_rect(&nr);
                    let mut tmp = [nr];
                    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                    nr = tmp[0];
                    let chain = auto_align_on() || grid_align_on();
                    let insert = if chain {
                        fence_insertion_plan(&drag, cx, cy)
                    } else {
                        None
                    };
                    // 四边/四角吸附:距任一边小于栅栏固定间距即贴齐(角=两轴同时)
                    const EDGE: f32 = model::GAP * 1.25;
                    if nr.x - vx < EDGE {
                        nr.x = vx;
                    }
                    if vx + vw - (nr.x + nr.w) < EDGE {
                        nr.x = vx + vw - nr.w;
                    }
                    if nr.y - vy < EDGE {
                        nr.y = vy;
                    }
                    if vy + vh - (nr.y + nr.h) < EDGE {
                        nr.y = vy + vh - nr.h;
                    }
                    // 无插入线(上下方自由放置/自由档):邻居固定间隔吸附
                    if insert.is_none() {
                        let others: Vec<Rect> = drag
                            .start_layout
                            .iter()
                            .filter(|f| f.id != fence_id && !f.hidden)
                            .map(|f| f.rect)
                            .collect();
                        snap_rect_to_neighbors(&mut nr, &others);
                        // 抗重叠:任何位置都不允许覆盖其他栅栏(最小位移推开,保持 GAP)
                        nr = model::avoid_overlap(&nr, &others, vx, vy, vw, vh);
                    }
                    let line = insert.as_ref().map(|p| p.line);
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.rect = nr;
                    }
                    s.insert_line = line;
                    // 记录最后一次有线的方案:松手瞬间滑出容差也必须能插进去
                    if let Some(d) = s.drag.as_mut() {
                        d.last_insert = if chain { insert.clone() } else { None };
                    }
                    // 被拖栅栏需压过其他兄弟栅栏(穿过邻居时不被盖住),但
                    // 任何时候都不得高于正常窗口:提升锚点=最高兄弟栅栏
                    // (仍在桌面 band 内)。旧的 HWND_TOP 曾把它顶到整个 z 栈
                    // 顶端,拖完浮在所有窗口上方。
                    if let Some(&h) = s.windows.get(&fence_id) {
                        let hosts = desktop_hosts();
                        let anchor = s
                            .fences
                            .iter()
                            .find(|f| f.id == fence_id)
                            .and_then(|f| host_for_rect(&f.rect, &hosts))
                            .and_then(|host| drag_elevate_anchor(host.hwnd, h));
                        if let Some(anchor) = anchor {
                            let _z = z_scope(ZIntent::Drag);
                            unsafe {
                                let _ = SetWindowPos(
                                    h,
                                    anchor,
                                    0,
                                    0,
                                    0,
                                    0,
                                    SWP_NOACTIVATE
                                        | SWP_NOSIZE
                                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
                                );
                            }
                        }
                    }
                    // 便宜跟随(2026-09-01):已有像素按新位置重呈现(ULW 一次
                    // 调用完成移动+上屏,零重绘),逐帧 1:1 跟手;内容重渲染
                    // (壁纸种子随位置重烘焙)降到 ~30fps。旧的每 tick 全量
                    // refresh_fence 让 UI 线程饱和,消息积压+渲染掉帧=拖动
                    // 一卡一卡不跟手。
                    if let (Some(&h), Some(surface)) =
                        (s.windows.get(&fence_id), s.surfaces.get(&fence_id))
                    {
                        let _ = render::present_existing_surface(
                            surface,
                            h,
                            nr.x.round() as i32,
                            nr.y.round() as i32,
                        );
                    }
                    let now_ms = resize_now_ms();
                    let render_due = now_ms.saturating_sub(s.last_drag_render_ms) >= 33;
                    if render_due {
                        s.last_drag_render_ms = now_ms;
                    }
                    drop(s);
                    // 两模式统一(2026-08-26):透明模式同样用快照种子,栅栏
                    // 移动到新壁纸区域必须重渲染(重新取该处壁纸作种子)
                    if render_due {
                        refresh_fence(fence_id);
                    }
                    update_guides(None, None);
                    return;
                }
                DragMode::Resize { edges } => {
                    let (cx, cy) = screen_cursor();
                    if (cx - s.drag_settle_x).abs() + (cy - s.drag_settle_y).abs() < 2.0 {
                        return;
                    }
                    s.drag_settle_x = cx;
                    s.drag_settle_y = cy;
                    // 时间节流:表面重建(含 D2D 重绘)最多 ~80fps,
                    // 尺寸仍然 1:1 跟随鼠标(吸附整格只在松手时做,拖动手感保持灵敏)
                    let now = resize_now_ms();
                    if now - s.last_resize_ms < 12 {
                        return;
                    }
                    s.last_resize_ms = now;
                    let chars: Vec<char> = edges.iter().filter(|c| **c != '\0').copied().collect();
                    let nr = model::apply_resize(
                        &drag.start_rect,
                        &chars,
                        cx - drag.start_sx,
                        cy - drag.start_sy,
                    );
                    let (nr, gx, gy) = snap_drag(&s, fence_id, nr);
                    let (vx, vy, vw, vh) = work_area_for_rect(&nr);
                    let mut tmp = [nr];
                    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                    let cur = s.fences.iter().find(|f| f.id == fence_id).map(|f| f.rect);
                    let unchanged = cur.is_some_and(|c| c == tmp[0]);
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.rect = tmp[0];
                    }
                    if unchanged {
                        drop(s);
                        update_guides(gx, gy);
                        return;
                    }
                    drop(s);
                    refresh_fence(fence_id);
                    update_guides(gx, gy);
                    return;
                }
                DragMode::ScrollThumb { grab } => {
                    let calc = {
                        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
                            return;
                        };
                        let items = model::display_list(fence, &s.files);
                        let metrics = s
                            .metrics
                            .get(&fence_id)
                            .copied()
                            .unwrap_or_else(model::DpiMetrics::system);
                        let la = model::layout_with_metrics(fence, items.len(), &metrics);
                        let max = la.total_rows.saturating_sub(la.rows);
                        if max == 0 {
                            None
                        } else {
                            let track_top = metrics.title_h + metrics.pad;
                            let track_h =
                                (fence.rect.h - metrics.pad * 2.0 - metrics.title_h).max(1.0);
                            let thumb_h = (track_h * la.rows as f32 / la.total_rows as f32)
                                .clamp(12.0 * metrics.scale, track_h);
                            let avail = (track_h - thumb_h).max(1.0);
                            let pos = ((y - track_top - grab) / avail).clamp(0.0, 1.0);
                            Some((pos * max as f32).round() as usize)
                        }
                    };
                    if let Some(scroll) = calc {
                        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                            f.scroll_rows = scroll;
                        }
                    }
                    drop(s);
                    refresh_fence(fence_id);
                    return;
                }
                DragMode::Marquee => {
                    s.marquee = Some((drag.start_x, drag.start_y, x, y));
                    drop(s);
                    refresh_fence(fence_id);
                    return;
                }
                DragMode::Icon(idx) => {
                    // 残影拖拽进行中:更新残影位置 + 实时预览重排(其他图标即时让位,
                    // 与原生桌面一致;不松手不生效,松手在栅栏外/按 Esc 则回滚)
                    if s.drag_ghost.is_some() {
                        let (sx, sy) = screen_cursor();
                        let preview_changed = update_ghost_preview(&mut s, fence_id, x, y);
                        drop(s);
                        if preview_changed {
                            refresh_fence(fence_id);
                        }
                        update_ghost(sx - model::icon_size() / 2.0, sy - model::icon_size() / 2.0);
                        return;
                    }
                    if !drag.dragged_out && (dx * dx + dy * dy) > 64.0 {
                        // 判断拖拽目标:仍在当前栅栏内 → 内部残影拖拽(松手重排);
                        // 拖出栅栏 → OLE 拖拽(可与资源管理器互拖)
                        let inside = {
                            let fence = s.fences.iter().find(|f| f.id == fence_id).unwrap();
                            x >= 0.0 && y >= 0.0 && x <= fence.rect.w && y <= fence.rect.h
                        };
                        let paths: Vec<String> = {
                            let fence = s.fences.iter().find(|f| f.id == fence_id).unwrap();
                            let items = model::display_list(fence, &s.files);
                            let pressed = items.get(idx).map(|it| it.path.clone());
                            if pressed
                                .as_ref()
                                .is_some_and(|p| s.selected_paths.contains(p))
                            {
                                items
                                    .iter()
                                    .filter(|it| s.selected_paths.contains(&it.path))
                                    .map(|it| it.path.clone())
                                    .collect()
                            } else {
                                pressed.into_iter().collect()
                            }
                        };
                        if let Some(d) = s.drag.as_mut() {
                            d.dragged_out = true;
                        }
                        if inside && !paths.is_empty() {
                            // 内部拖拽:启动残影 + 实时预览;记录原始顺序与排序模式用于回滚
                            let (sx, sy) = screen_cursor();
                            let hx = sx - model::icon_size() / 2.0;
                            let hy = sy - model::icon_size() / 2.0;
                            let (original, original_sort_mode) = {
                                let fence = s.fences.iter().find(|f| f.id == fence_id).unwrap();
                                (
                                    model::display_list(fence, &s.files)
                                        .into_iter()
                                        .map(|it| it.path)
                                        .collect::<Vec<String>>(),
                                    fence.sort_mode.clone(),
                                )
                            };
                            let dragged_paths: Vec<String> = original
                                .iter()
                                .filter(|path| paths.contains(path))
                                .cloned()
                                .collect();
                            let dragged_set: HashSet<&str> =
                                dragged_paths.iter().map(String::as_str).collect();
                            // 初始槽位=块首在当前顺序中的位置(第一帧即恒等,不跳动)
                            let first_dragged = original
                                .iter()
                                .position(|p| dragged_set.contains(p.as_str()))
                                .unwrap_or(0);
                            let remaining = original.len().saturating_sub(dragged_paths.len());
                            let target = first_dragged.min(remaining);
                            // 预览要求 item_order 生效:立即切"手动"并令 item_order=
                            // 当前显示顺序,保证第一帧与按下时完全一致(否则
                            // "常用"/"时间"排序下 item_order 不参与排序,预览不可见)
                            if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                                f.item_order = original.clone();
                                f.sort_mode = "手动".into();
                            }
                            s.ghost_preview = Some(GhostPreview {
                                fence_id,
                                original,
                                original_sort_mode,
                                dragged_paths: dragged_paths.clone(),
                                target,
                            });
                            s.drag_ghost = Some((dragged_paths.clone(), hx, hy));
                            log(&format!(
                                "icon ghost drag started at ({sx},{sy}); items={} slot={target}",
                                dragged_paths.len()
                            ));
                            drop(s);
                            update_ghost(hx, hy);
                            return;
                        }
                        drop(s);
                        if !paths.is_empty() {
                            do_drag_out(paths);
                        }
                    }
                    return;
                }
            }
        }
    }
    // 悬停更新
    let (hit, n) = {
        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
            return;
        };
        let items = model::display_list(fence, &s.files);
        let n = items.len();
        let metrics = s
            .metrics
            .get(&fence_id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        let lay = model::layout_with_metrics(fence, n, &metrics);
        (
            model::hit_test_with_metrics(fence, &lay, x, y, n, &metrics),
            n,
        )
    };
    let _ = n;
    let new_hover = match hit {
        Hit::Icon(i) => Some(i),
        _ => None,
    };
    let prev = *s.hover.get(&fence_id).unwrap_or(&None);
    s.hover_hit.insert(fence_id, hit);
    // 卡片浮现与图标高亮一致走延迟提交:进入栅栏先记 pending,鼠标停留
    // 满悬停时间才点亮。立即点亮会让"快速划过/点击倒三角/移向托盘"路径
    // 产生亮-灭两次大面积重绘;菜单模态循环还会把熄灭推迟到关菜单之后
    // 才显示——表现为点桌面关闭菜单时栅栏闪一下。
    if !s.fence_hover.get(&fence_id).copied().unwrap_or(false)
        && !s
            .fence_hover_pending
            .get(&fence_id)
            .copied()
            .unwrap_or(false)
    {
        s.fence_hover_pending.insert(fence_id, true);
        unsafe {
            let _ = SetTimer(
                hwnd,
                TIMER_HOVER,
                mouse_hover_time_ms().max(50) as u32,
                None,
            );
        }
    }
    if prev != new_hover {
        // 原生桌面悬停高亮有延迟：先记入 pending，鼠标停留满悬停时间后才提交绘制
        let changed = *s.hover_pending.get(&fence_id).unwrap_or(&prev) != new_hover;
        s.hover_pending.insert(fence_id, new_hover);
        if changed {
            drop(s);
            unsafe {
                let _ = SetTimer(
                    hwnd,
                    TIMER_HOVER,
                    mouse_hover_time_ms().max(50) as u32,
                    None,
                );
            }
            return;
        }
    } else {
        // 回到已提交的图标：取消未到期的延迟提交
        if s.hover_pending.remove(&fence_id).is_some() {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_HOVER);
            }
        }
    }
}

/// 单调毫秒时钟(resize 时间节流用)
fn resize_now_ms() -> u64 {
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 系统悬停时间(SPI_GETMOUSEHOVERTIME,毫秒,默认 400)
fn mouse_hover_time_ms() -> i32 {
    unsafe {
        let mut v: u32 = 0;
        let ok = SystemParametersInfoW(
            SPI_GETMOUSEHOVERTIME,
            std::mem::size_of::<u32>() as u32,
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
        if ok && v > 0 {
            v as i32
        } else {
            400
        }
    }
}

fn handle_lbuttondown(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
    let (rename_edit, file_edit) = {
        let s = state().lock().unwrap();
        (s.rename_edit, s.file_rename_edit)
    };
    if let Some(edit) = rename_edit {
        unsafe {
            let _ = PostMessageW(edit, RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(edit) = file_edit {
        // 点击编辑框本身则交由 EDIT 处理，点击栅栏其它位置提交重命名
        let mut p = POINT {
            x: x as i32,
            y: y as i32,
        };
        let sp = unsafe {
            let _ = ClientToScreen(hwnd, &mut p);
            p
        };
        if !point_in_window_rect(edit, sp.x, sp.y) {
            log("COMMIT via lbuttondown");
            unsafe {
                let _ = PostMessageW(edit, FILE_RENAME_COMMIT_MSG, WPARAM(0), LPARAM(0));
            }
        }
    }
    let mut s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    s.active_fence = Some(fence_id);
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id).cloned() else {
        return;
    };
    if fence.locked {
        return;
    }
    let (sx, sy) = screen_cursor();
    let items = model::display_list(&fence, &s.files);
    let n = items.len();
    let metrics = s
        .metrics
        .get(&fence_id)
        .copied()
        .unwrap_or_else(model::DpiMetrics::system);
    let lay = model::layout_with_metrics(&fence, n, &metrics);
    let hit = model::hit_test_with_metrics(&fence, &lay, x, y, n, &metrics);
    match hit {
        Hit::Collapse => {
            log(&format!("collapse clicked fence {fence_id}"));
            // 点击箭头 = 弹出本栏操作菜单(菜单里含折叠/展开),菜单位置在箭头正下方
            let hwnd_menu = hwnd;
            let ax = fence.rect.x + fence.rect.w - metrics.collapse_w * 0.5;
            let ay = fence.rect.y + metrics.title_h + 6.0;
            drop(s);
            fence_menu(hwnd_menu, fence_id, ax as i32, ay as i32);
            return;
        }
        Hit::Icon(i) => {
            let mut icon_was_selected = false;
            let mut icon_path = String::new();
            let mut is_bin = false;
            if let Some(item) = items.get(i) {
                let path = item.path.clone();
                is_bin = model::is_recycle_bin(&path);
                icon_was_selected = s.selected_paths.contains(&path);
                icon_path = path.clone();
                let ctrl = (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
                let shift = (unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0;
                if shift {
                    let anchor = s.selection_anchor.clone();
                    let anchor_index = anchor
                        .as_ref()
                        .and_then(|p| items.iter().position(|it| &it.path == p))
                        .unwrap_or(i);
                    if !ctrl {
                        s.selected_paths.clear();
                    }
                    let lo = anchor_index.min(i);
                    let hi = anchor_index.max(i);
                    for it in &items[lo..=hi] {
                        s.selected_paths.insert(it.path.clone());
                    }
                } else if ctrl {
                    if !s.selected_paths.remove(&path) {
                        s.selected_paths.insert(path.clone());
                    }
                    s.selection_anchor = Some(path.clone());
                } else {
                    s.selected_paths.clear();
                    s.selected_paths.insert(path.clone());
                    s.selection_anchor = Some(path.clone());
                }
                s.focused_path = Some(path);
            }
            // 回收站图标固定第一位:可选中/可右键,但不可拖动重排
            if !is_bin {
                s.drag = Some(Drag {
                    fence_id,
                    mode: DragMode::Icon(i),
                    start_x: x,
                    start_y: y,
                    start_sx: sx,
                    start_sy: sy,
                    start_rect: fence.rect,
                    start_layout: s.fences.clone(),
                    dragged_out: false,
                    icon_was_selected,
                    icon_path,
                    last_insert: None,
                });
                unsafe {
                    SetCapture(hwnd);
                }
            }
        }
        Hit::Scrollbar => {
            let max = lay.total_rows.saturating_sub(lay.rows);
            if max > 0 {
                let track_top = metrics.title_h + metrics.pad;
                let track_h = (fence.rect.h - metrics.pad * 2.0 - metrics.title_h).max(1.0);
                let thumb_h = (track_h * lay.rows as f32 / lay.total_rows as f32)
                    .clamp(12.0 * metrics.scale, track_h);
                let pos = (fence.scroll_rows as f32 / max as f32).min(1.0);
                let thumb_top = track_top + pos * (track_h - thumb_h);
                let grab = (y - thumb_top).clamp(0.0, thumb_h);
                s.drag = Some(Drag {
                    fence_id,
                    mode: DragMode::ScrollThumb { grab },
                    start_x: x,
                    start_y: y,
                    start_sx: sx,
                    start_sy: sy,
                    start_rect: fence.rect,
                    start_layout: s.fences.clone(),
                    dragged_out: false,
                    icon_was_selected: false,
                    icon_path: String::new(),
                    last_insert: None,
                });
                unsafe {
                    SetCapture(hwnd);
                }
            }
        }
        Hit::Title | Hit::Blank => {
            if matches!(hit, Hit::Blank) {
                let ctrl = (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
                if !ctrl {
                    s.selected_paths.clear();
                    s.focused_path = None;
                    s.selection_anchor = None;
                }
                s.marquee = Some((x, y, x, y));
            }
            s.drag = Some(Drag {
                fence_id,
                mode: if matches!(hit, Hit::Blank) {
                    DragMode::Marquee
                } else {
                    DragMode::Move
                },
                start_x: x,
                start_y: y,
                start_sx: sx,
                start_sy: sy,
                start_rect: fence.rect,
                start_layout: s.fences.clone(),
                dragged_out: false,
                icon_was_selected: false,
                icon_path: String::new(),
                last_insert: None,
            });
            unsafe {
                SetCapture(hwnd);
            }
        }
        h if is_edge(&h) => {
            s.drag = Some(Drag {
                fence_id,
                mode: DragMode::Resize { edges: edges_of(h) },
                start_x: x,
                start_y: y,
                start_sx: sx,
                start_sy: sy,
                start_rect: fence.rect,
                start_layout: s.fences.clone(),
                dragged_out: false,
                icon_was_selected: false,
                icon_path: String::new(),
                last_insert: None,
            });
            unsafe {
                SetCapture(hwnd);
            }
        }
        _ => {}
    }
}

fn is_edge(h: &Hit) -> bool {
    matches!(
        *h,
        Hit::EdgeW
            | Hit::EdgeE
            | Hit::EdgeN
            | Hit::EdgeS
            | Hit::CornerNW
            | Hit::CornerNE
            | Hit::CornerSW
            | Hit::CornerSE
    )
}

fn edges_of(h: Hit) -> [char; 2] {
    match h {
        Hit::EdgeW => ['w', '\0'],
        Hit::EdgeE => ['e', '\0'],
        Hit::EdgeN => ['n', '\0'],
        Hit::EdgeS => ['s', '\0'],
        Hit::CornerNW => ['n', 'w'],
        Hit::CornerNE => ['n', 'e'],
        Hit::CornerSW => ['s', 'w'],
        Hit::CornerSE => ['s', 'e'],
        _ => ['\0', '\0'],
    }
}

/// 拖拽实时预览:根据当前鼠标客户区坐标计算目标格。
/// - 网格坐标双向 clamp(右/下越界映射到最后一列/行),消除边缘死区;
/// - 槽位=悬停格在当前预览顺序中的序号(块落点=鼠标所在格),
///   左右对称、无"慢一拍"滞后;
/// - 始终从 original 快照删除拖动块再插入,幂等可逆。
/// 返回是否发生了变化(调用方据此重绘);悬停在回收站上时不重排(松手即删除)。
fn update_ghost_preview(s: &mut UiState, fence_id: u32, x: f32, y: f32) -> bool {
    let Some(prev) = s.ghost_preview.clone() else {
        return false;
    };
    if prev.fence_id != fence_id || prev.dragged_paths.is_empty() {
        return false;
    }
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id).cloned() else {
        return false;
    };
    let metrics = s
        .metrics
        .get(&fence_id)
        .copied()
        .unwrap_or_else(model::DpiMetrics::system);
    if prev.original.is_empty() {
        return false;
    }
    let dragged: HashSet<&str> = prev.dragged_paths.iter().map(String::as_str).collect();
    // 当前显示顺序(预览期间=已按预览槽位重排的顺序)
    let items = model::display_list(&fence, &s.files);
    let lay = model::layout_with_metrics(&fence, items.len(), &metrics);
    // 回收站图标是固定删除目标:拖到它上方时不重排,松手删除拖动的文件
    {
        let hit = model::hit_test_with_metrics(&fence, &lay, x, y, items.len(), &metrics);
        let over_bin = matches!(hit, Hit::Icon(j)
            if items
                .get(j)
                .is_some_and(|it| model::is_recycle_bin(&it.path)));
        if over_bin != s.trash_target {
            s.trash_target = over_bin;
        }
        if over_bin {
            return false;
        }
    }
    let remaining = items.len().saturating_sub(dragged.len());
    // 悬停格序号(当前显示顺序);越界 clamp 到有效网格与列表末尾
    let cx = x - metrics.pad;
    let cy = y - metrics.title_h - metrics.pad;
    let col = ((cx / metrics.cell_w).floor().max(0.0) as usize).min(lay.cols.saturating_sub(1));
    let row = ((cy / metrics.cell_h).floor().max(0.0) as usize).min(lay.rows.saturating_sub(1));
    let target = (lay.first_index + row.saturating_mul(lay.cols) + col).min(remaining);
    if let Some(p) = s.ghost_preview.as_mut() {
        p.target = target;
    }
    // 插入式指示线(与原生"出现横线松手插入"一致):其余图标不动,
    // 只在目标槽左缘画竖线;行首(跨行插入)画横线。屏幕坐标存入 insert_line。
    let slot = target.min(lay.first_index + lay.rows.saturating_mul(lay.cols).saturating_sub(1));
    let srow = (slot - lay.first_index) / lay.cols.max(1);
    let scol = (slot - lay.first_index) % lay.cols.max(1);
    let gx0 = fence.rect.x + metrics.pad + scol as f32 * metrics.cell_w;
    let gy0 = fence.rect.y + metrics.title_h + metrics.pad + srow as f32 * metrics.cell_h;
    let line = if scol == 0 && srow > 0 {
        // 横线:插到上一行与本行之间
        let y = gy0 - (metrics.cell_h - metrics.icon_px) * 0.2;
        (
            fence.rect.x + metrics.pad * 0.5,
            y,
            fence.rect.w - metrics.pad,
            2.5,
        )
    } else {
        // 竖线:插到该槽左侧
        let x = gx0 - (metrics.cell_w - metrics.icon_px) * 0.2;
        (x, gy0 + 2.0, 2.5, metrics.icon_px + 4.0)
    };
    let changed = s.insert_line != Some(line);
    s.insert_line = Some(line);
    // 图标不动 → 无需刷新栅栏,只刷新 overlay 指示线
    let _ = changed;
    false
}

/// 回滚拖拽预览到原始顺序与排序模式(拖出释放/取消)
fn rollback_ghost_preview(s: &mut UiState) {
    if let Some(prev) = s.ghost_preview.take() {
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == prev.fence_id) {
            f.item_order = prev.original;
            f.sort_mode = prev.original_sort_mode;
        }
    }
}

/// 拖出释放点(屏幕坐标)是否落在某个非源栅栏的回收站图标上。
fn release_on_recycle_bin_screen(s: &UiState, sx: f32, sy: f32, source_fence: u32) -> bool {
    for fence in &s.fences {
        if fence.id == source_fence || fence.hidden || fence.collapsed {
            continue;
        }
        let cx = sx - fence.rect.x;
        let cy = sy - fence.rect.y;
        if cx < 0.0 || cy < 0.0 || cx > fence.rect.w || cy > fence.rect.h {
            continue;
        }
        let items = model::display_list(fence, &s.files);
        let metrics = s
            .metrics
            .get(&fence.id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        let lay = model::layout_with_metrics(fence, items.len(), &metrics);
        if let Hit::Icon(j) =
            model::hit_test_with_metrics(&fence, &lay, cx, cy, items.len(), &metrics)
        {
            let hit_bin = items
                .get(j)
                .is_some_and(|it| model::is_recycle_bin(&it.path));
            log(&format!(
                "[dbg] trash-release probe fence={} client=({:.0},{:.0}) hit=Icon({}) bin={}",
                fence.id, cx, cy, j, hit_bin
            ));
            if hit_bin {
                return true;
            }
        }
    }
    false
}

fn handle_lbuttonup(_hwnd: HWND, fence_id: u32, x: f32, y: f32) {
    let mut s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(drag) = s.drag.take() {
        let mut changed_final: Vec<u32> = Vec::new();
        // 内部图标残影拖拽收尾:松手在栅栏内 = 预览顺序生效(落格重排已实时完成,
        // 只需持久化);松手在栅栏外 = 未移动,回滚到原始顺序
        if s.drag_ghost.is_some() {
            s.drag_ghost = None;
            let inside = {
                let fence = s.fences.iter().find(|f| f.id == fence_id);
                match fence {
                    Some(f) => x >= 0.0 && y >= 0.0 && x <= f.rect.w && y <= f.rect.h,
                    None => false,
                }
            };
            // 拖到回收站图标上松手 → 删除拖动的文件(回滚重排预览,rescan 移除图标)
            if inside && s.trash_target {
                let paths: Vec<String> = s
                    .ghost_preview
                    .as_ref()
                    .map(|p| p.dragged_paths.clone())
                    .unwrap_or_default();
                s.trash_target = false;
                rollback_ghost_preview(&mut s);
                s.marquee = None;
                let hwnd = s.windows.get(&fence_id).copied().unwrap_or(HWND(0));
                log(&format!(
                    "drop onto recycle bin: deleting {} items",
                    paths.len()
                ));
                drop(s);
                unsafe {
                    let _ = ReleaseCapture();
                }
                if !paths.is_empty() {
                    shell::delete_to_recycle_bin_many(hwnd, &paths);
                    rescan();
                } else {
                    refresh_fence(fence_id);
                }
                update_guides(None, None);
                return;
            }
            s.trash_target = false;
            s.insert_line = None;
            if inside && s.ghost_preview.is_some() {
                let (target, count, original, dragged) = s
                    .ghost_preview
                    .as_ref()
                    .map(|p| {
                        (
                            p.target,
                            p.dragged_paths.len(),
                            p.original.clone(),
                            p.dragged_paths.clone(),
                        )
                    })
                    .unwrap_or((0, 0, Vec::new(), Vec::new()));
                s.ghost_preview = None;
                // 松手才拼接:按指示线位置把拖动块插入目标槽(其余项顺移)
                let order = model::reorder_paths_as_block(&original, &dragged, target);
                if !order.is_empty() {
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.item_order = order;
                        f.sort_mode = "手动".into();
                    }
                }
                log(&format!(
                    "icon drag committed: items={count} -> slot={target}"
                ));
                let cfg = s.fences.clone();
                if let Err(err) = model::save_config(&cfg) {
                    log(&format!("icon drag save failed: {err}"));
                }
            } else if !inside && {
                // 拖出栅栏后客户区坐标为负,WM 消息里以无符号解码会变成 ~65k;
                // 用真实屏幕光标位置做命中才可靠
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                release_on_recycle_bin_screen(&s, pt.x as f32, pt.y as f32, fence_id)
            } {
                // 拖出本栅栏释放:落点在其它栅栏的回收站图标上 → 删除(用户拖文件进回收站)
                let paths: Vec<String> = s
                    .ghost_preview
                    .as_ref()
                    .map(|p| p.dragged_paths.clone())
                    .unwrap_or_default();
                rollback_ghost_preview(&mut s);
                s.marquee = None;
                let hwnd = s.windows.get(&fence_id).copied().unwrap_or(HWND(0));
                log(&format!(
                    "cross-fence drop onto recycle bin: deleting {} items",
                    paths.len()
                ));
                drop(s);
                unsafe {
                    let _ = ReleaseCapture();
                }
                if !paths.is_empty() {
                    shell::delete_to_recycle_bin_many(hwnd, &paths);
                    rescan();
                } else {
                    refresh_fence(fence_id);
                }
                update_guides(None, None);
                return;
            } else {
                // 松手在其他栅栏上 = 把拖动的文件分配给那个栅栏
                // (自定义分类模式的核心入口;自动分类下也可用来"收藏"到自建栅栏)
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                let target = s
                    .fences
                    .iter()
                    .find(|f| {
                        f.id != fence_id
                            && !f.hidden
                            && !f.collapsed
                            && pt.x as f32 >= f.rect.x
                            && pt.y as f32 >= f.rect.y
                            && pt.x as f32 <= f.rect.x + f.rect.w
                            && pt.y as f32 <= f.rect.y + f.rect.h
                    })
                    .map(|f| f.id);
                let dragged: Vec<String> = s
                    .ghost_preview
                    .as_ref()
                    .map(|p| p.dragged_paths.clone())
                    .unwrap_or_default();
                let mut assigned: Option<u32> = None;
                if let (Some(tid), false) = (target, dragged.is_empty()) {
                    // 从其他栅栏的收纳表中移除(一个文件只属于一个栅栏)
                    for f in s.fences.iter_mut() {
                        f.pinned.retain(|p| !dragged.contains(p));
                    }
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == tid) {
                        for path in &dragged {
                            if !f.pinned.contains(path) {
                                f.pinned.push(path.clone());
                            }
                        }
                    }
                    assigned = Some(tid);
                }
                rollback_ghost_preview(&mut s);
                if let Some(tid) = assigned {
                    log(&format!(
                        "assigned {} items to fence {tid} (custom category)",
                        dragged.len()
                    ));
                    let cfg = s.fences.clone();
                    let _ = model::save_config(&cfg);
                    let _ = tid;
                } else if !inside {
                    log("icon drag released outside fence: rolled back");
                }
            }
            s.marquee = None;
            drop(s);
            rebuild_pins();
            refresh_all_fences();
            update_guides(None, None);
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        }
        if drag.fence_id == fence_id && !drag.dragged_out {
            let (cx, cy) = screen_cursor();
            let dx = cx - drag.start_sx;
            let dy = cy - drag.start_sy;
            // Explorer 慢双击重命名:第一次点击选中图标,稍后再次点击"已选中"的
            // 同一图标(非双击、无拖动、无Ctrl)→ 原位进入重命名
            if let DragMode::Icon(_) = drag.mode {
                let ctrl = (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
                let moved = (dx * dx + dy * dy) > 64.0;
                if drag.icon_was_selected
                    && !ctrl
                    && !moved
                    && !drag.icon_path.is_empty()
                    && !model::is_recycle_bin(&drag.icon_path)
                {
                    let path = drag.icon_path.clone();
                    s.marquee = None;
                    drop(s);
                    refresh_fence(fence_id);
                    start_file_rename(path);
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return;
                }
            }
            let others: Vec<Rect> = s
                .fences
                .iter()
                .filter(|f| f.id != fence_id)
                .map(|f| f.rect)
                .collect();
            if matches!(drag.mode, DragMode::Move | DragMode::Resize { .. }) {
                // s 已被本函数持有,直接用克隆快照入撤销栈,避免重复加锁
                // The live mouse-move path may have already changed the anchor.
                // Store the complete pre-operation snapshot captured at button-up entry
                // only as a fallback; the real snapshot is captured on button-down below.
                push_undo_snapshot(drag.start_layout.clone(), fence_id, drag.start_rect);
            }
            match drag.mode {
                DragMode::Marquee => {
                    if let Some(fence) = s.fences.iter().find(|f| f.id == fence_id).cloned() {
                        let items = model::display_list(&fence, &s.files);
                        let lay = model::layout(&fence, items.len());
                        let m = s
                            .marquee
                            .take()
                            .unwrap_or((drag.start_x, drag.start_y, x, y));
                        let mr = Rect {
                            x: m.0,
                            y: m.1,
                            w: m.2 - m.0,
                            h: m.3 - m.1,
                        };
                        let ctrl =
                            (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
                        if !ctrl {
                            s.selected_paths.clear();
                        }
                        for idx in model::indices_in_rect(&lay, &mr, items.len()) {
                            if let Some(it) = items.get(idx) {
                                s.selected_paths.insert(it.path.clone());
                            }
                        }
                        if let Some(path) = s.selected_paths.iter().next().cloned() {
                            s.focused_path = Some(path);
                        }
                    } else {
                        s.marquee = None;
                    }
                }
                DragMode::Move => {
                    // 插入式提交(2026-09-02 行内槽位模型):有指示线 → 应用
                    // model::row_insert_layout 的位置分配(尺寸保持各自,只动
                    // 受影响两行);无指示线 → 原地自由放置。
                    let (cx, cy) = screen_cursor();
                    let moved = (cx - drag.start_sx) * (cx - drag.start_sx)
                        + (cy - drag.start_sy) * (cy - drag.start_sy)
                        > 64.0;
                    let plan = if moved && (auto_align_on() || grid_align_on()) {
                        match fence_insertion_plan(&drag, cx, cy) {
                            Some(p) => Some(p),
                            // 松手瞬间滑出容差但线还亮着:沿用最后一次方案,
                            // 保证"看到线即可插入"
                            None => drag.last_insert.clone(),
                        }
                    } else {
                        None
                    };
                    // 纯点击(无拖动)不做任何拼接重排,位置保持原样
                    if !moved {
                        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                            f.rect = drag.start_rect;
                        }
                        s.insert_line = None;
                    } else if let Some(p) = plan {
                        // 行内槽位落位:只应用分配到的位置(未涉及成员取到
                        // 自己原位=不动),尺寸保持各自,行结构不塌
                        for (id, (x, y)) in &p.assign {
                            if let Some(f) = s.fences.iter_mut().find(|f| f.id == *id) {
                                f.rect.x = *x;
                                f.rect.y = *y;
                            }
                        }
                        // 被拖者落点夹回工作区(行尾延伸槽可能出屏)
                        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                            f.rect.x = p.land.0;
                            f.rect.y = p.land.1;
                            let (vx, vy, vw, vh) = work_area_for_rect(&f.rect);
                            let mut tmp = [f.rect];
                            model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                            f.rect = tmp[0];
                        }
                    } else {
                        // 原地放置(与拖动预览同式:跟手位置 + 夹屏)
                        let mut nr = Rect {
                            x: drag.start_rect.x + (cx - drag.start_sx),
                            y: drag.start_rect.y + (cy - drag.start_sy),
                            w: drag.start_rect.w,
                            h: drag.start_rect.h,
                        };
                        if !auto_align_on() {
                            let others: Vec<Rect> = drag
                                .start_layout
                                .iter()
                                .filter(|f| f.id != fence_id)
                                .map(|f| f.rect)
                                .collect();
                            let mut x = nr.x;
                            let mut y = nr.y;
                            if grid_align_on() {
                                let (vx, vy, _, _) = work_area_for_rect(&nr);
                                x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
                                y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
                            }
                            let probe = Rect { x, y, ..nr };
                            let (sx, snapped) = model::snap_gap_to_neighbors(
                                &probe,
                                &others,
                                model::SNAP_THRESHOLD * 1.5,
                            );
                            if snapped {
                                x = sx;
                            }
                            nr = Rect { x, y, ..nr };
                        }
                        let (vx, vy, vw, vh) = work_area_for_rect(&nr);
                        let mut tmp = [nr];
                        model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                        let mut fr = tmp[0];
                        const EDGE: f32 = model::GAP * 1.25;
                        if fr.x - vx < EDGE {
                            fr.x = vx;
                        }
                        if vx + vw - (fr.x + fr.w) < EDGE {
                            fr.x = vx + vw - fr.w;
                        }
                        if fr.y - vy < EDGE {
                            fr.y = vy;
                        }
                        if vy + vh - (fr.y + fr.h) < EDGE {
                            fr.y = vy + vh - fr.h;
                        }
                        let others: Vec<Rect> = drag
                            .start_layout
                            .iter()
                            .filter(|f| f.id != fence_id && !f.hidden)
                            .map(|f| f.rect)
                            .collect();
                        snap_rect_to_neighbors(&mut fr, &others);
                        fr = model::avoid_overlap(&fr, &others, vx, vy, vw, vh);
                        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                            f.rect = fr;
                        }
                    }
                    s.insert_line = None;
                    changed_final = s.fences.iter().map(|f| f.id).collect();
                }
                DragMode::Resize { edges } => {
                    // 用户手动缩放：此后高度不再自动收敛到内容（尊重用户意图）
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.manual_size = true;
                    }
                    let chars: Vec<char> = edges.iter().filter(|c| **c != '\0').copied().collect();
                    let nr0 = model::apply_resize(&drag.start_rect, &chars, dx, dy);
                    let (nr0, _, _) = snap_drag(&s, fence_id, nr0);
                    let w_inv = chars.contains(&'w');
                    let n_inv = chars.contains(&'n');
                    // Snap the final content area to the icon-cell grid so the
                    // fence always fits exactly N×M icons.
                    let (sw, sh) = model::snap_fence_size(nr0.w, nr0.h);
                    let mut nr = nr0;
                    nr.w = sw;
                    nr.h = sh;
                    if w_inv {
                        let far = drag.start_rect.x + drag.start_rect.w;
                        nr.x = (far - sw).max(0.0);
                    }
                    if n_inv {
                        let bot = drag.start_rect.y + drag.start_rect.h;
                        nr.y = (bot - sh).max(0.0);
                    }
                    let (vx, vy, vw, vh) = work_area_for_rect(&nr);
                    let mut tmp = [nr];
                    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                    // 保留用户缩放的尺寸，只解除重叠（其它栅栏不动）
                    let final_r = if auto_align_on() {
                        tmp[0]
                    } else {
                        model::avoid_overlap(&tmp[0], &others, vx, vy, vw, vh)
                    };
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.rect = final_r;
                    }
                    changed_final = vec![fence_id];
                }
                _ => {}
            }
            if auto_align_on() && matches!(drag.mode, DragMode::Resize { .. }) {
                compact_neighbors_after_resize(&mut s.fences, fence_id);
                changed_final = s.fences.iter().map(|f| f.id).collect();
            }
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
        s.marquee = None;
        drop(s);
        if changed_final.is_empty() {
            refresh_fence(fence_id);
        } else {
            for id in changed_final {
                refresh_fence(id);
            }
        }
        // 松手后清除对齐参考线和框选矩形
        update_guides(None, None);
        unsafe {
            let _ = ReleaseCapture();
        }
        // 拖拽期间被拖栅栏被提升到兄弟栅栏之上(仅 band 内);拖拽结束立即
        // 归位带内绝缘位(最低可见外来窗正下方,与整链同位;勿回退到宿主
        // 正上方——带底扰动区)。若等自愈兜底,栅栏会在其他窗口上方漂移=
        // 用户看到的"栅栏浮在别的窗口上方"。
        {
            let s = state().lock().unwrap();
            let hosts = desktop_hosts();
            let target = s.fences.iter().find(|f| f.id == fence_id).map(|f| {
                (f.hidden, f.rect)
            });
            if let Some((hidden, rect)) = target {
                if !hidden {
                    if let Some(host) = host_for_rect(&rect, &hosts) {
                        if let Some(after) = band_attach_anchor(host.hwnd, HWND(0), false) {
                            if let Some(fh) = s.windows.get(&fence_id) {
                                let _z = z_scope(ZIntent::Drag);
                                unsafe {
                                    let _ = SetWindowPos(
                                        *fh,
                                        after,
                                        rect.x.round() as i32,
                                        rect.y.round() as i32,
                                        0,
                                        0,
                                        SWP_NOSIZE | SWP_NOACTIVATE,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn handle_dblclk(fence_id: u32, x: f32, y: f32) {
    let s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
        return;
    };
    let items = model::display_list(fence, &s.files);
    let n = items.len();
    let metrics = s
        .metrics
        .get(&fence_id)
        .copied()
        .unwrap_or_else(model::DpiMetrics::system);
    let lay = model::layout_with_metrics(fence, n, &metrics);
    let hit = model::hit_test_with_metrics(fence, &lay, x, y, n, &metrics);
    if let Hit::Icon(i) = hit {
        if let Some(it) = items.get(i) {
            let p = it.path.clone();
            drop(s);
            open_item(&p);
            return;
        }
    }
}

fn handle_rbuttonup(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
    // 客户区坐标 → 屏幕坐标(TrackPopupMenu 要屏幕坐标,否则菜单弹到错误位置)
    let (sx, sy) = unsafe {
        let mut p = POINT {
            x: x as i32,
            y: y as i32,
        };
        let _ = ClientToScreen(hwnd, &mut p);
        (p.x, p.y)
    };
    // 右键同样先退出进行中的重命名(与原生一致:右键编辑框外部提交)
    rename_click_outside_hit(sx, sy, "rbutton");
    {
        let s = match state().try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
            return;
        };
        let items = model::display_list(fence, &s.files);
        let n = items.len();
        let metrics = s
            .metrics
            .get(&fence_id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        let lay = model::layout_with_metrics(fence, n, &metrics);
        let hit = model::hit_test_with_metrics(fence, &lay, x, y, n, &metrics);
        match hit {
            Hit::Icon(i) => {
                if let Some(it) = items.get(i) {
                    let p = it.path.clone();
                    drop(s);
                    {
                        let mut s = state().lock().unwrap();
                        if !s.selected_paths.contains(&p) {
                            s.selected_paths.clear();
                            s.selected_paths.insert(p.clone());
                        }
                        s.focused_path = Some(p.clone());
                        s.selection_anchor = Some(p.clone());
                    }
                    refresh_fence(fence_id);
                    let menu_paths = {
                        let s = state().lock().unwrap();
                        let selected: Vec<String> = s.selected_paths.iter().cloned().collect();
                        if selected.len() > 1 && selected.iter().any(|path| path == &p) {
                            selected
                        } else {
                            vec![p.clone()]
                        }
                    };
                    // 与 Explorer 一致：右键已选中的多个项目时，按完整选区构造 Shell 菜单。
                    shell::show_shell_context_menu_paths(hwnd, &menu_paths, sx, sy);
                    return;
                }
            }
            // 空白内容区:与原生桌面一致,弹桌面右键菜单(查看/排序方式/刷新/
            // 粘贴/新建/显示设置/个性化…),DeskLens 命令挂在子菜单里
            Hit::Blank => {
                drop(s);
                let cmd =
                    shell::show_desktop_context_menu(hwnd, sx, sy, &align_mode(), &render_mode());
                if cmd == 0 {
                    // 桌面菜单链路不可用时退化为栅栏管理菜单
                    fence_menu(hwnd, fence_id, sx, sy);
                } else {
                    dispatch_desktop_command(cmd);
                }
                return;
            }
            // 标题栏/折叠钮/滚动条:栅栏管理菜单
            _ => {}
        }
    }
    fence_menu(hwnd, fence_id, sx, sy);
}

/// 桌面背景右键菜单里 DeskFence 子菜单的命令分派
fn dispatch_desktop_command(id: u32) {
    match id {
        shell::DL_CMD_ADD_FENCE => {
            let base = state()
                .lock()
                .unwrap()
                .fences
                .iter()
                .map(|f| f.id)
                .next()
                .unwrap_or(0);
            if base != 0 {
                let _ = add_fence_after(base);
            }
        }
        shell::DL_CMD_SHOW_ALL => show_all_fences(),
        shell::DL_CMD_HIDE_ALL => set_all_hidden(true),
        shell::DL_CMD_UNDO => undo_layout(),
        shell::DL_CMD_AUTO_ALIGN => {
            // 桌面菜单入口:循环切换三档
            let next = match align_mode().as_str() {
                "auto" => "grid",
                "grid" => "free",
                _ => "auto",
            };
            set_align_mode(next);
        }
        shell::DL_CMD_RENDER_MODE => {
            // 渲染模式切换:透明(动态壁纸兼容) ↔ 精确(壁纸底+ClearType)
            let next = if render_mode() == "precise" {
                "transparent"
            } else {
                "precise"
            };
            set_render_mode(next);
        }
        shell::DL_CMD_HELP => show_help(),
        shell::DL_CMD_REFRESH => rescan(),
        shell::DL_CMD_QUIT => quit_app(),
        _ => {}
    }
}

fn handle_wheel(fence_id: u32, delta: i32) {
    let mut s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let (total_rows, rows) = {
        let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
            return;
        };
        if fence.locked {
            return;
        }
        let items = model::display_list(fence, &s.files);
        let lay = model::layout(fence, items.len());
        (lay.total_rows, lay.rows)
    };
    if total_rows <= rows {
        return;
    }
    let max = total_rows.saturating_sub(rows);
    let Some(fence) = s.fences.iter_mut().find(|f| f.id == fence_id) else {
        return;
    };
    if delta > 0 {
        fence.scroll_rows = fence.scroll_rows.saturating_sub(1);
    } else {
        fence.scroll_rows = fence.scroll_rows.saturating_add(1).min(max);
    }
    let cfg = s.fences.clone();
    let _ = model::save_config(&cfg);
    drop(s);
    refresh_fence(fence_id);
}

fn handle_setcursor(hwnd: HWND, fence_id: u32) {
    let hit = {
        let s = match state().try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let locked = s
            .fences
            .iter()
            .find(|f| f.id == fence_id)
            .map(|f| f.locked)
            .unwrap_or(false);
        if locked {
            // 锁定后不再显示缩放光标
            Hit::None
        } else {
            s.hover_hit.get(&fence_id).copied().unwrap_or(Hit::None)
        }
    };
    let cid: usize = match hit {
        Hit::CornerNW | Hit::CornerSE => 32642,
        Hit::CornerNE | Hit::CornerSW => 32643,
        Hit::EdgeW | Hit::EdgeE => 32644,
        Hit::EdgeN | Hit::EdgeS => 32645,
        _ => 32512,
    };
    unsafe {
        if let Ok(hc) = LoadCursorW(None, PCWSTR::from_raw(cid as usize as *const u16)) {
            SetCursor(hc);
        }
    }
    let _ = hwnd;
}

// ---------------- 对外回调 ----------------

/// OLE 拖入栅栏(跨栅栏/从资源管理器拖入)。落点在回收站图标上 → 删除文件;
/// 否则固定(pin)到该栅栏。screen_x/screen_y 为屏幕坐标。
pub fn on_fence_drop_cb(fence_id: u32, paths: Vec<String>, screen_x: i32, screen_y: i32) {
    // 先换算成栅栏客户区坐标做命中测试
    let (cx, cy) = {
        let s = state().lock().unwrap();
        match s.windows.get(&fence_id).copied() {
            Some(hwnd) if hwnd.0 != 0 => unsafe {
                let mut q = POINT {
                    x: screen_x,
                    y: screen_y,
                };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut q);
                (q.x as f32, q.y as f32)
            },
            _ => (-1.0, -1.0),
        }
    };
    let mut delete_to_bin = false;
    if cx >= 0.0 && cy >= 0.0 {
        let s = state().lock().unwrap();
        if let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) {
            let items = model::display_list(fence, &s.files);
            let lay = model::layout(fence, items.len());
            if let Hit::Icon(j) = model::hit_test(fence, &lay, cx, cy, items.len()) {
                delete_to_bin = items
                    .get(j)
                    .is_some_and(|it| model::is_recycle_bin(&it.path));
            }
        }
    }
    if delete_to_bin {
        let hwnd = state()
            .lock()
            .unwrap()
            .windows
            .get(&fence_id)
            .copied()
            .unwrap_or(HWND(0));
        log(&format!(
            "OLE drop onto recycle bin: deleting {} items",
            paths.len()
        ));
        shell::delete_to_recycle_bin_many(hwnd, &paths);
        rescan();
        return;
    }
    {
        let mut s = state().lock().unwrap();
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
            for p in &paths {
                if model::is_recycle_bin(p) {
                    continue;
                }
                if !f.pinned.contains(p) {
                    f.pinned.push(p.clone());
                }
            }
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
    }
    refit_auto_fence_heights();
    refresh_fence(fence_id);
}

fn open_item(path: &str) {
    if model::is_recycle_bin(path) {
        shell::open_recycle_bin();
        return;
    }
    shell::open_path(path);
    // 记录打开次数/时间(常用排序依据)
    model::record_open(path);
    model::save_usage();
}

fn do_drag_out(paths: Vec<String>) {
    ole::drag_out_files(&paths, |_target| {});
}
