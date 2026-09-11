//! 界面文案双语表(2026-09-11):zh=简体中文(历史语言) / en=English。
//! 只收用户可见的界面文字;run.log 诊断输出、分类名/栅栏标题等用户数据
//! (新建时的种子名除外)一律不翻译。
//!
//! 有效语言=设置 lang("auto"/"zh"/"en") + 系统 locale,启动时解析进
//! EFFECTIVE 原子量(见 ui::startup 预热);托盘"语言/Language"切换即改
//! 原子量+落盘。菜单每次现建、面板每次现开,查表天然即时生效,无需重启。
//!
//! 模块名取 lang 而非 str:避免与原语类型 str 混淆。
use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
}

/// 0=Zh 1=En;启动预热前默认中文(历史语言,老用户零感知)
static EFFECTIVE: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    if EFFECTIVE.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}
pub fn set_effective(l: Lang) {
    EFFECTIVE.store(if l == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

/// 设置值 + 系统是否简中 -> 有效语言。"auto"=系统安装语言是简中(0804)
/// 则中文,否则英文;显式 zh/en 覆盖一切;未知值按 auto 处理。
pub fn resolve(setting: &str, system_zh: bool) -> Lang {
    match setting {
        "zh" => Lang::Zh,
        "en" => Lang::En,
        _ => {
            if system_zh {
                Lang::Zh
            } else {
                Lang::En
            }
        }
    }
}

/// 系统安装语言是否简体中文(HKLM Nls\Language InstallLanguage 0804 判定,
/// 与原 shell::inject_rename_item 的判据同源)。读不到=非简中(按英文兜底)。
pub fn system_prefers_zh() -> bool {
    let key = crate::shell::wide(r"SYSTEM\CurrentControlSet\Control\Nls\Language");
    let val = crate::shell::wide("InstallLanguage");
    let mut buf = [0u16; 32];
    let mut sz = (buf.len() * 2) as u32;
    unsafe {
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_err()
        {
            return false;
        }
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let code = String::from_utf16_lossy(&buf[..end]);
    code.starts_with("08") && code.ends_with("04")
}

/// 双语文案表:每个条目生成一个 `pub fn name() -> &'static str`,
/// 两列文案并列一处,新增语言时改宏即可(编译期穷尽)。
macro_rules! bilingual {
    ($($name:ident => ($zh:expr, $en:expr)),* $(,)?) => {
        $(pub fn $name() -> &'static str {
            match lang() {
                Lang::Zh => $zh,
                Lang::En => $en,
            }
        })*
    };
}

bilingual! {
    // ── 托盘菜单 ──
    tray_show_all => ("显示全部栅栏", "Show all fences"),
    tray_hide_all => ("隐藏全部栅栏", "Hide all fences"),
    undo_layout => ("撤销上次布局调整", "Undo last layout change"),
    reset_layout => ("恢复默认布局", "Restore default layout"),
    show_desktop_icons => ("显示桌面图标", "Show desktop icons"),
    hide_desktop_icons => ("隐藏桌面图标", "Hide desktop icons"),
    restore_fence_desktop => ("恢复栅栏桌面", "Restore fence desktop"),
    restore_native_desktop => ("恢复原始桌面", "Restore original desktop"),
    align_submenu => ("对齐方式", "Alignment"),
    align_auto => ("自动对齐(固定间隔)", "Auto align (fixed spacing)"),
    align_grid => ("网格对齐(图标格倍数)", "Snap to icon grid"),
    align_free => ("自由移动(不受限)", "Free move (unrestricted)"),
    render_submenu => ("渲染模式", "Render mode"),
    render_precise => ("精确(与原生逐像素一致)", "Precise (pixel-identical to native)"),
    render_transparent => ("透明(兜底:动态壁纸不兼容时)", "Transparent (fallback: dynamic wallpapers)"),
    auto_cat_submenu => ("自动分类", "Auto sort"),
    mode_custom => ("自定义(拖入归类)", "Custom (drag to assign)"),
    mode_auto_category => ("自动分类(按类型归类)", "Auto sort by type"),
    add_category_item => ("新增分类…", "New category…"),
    toggle_chrome => ("显示栅栏边框线", "Always show fence borders"),
    check_update => ("检查更新", "Check for updates"),
    autostart => ("开机自启", "Start with Windows"),
    quit => ("退出", "Exit"),
    lang_submenu => ("语言", "Language"),
    lang_auto => ("跟随系统", "System default"),
    lang_zh => ("中文", "中文"),
    lang_en => ("English", "English"),
    // ── 栅栏倒三角菜单 ──
    new_fence => ("新建栅栏", "New fence"),
    rename => ("重命名", "Rename"),
    expand => ("展开", "Expand"),
    collapse => ("折叠", "Collapse"),
    unlock => ("解除锁定位置与大小", "Unlock position & size"),
    lock => ("锁定位置与大小", "Lock position & size"),
    sort_submenu => ("排序方式", "Sort by"),
    sort_freq => ("常用(默认)", "Most used (default)"),
    sort_time => ("时间(最近修改)", "Recently modified"),
    sort_name => ("名称", "Name"),
    sort_manual => ("手动(拖拽自定义)", "Manual (drag to arrange)"),
    delete_fence => ("删除栅栏", "Delete fence"),
    refresh => ("刷新", "Refresh"),
    // ── 桌面右键 DeskFence 子菜单 ──
    desktop_align_auto => ("对齐方式: 自动(固定间隔)", "Alignment: auto (fixed spacing)"),
    desktop_align_grid => ("对齐方式: 网格(图标格倍数)", "Alignment: snap to icon grid"),
    desktop_align_free => ("对齐方式: 自由移动", "Alignment: free move"),
    desktop_render_precise => ("渲染模式: 精确(壁纸底,与原生一致)", "Render: precise (native-like)"),
    desktop_render_transparent => ("渲染模式: 透明(动态壁纸兼容)", "Render: transparent (dynamic wallpaper)"),
    quit_deskfence => ("退出 DeskFence", "Exit DeskFence"),
    // ── 图标降级右键菜单(系统菜单链路不可用时的兜底) ──
    open => ("打开", "Open"),
    open_with => ("打开方式…", "Open with…"),
    copy => ("复制", "Copy"),
    open_location => ("打开所在位置", "Open file location"),
    delete => ("删除", "Delete"),
    properties => ("属性", "Properties"),
    rename_item => ("重命名(&M)", "Rename(&M)"),
    // ── 分类管理面板 ──
    cats_title => ("管理分类", "Manage categories"),
    cats_add_btn => ("＋ 新增分类", "＋ Add category"),
    dirs_marker => ("(目录)", "(dirs)"),
    // ── 新建用户数据的种子名(落盘后即用户数据,不再随语言变) ──
    new_category_base => ("新分类", "New category"),
    seed_desktop_title => ("桌面整理", "Desktop"),
    seed_new_fence_title => ("新栅栏", "New fence"),
}

/// 兜底分类名后缀(拼在分类名后):中文无空格贴合,英文留空格
pub fn fallback_suffix() -> &'static str {
    match lang() {
        Lang::Zh => "(兜底)",
        Lang::En => " (fallback)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_explicit_overrides_system() {
        assert_eq!(resolve("zh", false), Lang::Zh);
        assert_eq!(resolve("en", true), Lang::En);
    }

    #[test]
    fn resolve_auto_follows_system() {
        assert_eq!(resolve("auto", true), Lang::Zh);
        assert_eq!(resolve("auto", false), Lang::En);
    }

    #[test]
    fn resolve_unknown_setting_behaves_like_auto() {
        assert_eq!(resolve("", true), Lang::Zh);
        assert_eq!(resolve("bogus", false), Lang::En);
    }

    #[test]
    fn bilingual_pairs_are_non_empty() {
        // 抽查:表内条目两列都必须非空(防手滑删文案)
        assert!(!tray_show_all().is_empty());
        assert!(!quit_deskfence().is_empty());
        assert!(!fallback_suffix().is_empty());
    }
}
