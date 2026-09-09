//! 菜单子系统(2026-09-08 从 ui.rs 原样搬出,纯搬家不改行为):
//! 托盘菜单/栅栏倒三角菜单/桌面右键子菜单的构建、弹出(track)与命令分发。
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::render;
use crate::model::{self, Fence, Rect};
use crate::shell;
use crate::drag::*;
use crate::ui::*;

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
const MENU_TOGGLE_CHROME: u32 = 0x511A;
// 管理分类子菜单(2026-09-08 分类面板):表项=base+表内下标,上限 16 项
const MENU_CATS_BASE: u32 = 0x5120;
const MENU_CATS_ADD: u32 = 0x5130;
const MENU_CHECK_UPDATE: u32 = 0x5131;
const MENU_MODE_CUSTOM: u32 = 0x5133;

pub(crate) fn show_tray_menu(x: i32, y: i32) {
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
    // 自动分类(2026-09-08 用户定案):与"渲染模式"同款单一入口。子菜单
    // 首段=模式二选一(与精确/透明同款交互,点击即切换生效);分隔线之下
    // 类别清单归自动模式——点击分类名进面板就地改名,底部"新增分类…"直接
    // 建空分类;删除用面板行内 ×(承载面板见 cats_panel.rs)。父项对钩
    // 显示当前是否自动模式。
    let auto = unsafe { CreatePopupMenu().unwrap_or_default() };
    // 自定义(拖入归类)在上、自动分类(按类型归类)在其下(2026-09-09 用户
    // 定案):分隔线之后紧贴的类别清单一眼可知归属自动分类;类别行用全角
    // 空格缩进,与两个模式项拉开层次。
    if auto_category() {
        shell::append_menu(auto, MENU_MODE_CUSTOM, "自定义(拖入归类)");
    } else {
        shell::append_menu_checked(auto, MENU_MODE_CUSTOM, "自定义(拖入归类)");
    }
    shell::append_separator(auto);
    if auto_category() {
        shell::append_menu_checked(auto, MENU_AUTO_CATEGORY, "自动分类(按类型归类)");
    } else {
        shell::append_menu(auto, MENU_AUTO_CATEGORY, "自动分类(按类型归类)");
    }
    let table = model::category_table();
    let shown = table.len().min((MENU_CATS_ADD - MENU_CATS_BASE) as usize);
    for (i, c) in table.iter().take(shown).enumerate() {
        let label = if c.name == model::FALLBACK_CATEGORY {
            format!("{}(兜底)", c.name)
        } else {
            c.name.clone()
        };
        shell::append_menu(auto, MENU_CATS_BASE + i as u32, &format!("　{label}"));
    }
    shell::append_separator(auto);
    shell::append_menu(auto, MENU_CATS_ADD, "　新增分类…");
    if auto_category() {
        shell::append_submenu_checked(menu, "自动分类", auto);
    } else {
        shell::append_submenu(menu, "自动分类", auto);
    }
    if chrome_always_on() {
        shell::append_menu_checked(menu, MENU_TOGGLE_CHROME, "显示栅栏边框线");
    } else {
        shell::append_menu(menu, MENU_TOGGLE_CHROME, "显示栅栏边框线");
    }
    // z 序守卫不设菜单入口(2026-09-08 用户要求):降级开关走 settings.json
    // 的 z_guard 字段,默认开=实测验证过的正确状态。
    // 检查更新:浏览器打开 GitHub Releases 页(应用进程零联网)
    shell::append_menu(
        menu,
        MENU_CHECK_UPDATE,
        &format!("检查更新(v{})", env!("CARGO_PKG_VERSION")),
    );
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
        MENU_AUTO_CATEGORY => set_category_mode(true),
        MENU_MODE_CUSTOM => set_category_mode(false),
        MENU_CATS_ADD => crate::cats_panel::open_panel(None, true),
        id if (MENU_CATS_BASE..MENU_CATS_ADD).contains(&id) => {
            // 点击分类名:打开面板并把该行置为编辑焦点(超出表长视为陈旧菜单)
            let idx = (id - MENU_CATS_BASE) as usize;
            crate::cats_panel::open_panel(Some(idx), false);
        }
        MENU_TOGGLE_CHROME => {
            let on = !chrome_always_on();
            set_show_chrome_stored(on);
            refresh_all_fences();
            log(&format!("show_chrome={on}"));
        }
        MENU_CHECK_UPDATE => check_update(),
        MENU_AUTOSTART => toggle_autostart(),
        MENU_QUIT => quit_app(),
        _ => log(&format!("unknown tray command: {}", id)),
    }
}

/// 设置渲染模式并立即生效(作废壁纸快照,全部栅栏重绘)
pub(crate) fn set_render_mode(mode: &str) {
    set_render_mode_stored(mode);
    invalidate_wallpaper();
    refresh_all_fences();
    log(&format!("render_mode={mode}"));
}

/// 设定分类模式:开=自动归类(按类型);关=自定义分类(文件只进被拖入的
/// 栅栏,未归位文件由 display_list 收进兜底"其他"或旧未分类栅栏)。幂等:
/// 模式已是目标值时不做任何事(菜单二选一可能点当前项)。
fn set_category_mode(v: bool) {
    if auto_category() == v {
        return;
    }
    set_auto_category_stored(v);
    log(&format!("auto_category={v}"));
    // 切模式不再自动建"未分类"栅栏(2026-09-09 用户要求):未归位文件由
    // display_list 统一收进兜底"其他"(或旧未分类栅栏),不冒出多余栅栏
    rebuild_pins();
    settle_preserve_positions();
    refresh_all_fences();
}

/// 分类改名(管理面板回调):同步分类表(缓存+落盘)、栅栏(category+title)、
/// 文件归属。重名/空名拒绝;不改布局几何,仅全量重渲染。
pub(crate) fn apply_category_rename(old: &str, new: &str) -> bool {
    let new = new.trim();
    if new.is_empty() || new == old {
        return false;
    }
    if model::category_table().iter().any(|c| c.name == new) {
        log(&format!("category rename rejected, '{new}' already exists"));
        return false;
    }
    let mut table = model::category_table();
    for c in table.iter_mut() {
        if c.name == old {
            c.name = new.to_string();
        }
    }
    model::set_category_table(table.clone());
    update_stored_settings(|s| {
        s.categories = table;
        s.deleted_category_at.remove(old); // 改名后旧墓碑键无意义,顺带清理
    });
    {
        let mut s = state().lock().unwrap();
        for f in s.files.iter_mut() {
            if f.category == old {
                f.category = new.to_string();
            }
        }
        for f in s.fences.iter_mut() {
            if f.category == old {
                f.category = new.to_string();
                // 标题跟随仅当与旧分类名相同(用户自定义过的标题不覆盖,
                // 2026-09-09 体检 E7.1:两个方向行为对称)
                if f.title == old {
                    f.title = new.to_string();
                }
            }
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    rebuild_pins();
    refresh_all_fences();
    log(&format!("category renamed '{old}' -> '{new}'"));
    true
}

/// 分类规则(扩展名映射)编辑(管理面板回调,兜底不可改):归一小写去点
/// 去重;与其他分类冲突(同扩展名被占用)则整体拒绝;生效后全部文件归属
/// 即时重算,新匹配出成员的分类自动补建栅栏。
pub(crate) fn apply_category_exts(name: &str, exts: Vec<String>) -> bool {
    if name == model::FALLBACK_CATEGORY {
        return false;
    }
    let mut clean: Vec<String> = Vec::new();
    for e in &exts {
        let e = e.trim().trim_start_matches('.').to_lowercase();
        if e.is_empty() || clean.contains(&e) {
            continue;
        }
        if model::category_table()
            .iter()
            .any(|c| c.name != name && c.exts.contains(&e))
        {
            log(&format!("exts rejected: '{e}' already owned by another category"));
            return false;
        }
        clean.push(e);
    }
    let mut table = model::category_table();
    for c in table.iter_mut() {
        if c.name == name {
            c.exts = clean.clone();
        }
    }
    model::set_category_table(table.clone());
    update_stored_settings(|s| s.categories = table);
    {
        let mut s = state().lock().unwrap();
        for f in s.files.iter_mut() {
            f.category = model::categorize(&f.name, f.is_dir);
        }
    }
    {
        let mut s = state().lock().unwrap();
        let new_cats = ensure_missing_category_fences(&mut s);
        if !new_cats.is_empty() {
            settle_preserve_positions();
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    rebuild_pins();
    refresh_all_fences();
    log(&format!("category '{name}' exts -> {:?}", clean));
    true
}

/// 分类删除(管理面板回调,兜底"其他"不可删):分类移出表(之后缺类补建
/// 不再迭代=不复活),其文件全部重归兜底"其他"——不变式:任何文件不隐身。
/// 栅栏走 delete_fence_ex(含撤销点/窗口销毁/行补洞);兜底栅栏若缺则补建
/// (先清其历史墓碑,防止重归文件无栏可归)。
pub(crate) fn apply_category_delete(name: &str) -> bool {
    if name == model::FALLBACK_CATEGORY {
        log("category delete rejected: fallback is not deletable");
        return false;
    }
    let fid = state()
        .lock()
        .unwrap()
        .fences
        .iter()
        .find(|f| f.category == name)
        .map(|f| f.id);
    if let Some(id) = fid {
        // 记墓碑(2026-09-09 与栅栏菜单删除路径语义对齐:"用户明确删除");
        // 面板"新增"同名时同样会清墓碑,不会阻碍恢复
        delete_fence_ex(id, true);
    }
    {
        let mut s = state().lock().unwrap();
        for f in s.files.iter_mut() {
            if f.category == name {
                f.category = model::FALLBACK_CATEGORY.to_string();
            }
        }
    }
    let mut table = model::category_table();
    table.retain(|c| c.name != name);
    model::set_category_table(table.clone());
    update_stored_settings(|s| s.categories = table);
    clear_category_tombstone(model::FALLBACK_CATEGORY);
    {
        let mut s = state().lock().unwrap();
        let new_cats = ensure_missing_category_fences(&mut s);
        if !new_cats.is_empty() {
            settle_preserve_positions();
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    rebuild_pins();
    refresh_all_fences();
    log(&format!("category deleted '{name}', members refiled to {}", model::FALLBACK_CATEGORY));
    true
}

/// 分类新增(管理面板回调):空扩展名+非目录类,不吸走任何现有文件;
/// 同名自动加序号;建同名栅栏(清历史墓碑=明确意图),文件靠拖入(pin)。
pub(crate) fn apply_category_add(base: &str) -> Option<String> {
    let base = base.trim();
    if base.is_empty() {
        return None;
    }
    let mut table = model::category_table();
    let mut name = base.to_string();
    let mut n = 2;
    while table.iter().any(|c| c.name == name) {
        name = format!("{base}{n}");
        n += 1;
    }
    table.push(model::CategoryDef {
        name: name.clone(),
        exts: vec![],
        dirs: false,
    });
    model::set_category_table(table.clone());
    update_stored_settings(|s| s.categories = table);
    clear_category_tombstone(&name);
    {
        let mut s = state().lock().unwrap();
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        // 新建分类=空内容:按 2026-09-02 规则不足 5 项宽 1 列(用户 2026-09-08
        // 重申),高固定 4 行;default_fence_size 是 2 列的通用默认,不适用
        let (dw, dh) = default_size_for_items(0);
        s.fences.push(Fence {
            id: max_id,
            title: name.clone(),
            category: name.clone(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: Rect {
                x: 0.0,
                y: 0.0,
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
        let rect = new_fence_rect(&s, dw, dh);
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == max_id) {
            f.rect = rect;
        }
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    rebuild_pins();
    refresh_all_fences();
    log(&format!("category added '{name}'"));
    Some(name)
}

/// 检查更新:系统浏览器打开 GitHub Releases 页,由用户比对最新版本。
/// 应用进程自身不发起网络请求——README"程序不联网"的承诺保持成立。
fn check_update() {
    shell::open_url("https://github.com/zghehehe/DeskFence/releases/latest");
    log("check update: releases page opened in browser");
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
            let fg = GetForegroundWindow();
            let mut fb = [0u16; 32];
            let fn_ = GetClassNameW(fg, &mut fb);
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

pub(crate) fn fence_menu(hwnd: HWND, fence_id: u32, x: i32, y: i32) {
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

pub fn add_fence_after(_base_id: u32) -> u32 {
    push_undo();
    let max_id = {
        let mut s = state().lock().unwrap();
        let max_id = s.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        // 手动新建为空栅栏:宽 1 列;落位=第一行末尾右侧+靠顶(统一规则)
        let (dw, dh) = default_size_for_items(0);
        let r = new_fence_rect(&s, dw, dh);
        s.fences.push(Fence {
            id: max_id,
            title: "新栅栏".into(),
            category: String::new(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: r,
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
    delete_fence_ex(fence_id, true);
}

/// tombstone=false:空栏自动移除用——不记墓碑,该类之后再来文件时缺类
/// 补建照常重建(空栏移除≠用户拒绝该分类)。
pub(crate) fn delete_fence_ex(fence_id: u32, tombstone: bool) {
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
    // 同行左移补洞(2026-09-02 行内槽位模型):先按删除前的行结构算出
    // "洞后成员各左移一格(接管前一成员的槽)"的分配,删除后应用——
    // 体感:删掉行中间/行首的栅栏,右侧成员自动左移补位
    let shift: Vec<(u32, (f32, f32))> = {
        let s = state().lock().unwrap();
        let ids: Vec<u32> = s
            .fences
            .iter()
            .filter(|f| !f.hidden && !f.collapsed)
            .map(|f| f.id)
            .collect();
        let rects: Vec<Rect> = ids
            .iter()
            .map(|id| {
                s.fences
                    .iter()
                    .find(|f| f.id == *id)
                    .map(|f| f.rect)
                    .unwrap()
            })
            .collect();
        let slot = |i: usize| (rects[i].x, rects[i].y);
        let rows = model::rows_from_rects(&rects);
        let own = ids
            .iter()
            .position(|id| *id == fence_id)
            .and_then(|pos| {
                rows.iter().enumerate().find_map(|(ri, row)| {
                    row.iter().position(|&i| i == pos).map(|j| (ri, j))
                })
            });
        let mut out = Vec::new();
        if let Some((ri, j)) = own {
            // 行锚点=该行(含被删者)最左成员;洞后成员按各自宽度+GAP 从锚点
            // 重排——宽度不同也不会重叠(旧的"接管前一槽"轮转在非等宽行
            // 必然重叠,已废弃)
            let anchor_i = rows[ri]
                .iter()
                .copied()
                .min_by(|&a, &b| {
                    rects[a]
                        .x
                        .partial_cmp(&rects[b].x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            let (ax, ay) = slot(anchor_i);
            let mut x = ax;
            for &t in rows[ri].iter().filter(|&&t| t != rows[ri][j]) {
                out.push((ids[t], (x, ay)));
                x += rects[t].w + model::GAP;
            }
        }
        out
    };
    let hwnd = {
        let mut s = state().lock().unwrap();
        clear_fence_interaction(&mut s, fence_id);
        // 分类栅栏(自动建的)删除记墓碑:缺类补建不再复活它,直到该类
        // 出现新文件(2026-09-02 修"删了的栅栏又冒出来")。
        // 空栏自动移除(tombstone=false)不记墓碑:空栏移除≠用户拒绝该分类,
        // 之后该类再来文件时应照常补建。
        if tombstone {
            if let Some(f) = s.fences.iter().find(|f| f.id == fence_id) {
                if !f.category.is_empty() {
                    set_category_tombstone(&f.category);
                    log(&format!(
                        "category fence '{}' deleted, tombstone set",
                        f.category
                    ));
                }
            }
        }
        let removed = s.windows.remove(&fence_id);
        s.metrics.remove(&fence_id);
        s.presented.remove(&fence_id);
        s.attached.remove(&fence_id);
        if let Some(sf) = s.surfaces.remove(&fence_id) {
            render::release_surface(sf);
        }
        s.fences.retain(|f| f.id != fence_id);
        for (id, (x, y)) in &shift {
            if let Some(f) = s.fences.iter_mut().find(|f| f.id == *id) {
                f.rect.x = *x;
                f.rect.y = *y;
            }
        }
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
    // 删除后的 settle 归一(P1 触发时机"删除后"):行贴顶+推挤+夹回。
    // 左移补洞只重排本行,其余行的历史错位在此一并归一;矩形有变的成员
    // (可能与 shift 重合,刷新幂等)补重渲染并落盘最终位置。
    let pre_settle: Vec<(u32, Rect)> = state()
        .lock()
        .unwrap()
        .fences
        .iter()
        .map(|f| (f.id, f.rect))
        .collect();
    settle_all_fences();
    let settled_moved: Vec<u32> = {
        let s = state().lock().unwrap();
        s.fences
            .iter()
            .filter(|f| pre_settle.iter().any(|(id, r)| *id == f.id && *r != f.rect))
            .map(|f| f.id)
            .collect()
    };
    if !settled_moved.is_empty() {
        let s = state().lock().unwrap();
        let cfg = s.fences.clone();
        let _ = model::save_config(&cfg);
    }
    // 左移补洞的成员重渲染(位置变了)
    for (id, _) in &shift {
        refresh_fence(*id);
    }
    for id in &settled_moved {
        if !shift.iter().any(|(sid, _)| sid == id) {
            refresh_fence(*id);
        }
    }
    finish_interaction_cleanup();
    reconcile_desktop_icons();
}

/// 离开 DeskFence 桌面模式：先恢复 Explorer 原生图标，再隐藏本程序窗口。
/// 不修改原始图标位置、文件或 Explorer 布局；用户可通过“显示全部栅栏”再次接管。

pub(crate) fn dispatch_desktop_command(id: u32) {
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
        shell::DL_CMD_REFRESH => rescan(),
        shell::DL_CMD_QUIT => quit_app(),
        _ => {}
    }
}

