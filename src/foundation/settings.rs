//! 设置访问层(2026-09-16 从 ui.rs 原样搬出,纯搬家不改行为):
//! settings.json 的进程内缓存读取 + 部分更新落盘的统一入口(对齐档位/渲染
//! 模式/桌面状态/自动分类/z 守卫/常显边框/界面语言/分类墓碑等开关)。
//! 叶子模块——只依赖 std::sync / crate::model / crate::lang,不依赖 crate::ui
//! (ui.rs 上帝文件按依赖方向拆分的第二刀,读写设置的模块应直接依赖本模块)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::model;

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

/// 自动分类开关(2026-09-09 起单一真相=model 的线程局部缓存,boot 预热;
/// false=自定义分类模式:文件只进被拖入的栅栏,未归位文件进兜底"其他")
pub fn auto_category() -> bool {
    model::auto_category()
}
pub(crate) fn set_auto_category_stored(v: bool) {
    model::set_auto_category(v);
    update_stored_settings(|s| s.auto_category = v);
}

/// z 守卫设置(缓存读取,模式同上):菜单落盘点需要带上当前值。
/// 不设菜单入口(2026-09-08 用户要求):异常降级开关走手改 settings.json
/// 的 z_guard 字段,默认开=实测验证过的正确状态。
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

/// 常显栅栏边框线(托盘开关,默认关=悬停/拖拽才浮现,2026-09-01 用户新增):
/// 开=全部栅栏常显边框/标题/角手柄,便于观察布局边界;关=无边框常显基线。
/// pub(crate):ui 的 boot 预热直接写本静态(只读盘不写盘,故不经 set_show_chrome_stored)。
pub(crate) static SHOW_CHROME: AtomicBool = AtomicBool::new(false);

pub fn chrome_always_on() -> bool {
    SHOW_CHROME.load(Ordering::Relaxed)
}
pub(crate) fn set_show_chrome_stored(on: bool) {
    SHOW_CHROME.store(on, Ordering::Relaxed);
    update_stored_settings(|s| s.show_chrome = on);
}

/// 界面语言设置("auto"/"zh"/"en",默认 auto,2026-09-11):有效语言缓存在
/// lang::EFFECTIVE 原子量,启动预热;切换时同步改原子量+落盘。菜单每次
/// 现建、面板每次现开,查表即时生效,无需重启。
pub fn lang_setting_value() -> String {
    model::load_settings().lang
}
pub(crate) fn set_lang_stored(v: &str) {
    crate::lang::set_effective(crate::lang::resolve(v, crate::lang::system_prefers_zh()));
    update_stored_settings(|s| s.lang = v.to_string());
}

/// 分类栅栏删除墓碑:删除时刻 epoch ms。墓碑在位的分类不再被缺类补建
/// 复活,除非之后出现该类的新文件(mtime 晚于墓碑)——那时清除墓碑并
/// 正常补建,保留"首次出现该类文件会自动新建"的原设计。
pub(crate) fn category_tombstone_at(cat: &str) -> Option<u64> {
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

/// 精确模式是否生效(用户开启)。动态壁纸检测已退役(2026-08-26):ink 常驻
/// 后背景实时透出、阴影背景无关,动态壁纸不再构成降级理由;两模式共用同一
/// 条 seeded 文字管线,区别仅剩启动守卫(精确模式等首帧种子就绪再呈现)。
pub fn precise_mode_on() -> bool {
    render_mode() == "precise"
}
