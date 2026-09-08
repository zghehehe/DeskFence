//! 窗口管理与交互：栅栏窗口、命中测试、移动/缩放/滚动、右键菜单、重命名、刷新

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateFontIndirectW, DeleteObject, EnumDisplayMonitors, GetDC, GetMonitorInfoW,
    GetTextExtentPoint32W, GetTextMetricsW, MonitorFromRect, MonitorFromWindow, ReleaseDC,
    SelectObject, HBRUSH, HDC, HFONT, HMONITOR, LOGFONTW, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    ScreenToClient, TEXTMETRICW,
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
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
};
// windows 0.52 未导出的 WinEvent 标志,按 WinUser.h 补定义
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::{self, Fence, FileItem, Hit, Rect};
use crate::ole;
use crate::render;
use crate::render::{IconBuffer, Renderer, Surface};
use crate::shell;
use crate::iconcache::{load_icon_cache_file, save_icon_cache_file_now};
use crate::selfheal::*;
use crate::menu::{delete_fence_ex, dispatch_desktop_command, fence_menu, quit_app, set_render_mode, show_tray_menu};

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

pub(crate) fn hinstance() -> HINSTANCE {
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
/// 统一的设置落盘入口:读 settings.json → 就地改一个字段 → 原子写回。
/// 旧实现是 5 处 set_*_stored 各自手工重建 Settings 逐字段拷贝,新增字段
/// 漏改任意一处=静默把该字段写回默认值(实锤:每次开关托盘设置都会把
/// deleted_category_at 墓碑表整个清空,已删的分类栅栏随后被缺类补建复活)。
/// 收敛后新增 Settings 字段无需改这里,任何部分更新天然保留其余字段。
pub(crate) fn update_stored_settings(f: impl FnOnce(&mut model::Settings)) {
    let mut s = model::load_settings();
    f(&mut s);
    model::save_settings(&s);
}

/// 写入对齐档位并立即持久化到设置文件
pub(crate) fn set_align_mode_stored(mode: &str) {
    *ALIGN_MODE.lock().unwrap() = mode.to_string();
    update_stored_settings(|s| s.align_mode = mode.to_string());
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
pub(crate) fn set_render_mode_stored(mode: &str) {
    *RENDER_MODE.lock().unwrap() = mode.to_string();
    update_stored_settings(|s| s.render_mode = mode.to_string());
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
pub(crate) fn set_desktop_state_stored(mode: &str) {
    *DESKTOP_STATE.lock().unwrap() = mode.to_string();
    update_stored_settings(|s| s.desktop_state = mode.to_string());
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
pub(crate) fn set_auto_category_stored(v: bool) {
    model::set_auto_category(v);
    *AUTO_CATEGORY.lock().unwrap() = Some(v);
    update_stored_settings(|s| s.auto_category = v);
}

/// z 守卫设置(缓存读取,模式同上):菜单落盘点需要带上当前值。
/// 2026-09-08 起暴露为托盘开关(异常降级用),缓存需可写。
static Z_GUARD: Mutex<Option<bool>> = Mutex::new(None);
pub(crate) fn z_guard_setting() -> bool {
    let mut g = Z_GUARD.lock().unwrap();
    if let Some(v) = *g {
        return v;
    }
    let v = model::load_settings().z_guard;
    *g = Some(v);
    v
}
pub(crate) fn set_z_guard_stored(v: bool) {
    *Z_GUARD.lock().unwrap() = Some(v);
    update_stored_settings(|s| s.z_guard = v);
}

/// 常显栅栏边框线(托盘开关,默认关=悬停/拖拽才浮现,2026-09-01 用户新增):
/// 开=全部栅栏常显边框/标题/角手柄,便于观察布局边界;关=无边框常显基线。
static SHOW_CHROME: AtomicBool = AtomicBool::new(false);

pub fn chrome_always_on() -> bool {
    SHOW_CHROME.load(Ordering::Relaxed)
}
pub(crate) fn set_show_chrome_stored(on: bool) {
    SHOW_CHROME.store(on, Ordering::Relaxed);
    update_stored_settings(|s| s.show_chrome = on);
}

/// 分类栅栏删除墓碑:删除时刻 epoch ms。墓碑在位的分类不再被缺类补建
/// 复活,除非之后出现该类的新文件(mtime 晚于墓碑)——那时清除墓碑并
/// 正常补建,保留"首次出现该类文件会自动新建"的原设计。
fn category_tombstone_at(cat: &str) -> Option<u64> {
    model::load_settings().deleted_category_at.get(cat).copied()
}
pub(crate) fn set_category_tombstone(cat: &str) {
    let mut s = model::load_settings();
    s.deleted_category_at
        .insert(cat.to_string(), model::epoch_ms());
    model::save_settings(&s);
}
pub(crate) fn clear_category_tombstone(cat: &str) {
    let mut s = model::load_settings();
    if s.deleted_category_at.remove(cat).is_some() {
        model::save_settings(&s);
    }
}
/// 重建"已收纳(pinned)"路径表(自定义分类模式的数据源)
pub(crate) fn rebuild_pins() {
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
pub(crate) const WM_DL3_ZCHECK: u32 = WM_APP + 7;
/// windows 0.52 crate 未导出,按 Win32 头文件补定义
const WM_MOUSELEAVE: u32 = 0x02A3;
pub(crate) static TRAY_HWND: OnceLock<HWND> = OnceLock::new();
/// 菜单前台宿主窗口(1x1 隐形):菜单前台化的目标,避免提升栅栏窗口 z 序
pub(crate) static MENU_HOST_HWND: OnceLock<HWND> = OnceLock::new();

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
pub(crate) enum DragMode {
    Move,
    Resize { edges: [char; 2] },
    Icon(usize),
    Marquee,
    ScrollThumb { grab: f32 },
}

/// 拖拽插入方案(2026-09-02:行内槽位模型)。几何在 model.rs
/// (rows_from_rects/row_slot_of/row_insert_layout,有单测),此处只做适配。
#[derive(Clone)]
pub(crate) struct InsertPlan {
    /// 全体可见栅栏的新位置(逐 start_layout 可见成员,含被拖者)
    assign: Vec<(u32, (f32, f32))>,
    /// 被拖者落点
    land: (f32, f32),
    /// 指示线 (x, y, w, h)
    line: (f32, f32, f32, f32),
}

#[derive(Clone)]
pub(crate) struct Drag {
    pub(crate) fence_id: u32,
    pub(crate) mode: DragMode,
    pub(crate) start_x: f32,
    pub(crate) start_y: f32,
    /// 按下时的屏幕坐标（Move/Resize 的位移基准，与窗口位置无关）
    pub(crate) start_sx: f32,
    pub(crate) start_sy: f32,
    pub(crate) start_rect: Rect,
    pub(crate) start_layout: Vec<Fence>,
    pub(crate) dragged_out: bool,
    /// 图标按下时该项是否已被选中(第二次点击已选中项 = Explorer 的慢双击重命名)
    pub(crate) icon_was_selected: bool,
    pub(crate) icon_path: String,
    /// Move 拖拽最后一次有插入线的方案:松手瞬间滑出容差也必须能插进去
    /// (以最后一次方案为准,2026-09-02)
    pub(crate) last_insert: Option<InsertPlan>,
}

/// 拖拽实时预览状态:拖动中即时重排显示,松手才生效;取消/拖出释放则回滚
#[derive(Clone)]
pub(crate) struct GhostPreview {
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
pub(crate) struct ArrivalAnimation {
    fence_id: u32,
    path: String,
    name: String,
    from: (f32, f32),
    to: (f32, f32),
    started_ms: u64,
    duration_ms: u64,
}

pub(crate) struct UiState {
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
    /// Target-fence metrics and stable cell centers for active file editors.
    pub rename_metrics: HashMap<isize, model::DpiMetrics>,
    pub rename_centers: HashMap<isize, i32>,
    pub rename_fonts: HashMap<isize, HFONT>,
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

pub(crate) fn state() -> &'static Mutex<UiState> {
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
            rename_metrics: HashMap::new(),
            rename_centers: HashMap::new(),
            rename_fonts: HashMap::new(),
            drag_settle_x: 0.0,
            drag_settle_y: 0.0,
            last_resize_ms: 0,
            last_move_ms: 0,
            last_drag_render_ms: 0,
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
    s.arrival_animations.clear();
}

pub(crate) fn clear_fence_interaction(s: &mut UiState, fence_id: u32) {
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
    }
    // Selection is global because pinned items can appear in multiple fences.
    // A lifecycle change invalidates any visual ownership, so clear it wholesale.
    s.selected_paths.clear();
    s.focused_path = None;
    s.selection_anchor = None;
}

pub(crate) fn finish_interaction_cleanup() {
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
    if s.drag_ghost.is_none() && s.arrival_animations.is_empty() {
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
pub(crate) struct HostInfo {
    pub(crate) hwnd: HWND,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    pub(crate) primary: bool,
    /// 宿主是否可见:Explorer 重启重建期间 WorkerW 可能短暂隐藏,
    /// 收养到隐藏宿主会导致栅栏不可见,必须过滤。
    pub(crate) visible: bool,
}

static HOSTS_CACHE: OnceLock<Mutex<(std::time::Instant, Vec<HostInfo>)>> = OnceLock::new();

fn invalidate_hosts_cache() {
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
pub(crate) fn host_for_rect(rect: &Rect, hosts: &[HostInfo]) -> Option<HostInfo> {
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
pub(crate) fn refresh_all_fences() {
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

pub(crate) fn create_fence_window(s: &mut UiState, fence_id: u32, hosts: &[HostInfo]) -> bool {
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

pub(crate) fn save_wallpaper_cache(caps: &[render::WallpaperPixels]) {
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
pub(crate) fn invalidate_wallpaper() {
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
    let Some(surf_ref) = s.surfaces.get(&fence_id) else {
        // 防御(2026-09-08):上方 needs_new 分支正常已保证表面存在;万一未来
        // 路径破坏该不变式,跳过本帧留痕即可,不 panic 整个进程(图标还在
        // 隐藏态,进程一死用户看到的就是"程序凭空消失")
        log(&format!("draw skip: surface missing fence {fence_id}"));
        return;
    };
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
        // 正在就地重命名的成员:标签由编辑框替代(与原生一致)
        FILE_RENAME_PATH.lock().unwrap().as_deref(),
        // 入场动画中的成员:落地前不在栅栏里露脸(先落在桌面格,再飞入)
        &s.arrival_animations
            .iter()
            .map(|a| a.path.clone())
            .collect::<Vec<String>>(),
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

pub(crate) fn refresh_fence(fence_id: u32) {
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

pub(crate) fn ensure_fence_window(id: u32) {
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
        None,
        &[],
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
pub(crate) fn ensure_missing_category_fences(s: &mut UiState) -> Vec<String> {
    let mut have: std::collections::HashSet<String> = s
        .fences
        .iter()
        .filter(|f| !f.category.is_empty())
        .map(|f| f.category.clone())
        .collect();
    let mut added = Vec::new();
    if auto_category() {
        // 动态分类表(2026-09-08):按可编辑表补建;表里新增的空分类因无
        // 文件不会在此建栏(由面板"新增"显式建),已删分类不再迭代=不复活
        for cat_def in model::category_table() {
            let cat: &str = &cat_def.name;
            if have.contains(cat) {
                continue;
            }
            if s.files.iter().any(|f| f.category == cat) {
                // 用户手动删过的分类在墓碑期内不复活;该类出现**新文件**
                // (mtime 晚于删除时刻)才清除墓碑并补建
                if let Some(ts) = category_tombstone_at(cat) {
                    let has_newer = s
                        .files
                        .iter()
                        .any(|f| f.category == cat && f.mtime_ms > ts);
                    if has_newer {
                        clear_category_tombstone(cat);
                        log(&format!("category fence '{cat}' resurrected by newer file"));
                    } else {
                        continue;
                    }
                }
                added.push(cat.to_string());
                have.insert(cat.to_string());
            }
        }
    } else if !have.contains(model::UNCATEGORIZED) {
        // 自定义分类模式:未分配文件都进"未分类",保证没有任何文件隐身
        added.push(model::UNCATEGORIZED.to_string());
    }
    let mut new_ids = Vec::new();
    for cat in &added {
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        new_ids.push(max_id);
        // 尺寸按该类当前内容数收窄:不足 5 项宽 1 列(2026-09-02 用户要求)
        let count = s.files.iter().filter(|f| &f.category == cat).count();
        s.fences.push(Fence {
            id: max_id,
            title: cat.clone(),
            category: cat.clone(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: {
                let (dw, dh) = default_size_for_items(count);
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: dw,
                    h: dh,
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
    // 新栅栏落位:第一行最后一个栅栏右侧,靠顶对齐,保持默认间隔
    // (与手动新建同一规则 new_fence_rect,2026-09-02;旧的左上角落位会
    // 压到占住左上角的既有栅栏)
    for id in &new_ids {
        let Some((w, h)) = s
            .fences
            .iter()
            .find(|f| f.id == *id)
            .map(|f| (f.rect.w, f.rect.h))
        else {
            continue;
        };
        let rect = new_fence_rect(s, w, h);
        if let Some(fence) = s.fences.iter_mut().find(|f| f.id == *id) {
            fence.rect = rect;
        }
    }
    added
}

/// 桌面文件变更刷新
/// 空分类栅栏自动移除(2026-09-04 用户要求):分类成员走光(改名换类/
/// 删除)后栅栏不再占位,右侧栅栏经 delete_fence_ex 的行内左移补位。
/// 只处理分类栅栏(category 非空;手动新建的空栏是用户预留,不自动删);
/// 不记墓碑。逐个走 delete_fence_ex(含 undo/左移/落盘),须在 state 锁外调用。
fn remove_empty_category_fences() {
    let empty_ids: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences
            .iter()
            .filter(|f| !f.category.is_empty())
            .filter(|f| model::display_list(f, &s.files).is_empty())
            .map(|f| f.id)
            .collect()
    };
    for id in empty_ids {
        log(&format!("auto-removed empty category fence {}", id));
        delete_fence_ex(id, false);
    }
}

pub fn rescan() {
    let mut files = with_recycle_bin(shell::scan_desktop());
    let (added_paths, removed_any, recat_any, gained_cats) = {
        let s = state().lock().unwrap();
        // 扫描宽恕:上一轮在册、本轮扫不到的路径,连续 SCAN_MISS_DROP 轮
        // 才真正移除(未达阈值时从上一轮找回,保持文件可见)——元数据瞬态
        // 读取失败不再引发"文件消失/栅栏重排"(用户实测"文档自动移位")
        {
            let mut miss = scan_miss_map().lock().unwrap();
            let present: std::collections::HashSet<String> =
                files.iter().map(|f| f.path.clone()).collect();
            for f in s.files.iter() {
                if present.contains(&f.path) {
                    miss.remove(&f.path);
                } else {
                    let c = miss.entry(f.path.clone()).or_insert(0);
                    if *c < SCAN_MISS_DROP {
                        files.push(f.clone());
                    }
                }
            }
            miss.retain(|k, _| present.contains(k) || s.files.iter().any(|f| f.path == *k));
        }
        let added = model::newly_added_paths(&s.files, &files);
        let new_set: std::collections::HashSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();
        let removed = s.files.iter().any(|f| !new_set.contains(f.path.as_str()));
        // 分类漂移也算变化(2026-09-03):同名文件的分类变了(改名内存同步
        // 后、或将来分类规则调整)不能走"无变化早退"——早退会跳过
        // ensure_missing_category_fences,改名成 mp4 的文件永远留在文档栏
        let mut old_cats: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for f in s.files.iter() {
            old_cats.insert(f.path.as_str(), f.category.as_str());
        }
        let recat = files
            .iter()
            .any(|f| old_cats.get(f.path.as_str()).is_some_and(|&c| c != f.category));
        // 有成员"迁入"的分类(2026-09-03):外部改名/分类规则调整导致某文件
        // 分类变化,与在应用内改名同权——清该分类墓碑,否则墓碑挡住缺类补
        // 建,迁入成员无栏可归=隐身
        let mut gained: Vec<String> = Vec::new();
        for f in files.iter() {
            if let Some(old) = old_cats.get(f.path.as_str()) {
                if *old != f.category && !gained.contains(&f.category) {
                    gained.push(f.category.clone());
                }
            }
        }
        (added, removed, recat, gained)
    };
    if added_paths.is_empty() && !removed_any && !recat_any {
        if !RENAME_RESCAN_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
            // 文件集合没有任何变化:桌面目录的文件系统事件(Explorer 的元数据
            // 触碰、菜单交互的伴生事件)不值得做任何重绘。此前的无条件
            // show_all_fences 让每次 watcher dirty 都全量重绘 5 个栅栏,
            // 表现为点桌面/关菜单后栅栏区域闪一下。
            return;
        }
        // 改名提交强制走一遍:补建缺类栅栏+收敛,尽管 diff 为空
    }
    let new_cats: Vec<String> = {
        let mut s = state().lock().unwrap();
        s.files = files;
        // 桌面变更可能同时改变快捷方式目标、文件关联或 Shell overlay 状态。
        // 缓存键还包含像素尺寸，因此扫描时统一失效可避免保留陈旧图标。
        let keep: std::collections::HashSet<String> =
            s.files.iter().map(|f| f.path.clone()).collect();
        s.icon_cache.clear();
        // 已删文件的常用记录同步剔除(2026-09-03):usage.json 残留旧路径时,
        // 同名新建会继承旧次数直接顶到"常用"第一位(用户实测)
        let pruned = model::prune_usage(&keep);
        if pruned > 0 {
            log(&format!("pruned {pruned} stale usage entries"));
        }
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
        // 迁入分类的墓碑清理必须在补建之前(否则墓碑挡路,迁入成员隐身)
        for cat in &gained_cats {
            if category_tombstone_at(cat).is_some() {
                clear_category_tombstone(cat);
                log(&format!(
                    "category '{cat}' tombstone cleared by incoming member"
                ));
            }
        }
        let new_cats_inner = ensure_missing_category_fences(&mut s);
        // 到达顺序登记进 item_order(2026-09-04):新出现/迁入的成员追加到
        // 所在栅栏拖拽顺序表末尾,排序按"先来在左、后来靠右"——迁移来的
        // 文件不再因旧 mtime 排到最前面
        // MutexGuard 的 Deref 不支持字段级分裂借用:先克隆文件列表
        let files_snapshot = s.files.clone();
        for fence in s.fences.iter_mut() {
            let items = model::display_list(fence, &files_snapshot);
            let missing: Vec<String> = items
                .iter()
                .filter(|it| !fence.item_order.contains(&it.path))
                .map(|it| it.path.clone())
                .collect();
            fence.item_order.extend(missing);
        }
        new_cats_inner
    };
    // 栅栏创建已全部收口在 ensure_missing_category_fences 内部(单一创建
    // 来源)。此前这里还有第二个创建循环——rescan 路径每个新分类会建出
    // 两个同名栅栏(2026-09-02 修"mp3 一来冒出两个媒体")。
    // 只有真的新增了分类栅栏才收敛；周期 rescan 不应把用户手动摆放的
    // 位置重排回左上角（推挤式保留相对位置 + 行贴顶归一，而不是流式重排）。
    if !new_cats.is_empty() {
        settle_preserve_positions();
    }
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    rebuild_pins();
    refit_auto_fence_heights();
    // 空分类栅栏自动移除(右侧左移补位)——先于 show_all_fences,避免
    // 空栏闪现;不记墓碑,该类再来文件时缺类补建照常重建
    remove_empty_category_fences();
    // 先排队入场动画再渲染栅栏(2026-09-03):栅栏帧按 hide_arrivals 跳过
    // 飞行中成员的墨水——新文件"先落在桌面格、飞入落地后才在栅栏显形";
    // 若先渲染后排队,文件会瞬间出现在栅栏里,动画沦为重复影子
    start_arrival_animations(&added_paths);
    show_all_fences();
}

/// 默认栅栏尺寸:2 列宽 × 5 行高(用户指定;内容超出自动滚动)
pub(crate) fn default_fence_size() -> (f32, f32) {
    let (title_h, pad) = model::chrome(model::dpi_scale());
    (
        model::cell_w() * 2.0 + pad * 2.0 + 2.0,
        title_h + model::cell_h() * 4.0 + pad * 2.0 + 2.0,
    )
}

/// 按内容数给默认尺寸(2026-09-02 用户要求):不足 5 项宽 1 列,≥5 项宽 2 列;
/// 高固定 4 行。与首次运行布局(build_global_config)同一规则
pub(crate) fn default_size_for_items(n: usize) -> (f32, f32) {
    let cols = if n < 5 { 1usize } else { 2usize };
    let (title_h, pad) = model::chrome(model::dpi_scale());
    (
        model::cell_w() * cols as f32 + pad * 2.0 + 2.0,
        title_h + model::cell_h() * 4.0 + pad * 2.0 + 2.0,
    )
}

/// 新栅栏落位(2026-09-02 统一规则,手动/自动新建共用):第一行最后一个
/// 栅栏右侧,与其靠顶对齐、保持默认间隔;无栅栏时放工作区左上角;
/// 行尾放不下夹回屏内(残余重叠由随后的 settle 推开兜底)。
pub(crate) fn new_fence_rect(s: &UiState, w: f32, h: f32) -> Rect {
    let visible: Vec<Rect> = s
        .fences
        .iter()
        .filter(|f| !f.hidden && !f.collapsed)
        .map(|f| f.rect)
        .collect();
    let rows = model::rows_from_rects(&visible);
    let (x, y) = match rows.first() {
        Some(row) if !row.is_empty() => {
            let last = &visible[row[row.len() - 1]];
            (last.x + last.w + model::GAP, last.y)
        }
        _ => {
            let (vx, vy, _, _) = work_area();
            (vx, vy)
        }
    };
    let r = Rect { x, y, w, h };
    let (vx, vy, vw, vh) = work_area_for_rect(&r);
    let mut tmp = [r];
    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
    tmp[0]
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

/// 定位能展示 path 的栅栏及其可见槽位的图标屏幕坐标(图标左上角)。
/// auto_scroll=槽位在滚动页外时把栅栏滚到该行(新建入场用);false=页外
/// 直接返回 None(迁移动画抓旧栏起点用,不应为起飞而滚动旧栏)。
fn fence_slot_screen_pos(
    s: &mut UiState,
    path: &str,
    auto_scroll: bool,
) -> Option<(u32, (f32, f32))> {
    let item = s.files.iter().find(|item| item.path == path)?.clone();
    let mut fence = s
        .fences
        .iter()
        .find(|fence| {
            fence.pinned.contains(&item.path)
                || fence.category.is_empty()
                || fence.category == item.category
        })?
        .clone();
    if fence.hidden || fence.collapsed {
        return None;
    }
    let items = model::display_list(&fence, &s.files);
    let index = items.iter().position(|c| c.path == item.path)?;
    let metrics = s
        .metrics
        .get(&fence.id)
        .copied()
        .unwrap_or_else(model::DpiMetrics::system);
    let mut layout = model::layout_with_metrics(&fence, items.len(), &metrics);
    if index < layout.first_index || index >= layout.first_index + layout.visible {
        // 新条目落在滚动页外(2026-09-03):"常用"排序下新文件使用次数为
        // 零只能排最后,内容超一页的栅栏(如文档 2×4=8 槽)会把它排进
        // 第二页——旧逻辑这里静默跳过,新文件既无动画也看不见落在哪
        // (用户实测"新建没动画"的真因)。改为把栅栏滚到该条目所在行,
        // 让飞入落点可见;连滚都滚不到(不可能:行数≤总行数)才放弃。
        if !auto_scroll {
            return None;
        }
        let cols = layout.cols.max(1);
        let row = index / cols;
        let max_scroll = layout.total_rows.saturating_sub(layout.rows);
        if max_scroll == 0 {
            return None;
        }
        let scroll = row.min(max_scroll);
        fence.scroll_rows = scroll;
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence.id) {
            f.scroll_rows = scroll;
        }
        layout = model::layout_with_metrics(&fence, items.len(), &metrics);
        if index < layout.first_index || index >= layout.first_index + layout.visible {
            return None;
        }
        log(&format!(
            "arrival: scrolled fence {} to row {} for off-page item",
            fence.id, scroll
        ));
    }
    let (cell_x, cell_y) = model::cell_pos_with_metrics(&layout, index, &metrics);
    let icon_offset = (metrics.cell_w - metrics.icon_px) / 2.0;
    Some((
        fence.id,
        (
            fence.rect.x + cell_x + icon_offset,
            fence.rect.y + cell_y + 4.0,
        ),
    ))
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
        let Some((fence_id, to)) = fence_slot_screen_pos(&mut s, path, true) else {
            continue;
        };
        let metrics = s
            .metrics
            .get(&fence_id)
            .copied()
            .unwrap_or_else(model::DpiMetrics::system);
        // 新文件先"落在桌面空白处"(围栏外的原生网格位),停留片刻再飞入栅栏;
        // 找不到围栏外空位时回退为从栅栏标题中心飞出
        let from = desktop_free_slot(&s, &mut used_slots).unwrap_or_else(|| {
            let (fx, fy, fw) = s
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| (f.rect.x, f.rect.y, f.rect.w))
                .unwrap_or((0.0, 0.0, 200.0));
            (
                fx + fw * 0.5 - metrics.icon_px * 0.5,
                (fy + metrics.title_h * 0.5 - metrics.icon_px * 0.5).max(0.0),
            )
        });
        pending.push(ArrivalAnimation {
            fence_id,
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
    for p in &pending {
        log(&format!(
            "arrival: animate '{}' fence={} from=({:.0},{:.0}) to=({:.0},{:.0})",
            p.name, p.fence_id, p.from.0, p.from.1, p.to.0, p.to.1
        ));
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

/// 跨栏迁移动画(2026-09-03):改名改扩展名(如 mp3→md)导致分类变化时,
/// 从旧栏旧槽位飞向新栏新槽位——与新建入场动画共用同一 overlay 管线。
/// moves: (路径, 旧栅栏id, 旧槽位屏幕x, y)。必须在 rescan 之后调用:
/// 旧栏已不含该成员、新栏帧已渲染;排队后刷新新栏把成员藏到落地
/// (hide_arrivals),飞行由 TIMER_ANIMATION 驱动,落地由 tick 显形。
fn queue_migration_animations(moves: &[(String, u32, f32, f32)]) {
    if moves.is_empty() || !client_area_animations_enabled() {
        return;
    }
    let now = resize_now_ms();
    let mut refresh_ids: Vec<u32> = Vec::new();
    {
        let mut s = state().lock().unwrap();
        let mut queued_any = false;
        for (path, old_fence, fx, fy) in moves {
            let Some((fence_id, to)) = fence_slot_screen_pos(&mut s, path, true) else {
                continue;
            };
            if fence_id == *old_fence {
                continue;
            }
            let name = s
                .files
                .iter()
                .find(|i| &i.path == path)
                .map(|i| render::display_name(&i.name))
                .unwrap_or_default();
            log(&format!(
                "arrival: migrate '{}' fence {}->{} from=({:.0},{:.0}) to=({:.0},{:.0})",
                name, old_fence, fence_id, fx, fy, to.0, to.1
            ));
            s.arrival_animations.push(ArrivalAnimation {
                fence_id,
                path: path.clone(),
                name,
                from: (*fx, *fy),
                to,
                // 旧栏位置短暂停留(150ms)再起飞,飞行距离跨栏更远,不加停留
                started_ms: now + 150,
                duration_ms: 520,
            });
            if !refresh_ids.contains(&fence_id) {
                refresh_ids.push(fence_id);
            }
            queued_any = true;
        }
        if !queued_any {
            return;
        }
        ensure_guide_window(&mut s);
        refresh_guide(&mut s);
    }
    // 锁外刷新:refresh_fence 内部要拿 state 锁
    for id in refresh_ids {
        refresh_fence(id);
    }
    if let Some(tray) = TRAY_HWND.get().copied() {
        unsafe {
            let _ = SetTimer(tray, TIMER_ANIMATION, 16, None);
        }
    }
}

fn tick_arrival_animations() {
    let landed: Vec<u32>;
    {
        let mut s = state().lock().unwrap();
        if s.arrival_animations.is_empty() {
            return;
        }
        // 已落地(动画到期)的成员:记下栅栏,refresh_guide 的 retain 清掉
        // 它们之后逐栏刷新——栅栏此前按 hide_arrivals 跳过其墨水,落地即显形
        let now = resize_now_ms();
        landed = s
            .arrival_animations
            .iter()
            .filter(|a| now.saturating_sub(a.started_ms) > a.duration_ms + 180)
            .map(|a| a.fence_id)
            .collect();
        ensure_guide_window(&mut s);
        refresh_guide(&mut s);
        if s.arrival_animations.is_empty() {
            if let Some(tray) = TRAY_HWND.get().copied() {
                unsafe {
                    let _ = KillTimer(tray, TIMER_ANIMATION);
                }
            }
            if s.drag_ghost.is_none() {
                if let Some(hwnd) = s.guide_hwnd {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
            }
        }
    }
    // 锁外刷新:refresh_fence 内部要拿 state 锁,持锁重入必死锁
    for fence_id in landed {
        refresh_fence(fence_id);
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
        // 启动即清理已删文件的常用记录(2026-09-03):应用关闭期间删的文件
        // 同样会在 usage.json 留残账,同名新建继承旧次数顶到常用第一位
        {
            let keep: std::collections::HashSet<String> =
                s.files.iter().map(|f| f.path.clone()).collect();
            let pruned = model::prune_usage(&keep);
            if pruned > 0 {
                log(&format!("pruned {pruned} stale usage entries at boot"));
            }
        }
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
// ---------------- 托盘图标 ----------------

pub(crate) static DESKTOP_ICONS_HIDDEN: AtomicBool = AtomicBool::new(false);
/// User explicitly requested native desktop icons to remain visible.
static NATIVE_DESKTOP_OVERRIDE: AtomicBool = AtomicBool::new(false);
/// 纯净态:用户主动"隐藏全部栅栏"——栅栏与原生图标都隐藏,桌面只剩壁纸。
/// 图标协调逻辑在此状态下不因"无栅栏呈现"而恢复原生图标(那正是旧的
/// "隐藏栅栏=回到原生桌面"重复感的来源)。仅在本次运行内生效,重启回正常态。
pub(crate) static ZEN_MODE: AtomicBool = AtomicBool::new(false);

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
pub(crate) fn desktop_shell_window() -> Option<HWND> {
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
    let orphans;
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
pub(crate) fn reconcile_desktop_icons() {
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

pub(crate) fn toggle_desktop_icons() {
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

pub(crate) fn restore_desktop_icons() {
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
pub(crate) fn push_undo() {
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

pub(crate) fn undo_layout() {
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
            update_overlay();
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
                update_overlay();
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
        // 主动删除:扫描宽恕立即放行,删除当轮即生效
        mark_scan_removed(&delete_paths);
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

pub(crate) fn uninstall_keyboard_hook() {
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

pub(crate) fn uninstall_mouse_hook() {
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

pub(crate) fn restore_original_desktop() {
    ZEN_MODE.store(false, Ordering::Relaxed);
    set_desktop_state_stored("native");
    let _ = restore_desktop_now();
    set_all_hidden(true);
    log("returned to original desktop without changing files or icon layout");
}

pub(crate) fn set_all_hidden(hidden: bool) {
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

// ---------------- 重命名 ----------------

static RENAME_OLD_PROC: std::sync::OnceLock<isize> = std::sync::OnceLock::new();
static INTENTIONAL_HIDE: AtomicBool = AtomicBool::new(false);

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

const FILE_RENAME_COMMIT_MSG: u32 = WM_USER + 3;
/// 重命名框行数自适应定时器(120ms):IME 提交不产生 WM_CHAR,纯消息钩子
/// 会漏掉中文输入——定时器兜底覆盖一切改动来源。id 取高位避开 EDIT 内部
/// 小整数定时器 id(互不覆盖)
const RENAME_FIT_TIMER: usize = 0x4DF5;
const WM_IME_COMPOSITION: u32 = 0x010F;
const FILE_RENAME_CANCEL_MSG: u32 = WM_USER + 4;
static FILE_RENAME_OLD_PROC: OnceLock<isize> = OnceLock::new();
static FILE_RENAME_PATH: Mutex<Option<String>> = Mutex::new(None);
/// 改名提交后置位:rescan 的"无变化早退"必须跳过一次。改名提交已把内存
/// 文件列表同步到新路径/新分类,磁盘扫描结果与内存全一致,任何 diff 都看
/// 不出变化——但新分类缺栅栏时 ensure_missing_category_fences 必须跑一遍,
/// 否则改名成 mp4 的文件无栅栏可归=隐身(2026-09-03 实测"改名后不再自动
/// 建分类栅栏"的根因:内存同步把变化对 rescan 藏住了)
static RENAME_RESCAN_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ---------------- 扫描宽恕(2026-09-03) ----------------
/// 路径→连续扫描未遇次数。刚消失的文件连续 SCAN_MISS_DROP 轮扫不到才真正
/// 移除:新建/写入中的文件元数据可能被创建方进程短暂锁住,单轮扫描漏掉
/// 就把在册文件当"消失"会引发栅栏重排、位置漂移(用户实测"文档自动移位")。
/// 应用主动删除的路径用 mark_scan_removed 立即达阈值,不拖尾巴。
const SCAN_MISS_DROP: u32 = 2;
fn scan_miss_map() -> &'static Mutex<std::collections::HashMap<String, u32>> {
    static M: OnceLock<Mutex<std::collections::HashMap<String, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn mark_scan_removed(paths: &[String]) {
    let mut miss = scan_miss_map().lock().unwrap();
    for p in paths {
        miss.insert(p.clone(), SCAN_MISS_DROP);
    }
}

// ---------------- 双击打开延迟执行(2026-09-04 兼顾两种手势) ----------------
// DBLCLK 先登记"待打开"而不立即执行;随后的 UP 判定:与上一次图标 UP 的
// 间隔 > 系统双击时长 = 慢双击改名意图(取消打开、进入改名);否则执行
// 打开。快速双击=打开、慢双击=改名,互不误伤,与原生同款时序语义。
fn pending_open() -> &'static Mutex<Option<String>> {
    static M: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}
fn last_icon_up_ms() -> &'static std::sync::atomic::AtomicU64 {
    static M: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    M.get_or_init(|| std::sync::atomic::AtomicU64::new(0))
}
#[link(name = "user32")]
unsafe extern "system" {
    fn GetDoubleClickTime() -> u32;
}

/// 系统菜单"重命名"拦截回调(shell.rs 在 init 时注册)
fn on_shell_rename_request(path: &str) {
    start_file_rename(path.to_string());
}

fn start_file_rename(path: String) {
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
        WM_TIMER if wparam.0 as usize == RENAME_FIT_TIMER => {
            // 120ms 兜底:任何来源(IME/粘贴/程序性)的文本变化都收敛
            adjust_rename_edit_height(hwnd);
            return LRESULT(0);
        }
        WM_CHAR | WM_PASTE => {
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
    log("file rename commit");
    let old_path = FILE_RENAME_PATH.lock().unwrap().clone().unwrap_or_default();
    let mut renamed = false;
    let mut migration: Option<(String, u32, f32, f32)> = None;
    if !old_path.is_empty() {
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
        // 与 Explorer 相同的非法字符集合;空名/原名不动
        let invalid = new_name.is_empty()
            || new_name == old_name
            || new_name
                .chars()
                .any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
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
    uninstall_rename_mouse_hook();
    unsafe {
        let _ = KillTimer(edit, RENAME_FIT_TIMER);
        let _ = DestroyWindow(edit);
    }
    if renamed {
        // 内存已同步,磁盘扫描与内存一致,rescan 的 diff 必为空——
        // 置强制位让缺类补建跑一遍(改名成 mp4 要能冒出媒体栅栏)
        RENAME_RESCAN_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    rescan();
    // 跨栏迁移动画必须在 rescan 之后排队:目标分类栅栏(可能新建)已就位、
    // 新栏帧已渲染,这里排队并刷新新栏把成员藏到落地显形
    if let Some((new_path, old_fid, fx, fy)) = migration {
        queue_migration_animations(&[(new_path, old_fid, fx, fy)]);
    }
}

fn cancel_file_rename(edit: HWND) {
    log("file rename cancel");
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
pub(crate) fn work_area() -> (f32, f32, f32, f32) {
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
pub(crate) fn all_work_areas() -> Vec<(f32, f32, f32, f32)> {
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

/// 自动档缩放后的邻居整理：锚(被缩放栅栏)同一行右侧的成员从锚右缘
/// 起按 GAP 依次右排,正下方同列的成员按 GAP 依次下排;越界夹回工作区。
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
    // 自由放置区:实时中心距最近行中心超过半高容差(至少 48)→ 无槽位。
    // 容差额外加一个 GAP:行间中线附近两侧行都恰好差半个 GAP,不加会
    // 留下一条竖直移动不出线的判定死区
    let half = rows
        .iter()
        .flat_map(|row| row.iter())
        .map(|&i| rects[i].h * 0.5)
        .fold(0f32, f32::max)
        .max(48.0)
        + model::GAP;
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
/// 用于自动档自由区与自由档;网格档(棋盘模式)不调用。
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

fn snap_drag(s: &UiState, fence_id: u32, nr: Rect) -> Rect {
    // 三档对齐的拖动吸附:
    // 网格档 = 只吸附图标格整数倍(棋盘模式,不磁吸邻居,2026-09-08);
    // 自由档 = 完全跟手,仅靠近邻居时磁吸到固定间距;
    // 自动档 = 不吸附(链式对齐实时保证间距)。
    if auto_align_on() {
        return nr;
    }
    let mut x = nr.x;
    let mut y = nr.y;
    if grid_align_on() {
        let (vx, vy, _, _) = work_area_for_rect(&nr);
        x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
        y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
    } else {
        // 靠近邻居 -> 磁吸到恰好 GAP 间距(优先于网格格点;仅自由档)
        let others: Vec<Rect> = s
            .fences
            .iter()
            .filter(|f| f.id != fence_id)
            .map(|f| f.rect)
            .collect();
        let probe = Rect { x, y, ..nr };
        let ((sx, sy), snapped) =
            model::snap_gap_to_neighbors(&probe, &others, model::SNAP_THRESHOLD * 1.5);
        if snapped {
            x = sx;
            y = sy;
        }
    }
    Rect { x, y, ..nr }
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

/// 绘制并显示 overlay：内部图标拖拽残影、新文件飞入动画、插入指示线。
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
        // 残影/动画/插入线都是屏幕坐标,换算到 overlay 本地坐标(虚拟桌面原点)
        let label_scale = frames
            .first()
            .and_then(|_| animation_meta.first())
            .and_then(|a| s.metrics.get(&a.fence_id))
            .or_else(|| s.active_fence.and_then(|id| s.metrics.get(&id)))
            .map(|m| m.scale)
            .unwrap_or_else(|| model::DpiMetrics::system().scale);
        let jobs = render::draw_guides(
            &surf.target,
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

/// 刷新 overlay(图标残影/飞入动画/插入线)；三者皆无时隐藏 overlay 窗口。
/// 拖拽各路径的每帧与收尾都必须调用一次,否则 overlay 残留旧帧浮在
/// 其他应用上方(2026-09-08 误删此泵导致,勿再删)。
fn update_overlay() {
    let mut s = state().lock().unwrap();
    if s.drag_ghost.is_none()
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

pub(crate) fn settle_all_fences() {
    let areas = all_work_areas();
    let mut s = state().lock().unwrap();
    push_settle(&mut s.fences, &areas);
}

/// settle 归一(P1 布局规范化)：① 行贴顶——同一行(可见集,与拖拽/删除
/// 槽位模型同口径)所有栅栏的 y 归一到该行最顶栅栏顶边;② 行间固定间隔
/// ——自上而下级联,下行顶=上行最深底+GAP,过近推下过远拉上(首行顶锚
/// 不动);③ 首行贴左——首行整体平移到行内最左栅栏所在工作区左缘,
/// 整排从屏幕左侧起步(行内间距保持);④ 两两收敛推挤解除重叠(级联已
/// 保证行间恰为 GAP,推挤只兜横向/夹回残冲突);⑤ 夹回屏幕。
/// 不改变栅栏顺序/行结构,行内相对 y 会归一。
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
    // 行规范化只作用于可见集:隐藏/折叠栅栏不参与行分组,也不会把历史
    // 位置的 y 带进来当行顶锚(与 fence_insertion_plan/delete_fence_ex
    // 的 !hidden && !collapsed 口径一致)。① 行贴顶;② 行间固定间隔
    // (级联,过近推下过远拉上);③ 首行贴左(锚=首行最左栅栏所在显示器
    // 的工作区左缘,整行平移保行内间距);新引入的残余冲突仍由下面的
    // 推挤与屏幕夹回兜底。
    let vis: Vec<usize> = (0..n)
        .filter(|&i| !fences[i].hidden && !fences[i].collapsed)
        .collect();
    let mut vis_rects: Vec<Rect> = vis.iter().map(|&i| rects[i]).collect();
    let aligned = model::align_rows_top(&mut vis_rects);
    let spaced = model::space_rows_gap(&mut vis_rects);
    let anchored = match vis_rects
        .iter()
        .min_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    {
        Some(leftmost) => {
            let (vxa, _, _, _) = work_area_for_rect(leftmost);
            model::align_first_row_left(&mut vis_rects, vxa)
        }
        None => false,
    };
    if aligned || spaced || anchored {
        for (k, &i) in vis.iter().enumerate() {
            rects[i] = vis_rects[k];
        }
    }
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

/// 周期性 rescan 用的收敛：行规范化(贴顶+行间 GAP+首行贴左) + 推挤 +
/// 夹回屏幕，除行内 y 归一与行距/首行锚定外保留用户手动摆放的相对
/// 位置，避免自动对齐模式下每 30 秒把所有栅栏流式重排回左上角
/// (与 settle_all_fences 当前同体,仅语义标注不同)。
pub(crate) fn settle_preserve_positions() {
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
                    // 网格档=只对齐最近格线(不吸附栅栏,2026-09-08 棋盘模式);
                    // 自由档=跟手+靠近邻居磁吸;自动档=原始跟手(插入线接管)
                    if !auto_align_on() {
                        let mut x = nr.x;
                        let mut y = nr.y;
                        if grid_align_on() {
                            let (vx, vy, _, _) = work_area_for_rect(&nr);
                            x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
                            y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
                        } else {
                            let others: Vec<Rect> = drag
                                .start_layout
                                .iter()
                                .filter(|f| f.id != fence_id)
                                .map(|f| f.rect)
                                .collect();
                            let probe = Rect { x, y, ..nr };
                            let ((sx, sy), snapped) = model::snap_gap_to_neighbors(
                                &probe,
                                &others,
                                model::SNAP_THRESHOLD * 1.5,
                            );
                            if snapped {
                                x = sx;
                                y = sy;
                            }
                        }
                        nr = Rect { x, y, ..nr };
                    }
                    let (vx, vy, vw, vh) = work_area_for_rect(&nr);
                    let mut tmp = [nr];
                    model::fit_to_screen(&mut tmp, vx, vy, vw, vh);
                    nr = tmp[0];
                    // 插入线/行槽落位=自动档专属(网格档棋盘化,2026-09-08)
                    let chain = auto_align_on();
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
                    // 无插入线:只做 ≤16px 的轻磁吸贴齐,不做抗重叠推挤;
                    // 网格档(棋盘模式)连轻磁吸也不做——格线是唯一对齐。
                    // (2026-09-08 修"向左拖吃力":P1 归一后布局恰为 GAP 紧排,
                    // 原位动 1px 就与邻居构成 GAP 冲突,预览被 avoid_overlap
                    // 按在原位不跟手;右向因槽位模型把自身按下位计入统计,
                    // 线立即出现走原始跟手,才显得"向右自然"。被拖者已提升
                    // 到兄弟之上,预览覆盖邻居无碍;解重叠由松手落位+settle
                    // 负责。)
                    if insert.is_none() && !grid_align_on() {
                        let others: Vec<Rect> = drag
                            .start_layout
                            .iter()
                            .filter(|f| f.id != fence_id && !f.hidden)
                            .map(|f| f.rect)
                            .collect();
                        snap_rect_to_neighbors(&mut nr, &others);
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
                    // 每帧刷新 overlay(插入线显示/收起都走这里)
                    update_overlay();
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
                    let nr = snap_drag(&s, fence_id, nr);
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
                        update_overlay();
                        return;
                    }
                    drop(s);
                    refresh_fence(fence_id);
                    update_overlay();
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
                        // 防御(2026-09-08):栅栏若在按住期间被删除/重建(分类
                        // 编辑、rescan 等路径),放弃本次拖拽而不是 panic 全进程
                        let (inside, paths) = {
                            let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
                                log("icon drag aborted: fence vanished mid-press");
                                s.drag = None;
                                drop(s);
                                return;
                            };
                            let inside =
                                x >= 0.0 && y >= 0.0 && x <= fence.rect.w && y <= fence.rect.h;
                            let items = model::display_list(fence, &s.files);
                            let pressed = items.get(idx).map(|it| it.path.clone());
                            let paths: Vec<String> = if pressed
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
                            };
                            (inside, paths)
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
                                let Some(fence) = s.fences.iter().find(|f| f.id == fence_id)
                                else {
                                    log("icon drag aborted: fence vanished mid-press");
                                    s.drag = None;
                                    drop(s);
                                    return;
                                };
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
pub(crate) fn resize_now_ms() -> u64 {
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
    let now_ms = resize_now_ms();
    // 交换出"上一次图标 UP"的时间:本 UP 与它的间隔=双击/慢击判定依据
    let prev_up_ms = last_icon_up_ms().swap(now_ms as u64, Ordering::Relaxed) as i64;
    let mut s = match state().try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(drag) = s.drag.take() {
        let mut changed_final: Vec<u32> = Vec::new();
        // Move 拖拽真实位移过:松手后要跑一次 settle 归一(P1 触发时机)
        let mut drop_move_settle = false;
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
                    mark_scan_removed(&paths);
                    rescan();
                } else {
                    refresh_fence(fence_id);
                }
                update_overlay();
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
                    mark_scan_removed(&paths);
                    rescan();
                } else {
                    refresh_fence(fence_id);
                }
                update_overlay();
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
            update_overlay();
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
                let slow_rename = now_ms as i64 - prev_up_ms > unsafe { GetDoubleClickTime() } as i64;
                if drag.icon_was_selected
                    && !ctrl
                    && !moved
                    && slow_rename
                    && !drag.icon_path.is_empty()
                    && !model::is_recycle_bin(&drag.icon_path)
                {
                    let path = drag.icon_path.clone();
                    s.marquee = None;
                    // 慢双击改名:取消 DBLCLK 登记的待打开(否则文件误打开)
                    *pending_open().lock().unwrap() = None;
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
                    // 插入线/行槽落位=自动档专属;网格档(棋盘模式)松手按
                    // 网格取整自由放置(走下面 else 分支)
                    let plan = if moved && auto_align_on() {
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
                        // 网格档=只对齐最近格线;自由档=跟手+邻居磁吸(同预览)
                        if !auto_align_on() {
                            let mut x = nr.x;
                            let mut y = nr.y;
                            if grid_align_on() {
                                let (vx, vy, _, _) = work_area_for_rect(&nr);
                                x = vx + ((nr.x - vx) / model::cell_w()).round() * model::cell_w();
                                y = vy + ((nr.y - vy) / model::cell_h()).round() * model::cell_h();
                            } else {
                                let others: Vec<Rect> = drag
                                    .start_layout
                                    .iter()
                                    .filter(|f| f.id != fence_id)
                                    .map(|f| f.rect)
                                    .collect();
                                let probe = Rect { x, y, ..nr };
                                let ((sx, sy), snapped) = model::snap_gap_to_neighbors(
                                    &probe,
                                    &others,
                                    model::SNAP_THRESHOLD * 1.5,
                                );
                                if snapped {
                                    x = sx;
                                    y = sy;
                                }
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
                        // 网格档(棋盘模式)不吸附栅栏,格线是唯一对齐;轻磁吸
                        // 只属于自动档自由区/自由档
                        if !grid_align_on() {
                            snap_rect_to_neighbors(&mut fr, &others);
                        }
                        fr = model::avoid_overlap(&fr, &others, vx, vy, vw, vh);
                        if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                            f.rect = fr;
                        }
                    }
                    s.insert_line = None;
                    changed_final = s.fences.iter().map(|f| f.id).collect();
                    drop_move_settle = moved;
                }
                DragMode::Resize { edges } => {
                    // 用户手动缩放：此后高度不再自动收敛到内容（尊重用户意图）
                    if let Some(f) = s.fences.iter_mut().find(|f| f.id == fence_id) {
                        f.manual_size = true;
                    }
                    let chars: Vec<char> = edges.iter().filter(|c| **c != '\0').copied().collect();
                    let nr0 = model::apply_resize(&drag.start_rect, &chars, dx, dy);
                    let nr0 = snap_drag(&s, fence_id, nr0);
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
        if drop_move_settle {
            // 拖动落位后的 settle 归一(P1 触发时机"拖动后"):行贴顶+推挤+
            // 夹回一次跑完。Move 分支 changed_final 已是全体 id,归一动到
            // 的栅栏会随下面的刷新一并重渲染。settle 自取状态锁,必须在
            // drop(s) 之后调用;归一后的最终矩形才是应持久化的位置。
            settle_all_fences();
            let s = state().lock().unwrap();
            let cfg = s.fences.clone();
            let _ = model::save_config(&cfg);
        }
        if changed_final.is_empty() {
            refresh_fence(fence_id);
        } else {
            for id in changed_final {
                refresh_fence(id);
            }
        }
        // 松手后刷新 overlay(插入线/残影已清,无内容即隐藏)并清除框选矩形
        update_overlay();
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
    // DBLCLK 登记的待打开在此无条件执行(2026-09-04):快速双击时系统以
    // DBLCLK 替代第二次 DOWN,UP 时拖拽已被首次 UP 取走——执行点若放在
    // 拖拽块内,快速双击的打开永远不会发生,直到后续点击才补开(用户实测
    // "打不开/很久才有反应")。打开走工作线程:ShellExecute 启动播放器
    // 会被安全软件扫描,同步执行会挂住 UI 线程(用户实测"卡死")。
    if let Some(p) = pending_open().lock().unwrap().take() {
        std::thread::spawn(move || open_item(&p));
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
            // 延迟执行:是否真打开由随后的 UP 判定(快双击=执行打开;
            // 慢双击=取消打开进入改名)。见 pending_open 注释。
            *pending_open().lock().unwrap() = Some(p);
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
        mark_scan_removed(&paths);
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

#[cfg(test)]
mod rename_geom_tests {
    use super::*;
    use windows::core::PCWSTR;

    /// 实测 adjust 几何:同款字体+文本创建 EDIT,调用 adjust 后回读
    /// 宽/高/首行内容,验证换行点与原生一致( ignored:会创建真实窗口 )。
    #[test]
    #[ignore]
    fn debug_adjust_wrap_geometry() {
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
            let cls = crate::shell::wide("EDIT");
            let edit = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR::from_raw(cls.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0 | WS_BORDER.0 | 0xC5),
                60,
                60,
                111,
                54,
                HWND(0),
                HMENU(0),
                hinstance(),
                None,
            );
            assert!(edit.0 != 0);
            let lf = crate::shell::icon_title_logfont().unwrap();
            let font = CreateFontIndirectW(&lf);
            let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            let name = "v4flash测试.txt";
            let w16 = crate::shell::wide(name);
            let _ = SetWindowTextW(edit, PCWSTR::from_raw(w16.as_ptr()));
            // 模拟真实选中(0..basename)
            let _ = SendMessageW(edit, 0x00B1 /*EM_SETSEL*/, WPARAM(0), LPARAM(name.encode_utf16().count() as isize - 4));
            adjust_rename_edit_height(edit);
            let mut rc = RECT::default();
            let _ = GetWindowRect(edit, &mut rc);
            let lines = SendMessageW(edit, 0x00BA, WPARAM(0), LPARAM(0)).0;
            let mut first = [0u16; 64];
            first[0] = 63;
            let _ = SendMessageW(edit, 0x00C1 /*EM_GETLINE*/, WPARAM(0), LPARAM(first.as_mut_ptr() as isize));
            let mut dump = String::new();
            for li in 0..lines {
                let mut b = [0u16; 128];
                b[0] = 127;
                let _ = SendMessageW(edit, 0x00C1, WPARAM(li as usize), LPARAM(b.as_mut_ptr() as isize));
                let n = SendMessageW(edit, 0x00C1, WPARAM(li as usize), LPARAM(b.as_mut_ptr() as isize)).0 as usize;
                dump.push_str(&format!("L{}='{}' ", li, String::from_utf16_lossy(&b[0..n.min(126)])));
            }
            println!(
                "ADJ-RESULT: outer={}x{} lines={} {}",
                rc.right - rc.left,
                rc.bottom - rc.top,
                lines,
                dump
            );
            let _ = DestroyWindow(edit);
        }
    }
}

