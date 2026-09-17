//! 全局 UI 状态与内嵌数据类型（2026-09-16 从 ui.rs/drag.rs/selfheal.rs 原样
//! 搬出，纯搬家不改行为）：UiState 结构体 + state() 全局锁访问器，以及
//! UiState 内嵌的数据类型（Drag/DragMode/InsertPlan/GhostPreview/ArrivalAnimation
//! 原在 drag.rs，WalkFault/WalkStrike 原在 selfheal.rs——类型随迁是为了让
//! 本模块保持叶子，行为函数仍各在原模块）。其后又归入同性质的原语：
//! 单调时基 resize_now_ms、z 序意图守卫 ZIntent/z_scope、INTENTIONAL_HIDE、
//! FILE_RENAME_PATH 等跨模块共享的少量运行时标记。
//! 叶子模块——只依赖 std / windows crate / crate::model / crate::render，
//! 不依赖 crate::ui；任何模块读写 UI 状态都应直接依赖本模块。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HFONT;

use crate::model::{self, Fence, FileItem, Hit, Rect};
use crate::render;
use crate::render::{IconBuffer, Renderer, Surface};
use crate::winids::SyncHandle;

// ---------------- 拖拽数据类型(自 drag.rs 迁入) ----------------

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
pub struct InsertPlan {
    /// 全体可见栅栏的新位置(逐 start_layout 可见成员,含被拖者)
    pub assign: Vec<(u32, (f32, f32))>,
    /// 被拖者落点
    pub land: (f32, f32),
    /// 指示线 (x, y, w, h)
    pub line: (f32, f32, f32, f32),
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
    pub(crate) fence_id: u32,
    /// 按下时的完整显示顺序(回滚与重排的基准)
    pub(crate) original: Vec<String>,
    /// 按下时的排序模式(预览期间切"手动",回滚时恢复)
    pub(crate) original_sort_mode: String,
    /// 被拖路径集合；重排时按它们在 original 中的相对顺序组成块
    pub(crate) dragged_paths: Vec<String>,
    /// 当前预览目标槽位（删除拖动块后的列表中，范围 0..=剩余项数）
    pub(crate) target: usize,
}

#[derive(Clone)]
pub(crate) struct ArrivalAnimation {
    pub(crate) fence_id: u32,
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) from: (f32, f32),
    pub(crate) to: (f32, f32),
    pub(crate) started_ms: u64,
    pub(crate) duration_ms: u64,
}

// ---------------- 走查故障类型(自 selfheal.rs 迁入) ----------------

/// 同一故障签名连续出现才累计拍数;恢复过渡期拦路者换窗即重置。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkFault {
    /// 可见外来窗先于栅栏出现在宿主之上
    Blocked { hwnd: isize, class: u64 },
    /// 走查到栈顶未找到:栅栏在宿主之下,首拍即修
    NotFoundTop,
    /// 走查预算耗尽:状态不明,只记日志不动手
    NotFoundBudget,
}

pub(crate) struct WalkStrike {
    pub(crate) fault: WalkFault,
    pub(crate) count: u32,
}

// ---------------- 全局状态(自 ui.rs 迁入) ----------------

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

/// 全局状态锁访问器。**锁中毒策略=快速失败（2026-09-17 拍板）**：
/// lock().unwrap() 遇毒 panic,由 lib.rs 的 panic hook 落盘 run.log 后进程
/// 退出,下次启动走崩溃恢复(icons 标记残留→先恢复原生图标)。不采用
/// into_inner 带毒继续——中毒意味着某线程在持锁途中 panic,状态可能
/// 半写,带毒继续的"界面看似正常"比快速失败更危险;单 UI 线程架构下
/// 中毒本就等价于致命伤。
pub(crate) fn state() -> &'static Mutex<UiState> {
    static S: SyncHandle<OnceLock<Mutex<UiState>>> = SyncHandle(OnceLock::new());
    S.get_or_init(|| {
        Mutex::new(UiState {
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

// ---------------- 运行时时基(自 drag.rs 迁入) ----------------

/// 单调毫秒时基(进程启动为 0):拖动/呈现节流、交互防抖、壁纸懒捕获等
/// 全部计时共用同一时基,勿混用 SystemTime(系统时钟回拨会打乱节流)。
pub(crate) fn resize_now_ms() -> u64 {
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

// ---------------- z 序意图守卫(自 selfheal.rs 迁入) ----------------

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

/// 我们主动隐藏栅栏时置位,避免 WM_WINDOWPOSCHANGING 的"防最小化"拦截
/// 误伤自家隐藏动作(refresh_fence_impl 隐藏分支置位,wndproc 读取)。
pub(crate) static INTENTIONAL_HIDE: AtomicBool = AtomicBool::new(false);

/// 正在就地重命名的文件路径:呈现层用它把该成员的标签换成编辑框(与原生一致)。
/// 由 rename.rs 写入,present.rs 读取。
pub(crate) static FILE_RENAME_PATH: Mutex<Option<String>> = Mutex::new(None);

// ---------------- 桌面接管状态标记(自 ui.rs 迁入) ----------------

pub(crate) static DESKTOP_ICONS_HIDDEN: AtomicBool = AtomicBool::new(false);
/// 纯净态:用户主动"隐藏全部栅栏"——栅栏与原生图标都隐藏,桌面只剩壁纸。
/// 图标协调逻辑在此状态下不因"无栅栏呈现"而恢复原生图标(那正是旧的
/// "隐藏栅栏=回到原生桌面"重复感的来源)。仅在本次运行内生效,重启回正常态。
pub(crate) static ZEN_MODE: AtomicBool = AtomicBool::new(false);

// ---------------- 交互时间标记(自 ui.rs 迁入) ----------------

/// 最近一次用户交互(菜单开/关、桌面点击)的时刻,毫秒时基。壁纸捕获在
/// 交互后 2.5s 内主动推迟:宿主在前台切换后的未稳定态下被 PrintWindow 强制
/// 重绘会闪 ±4% 亮度,稳态则无感。
pub static LAST_INTERACTION_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn mark_interaction() {
    LAST_INTERACTION_MS.store(resize_now_ms(), Ordering::Relaxed);
}

// ---------------- 扫描宽恕与扫描纪元(自 rename.rs/ui.rs 迁入) ----------------

/// 路径到连续扫描未遇次数。刚消失的文件连续 SCAN_MISS_DROP 轮扫不到才真正
/// 移除:新建/写入中的文件元数据可能被创建方进程短暂锁住,单轮扫描漏掉
/// 就把在册文件当"消失"会引发栅栏重排、位置漂移(用户实测"文档自动移位")。
/// 应用主动删除的路径用 mark_scan_removed 立即达阈值,不拖尾巴。
pub(crate) const SCAN_MISS_DROP: u32 = 2;
pub(crate) fn scan_miss_map() -> &'static Mutex<HashMap<String, u32>> {
    static M: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn mark_scan_removed(paths: &[String]) {
    // 应用主动删除=内存状态在扫描器背后变了:作废在途异步快照,防止它把
    // 已删路径当"新增"混回内存列表(见下方 SCAN_EPOCH)
    invalidate_pending_scans();
    let mut miss = scan_miss_map().lock().unwrap();
    for p in paths {
        miss.insert(p.clone(), SCAN_MISS_DROP);
    }
}

/// 扫描纪元:作废在途异步快照(改名提交/分类规则应用/主动删除时递增,
/// rescan 读取、apply 时不匹配即丢弃)。
static SCAN_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn invalidate_pending_scans() {
    SCAN_EPOCH.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn scan_epoch() -> u64 {
    SCAN_EPOCH.load(Ordering::Relaxed)
}

/// 重建"已收纳(pinned)"路径表(自定义分类模式的数据源)。
/// 布局或收纳关系变化后调用;放在 state 因为它是 state().fences 的
/// 派生缓存刷新,与锁同源。
pub(crate) fn rebuild_pins() {
    let s = state().lock().unwrap();
    model::rebuild_pinned_registry(&s.fences);
}
