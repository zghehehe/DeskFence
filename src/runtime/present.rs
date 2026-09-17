//! 呈现管线（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
//! 栅栏窗口创建（create_fence_window）、内容绘制与上屏（refresh_fence_impl/
//! refresh_fence/refresh_all_fences/present_fence_only）、渲染器预热
//! （warm_renderer_scratch）、呈现就绪判定（fence_needs_presentation）、
//! 精确模式首帧追赶定时器的武装（arm_wallpaper_catchup）。
//! 中层模块——依赖 state/hosts/metrics/settings/winids/logging/render/model/ole
//! （创建时的带内锚点取自 hosts::band_attach_anchor）。本模块不依赖
//! selfheal（2026-09-17 锚点解析族迁 hosts 后环已断）；selfheal 修复后回调
//! 本模块 refresh_fence 为单向边；ole 的拖入回调通向 drag
//! （ole→drag→present 小环,属既有结构）。
//! 捕获/壁纸调度与 show_all_fences 等编排仍在 ui.rs。

use std::sync::atomic::Ordering;

use windows::core::PCWSTR;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hosts::*;
use crate::logging::log;
use crate::metrics::*;
use crate::model::{self, Fence, Rect};
use crate::ole;
use crate::render;
use crate::settings::*;
use crate::state::*;
use crate::winids::*;

// ---------------- 呈现相关的定时器与闸门 ----------------

/// 壁纸追赶定时器:精确模式快照缺失时以 200ms 节奏重捕获,
/// 就绪后一次性整帧重绘,避免栅栏先出 D2D 文字帧再切换成 ClearType+阴影
pub(crate) const TIMER_WALLPAPER_CATCHUP: usize = 5;

/// 启动阶段首帧呈现日志只打一次的闸门
static BOOT_FIRST_PRESENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(crate) static WALLPAPER_CATCHUP_ARMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(crate) static WALLPAPER_CATCHUP_TRIES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// 武装壁纸追赶定时器(200ms)。幂等:已武装时直接返回,避免每次刷新
/// 重置计时周期导致永不触发。定时器到点在托盘窗口线程回调,与所有
/// 调用方同线程,无需加锁。
pub(crate) fn arm_wallpaper_catchup() {
    if WALLPAPER_CATCHUP_ARMED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(&tray) = TRAY_HWND.get() {
        // SAFETY: tray 是本进程托盘窗口（本函数与其定时器回调同线程）；
        // 纯定时器调用，无指针参数。
        if unsafe { SetTimer(Some(tray), TIMER_WALLPAPER_CATCHUP, 200, None) } != 0 {
            WALLPAPER_CATCHUP_ARMED.store(true, Ordering::Relaxed);
            WALLPAPER_CATCHUP_TRIES.store(0, Ordering::Relaxed);
        }
    }
}

// ---------------- 窗口创建 ----------------

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
    // 保持顶层分层 WS_POPUP,由桌面宿主持有,不是 WS_CHILD/SetParent。
    // owned popup 必须在 owner 之上,避免显示桌面后被再次压到壁纸下面。
    // 普通应用之下的上界仍由 band 就位维护;找不到宿主则延迟创建。
    let host = host_for_rect(&fence.rect, hosts);
    if host.is_none() {
        log(&format!("no desktop host yet, defer fence {}", fence_id));
        return false;
    }
    let Some(owner) = desktop_shell_window() else {
        return false;
    };
    let _zcreate = z_scope(ZIntent::Create);
    // SAFETY: class_name 是 winids 静态 NUL 宽串；owner=desktop_shell_window()
    // 现查的桌面宿主（owned 关系即本架构的核心不变式）；hinstance 是本进程
    // 模块；失败判空返回；z_scope 声明自家创建意图（放行 z 守卫）。
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
            Some(owner),
            None,
            Some(hinstance),
            None,
        )
        .unwrap_or_default()
    };
    if hwnd.0.is_null() {
        log(&format!("create window failed id={}", fence_id));
        return false;
    }
    // SAFETY(整块): hwnd 是刚创建的本进程栅栏窗口；GWLP_USERDATA 写入
    // fence_id（fence_wndproc 的身份判定）；register_drop_target 泄漏式注册
    // 随窗口生命周期（WM_DESTROY 时 RevokeDragDrop 配对）；insert_after
    // 只取 band_attach_anchor 的受限锚点（绝不 topmost/HWND_TOP），无锚点
    // 则本轮不定位、留待走查就位。
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, fence_id as isize);
        ole::register_drop_target(hwnd, fence_id);
        // owned 关系约束宿主下界;锚点限制应用上界,不能跨过最低可见应用窗。
        // 无安全锚点时不使用 HWND_TOP/topmost,留待下一轮就位。
        let insert_after = match host.and_then(|h| band_attach_anchor(h.hwnd, hwnd)) {
            Some(a) => Some(a),
            None => band_attach_anchor(owner, hwnd),
        };
        let mut attached = false;
        if let Some(after) = insert_after {
            attached = SetWindowPos(
                hwnd,
                Some(after),
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

pub(crate) fn ensure_fence_window(id: u32) {
    let hosts = desktop_hosts();
    let mut s = state().lock().unwrap();
    if !s.windows.contains_key(&id) {
        create_fence_window(&mut s, id, &hosts);
    }
}

// ---------------- 刷新与上屏 ----------------

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
        // SAFETY: hwnd 是本进程栅栏窗口；INTENTIONAL_HIDE 前后夹住，放行
        // 自家隐藏（z 守卫只拦外部操作）。
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
        // SAFETY: hwnd 是本进程栅栏窗口；z_scope(ZIntent::Show) 放行自家
        // 显示；SetWindowPos 带 NOZORDER|NOSIZE=纯位置+可见位修正。
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
    let fence_hovered = *s.fence_hover.get(&fence_id).unwrap_or(&false) || chrome_always_on();
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
            fence_id,
            t_draw,
            t_gdi,
            t_pres,
            items.len()
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
    let mut s = state().lock().unwrap();
    let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
        return;
    };
    let Some(hwnd) = s.windows.get(&fence_id).copied() else {
        return;
    };
    let Some(surface) = s.surfaces.get(&fence_id) else {
        return;
    };
    let ok = render::present_existing_surface(
        surface,
        hwnd,
        fence.rect.x.round() as i32,
        fence.rect.y.round() as i32,
    );
    if ok {
        s.presented.insert(fence_id);
    } else {
        s.presented.remove(&fence_id);
        log(&format!("present existing failed fence {fence_id}"));
    }
}

// ---------------- 渲染器预热 ----------------

/// 启动阶段第一次全量刷新的逐栅栏计时开关(启动结束关闭,避免常态刷日志)
pub(crate) static BOOT_VERBOSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// 用一次性小表面预热渲染管线:D2D 设备/画刷/壁纸位图创建、GDI 字体与
/// DrawShadowText 动态加载的首次使用合计可达 300-900ms。趁主线程等待
/// 后台 shell 预热 join 的空闲窗口先烧掉,真正首帧只剩纯绘制成本。
pub(crate) fn warm_renderer_scratch() {
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
    log(&format!(
        "boot renderer warm-up took {}ms",
        resize_now_ms() - t0
    ));
}

// ---------------- 呈现就绪判定 ----------------

pub(crate) fn fence_needs_presentation(hidden: bool, presented: bool, has_surface: bool) -> bool {
    !hidden && (!presented || !has_surface)
}

#[cfg(test)]
mod presentation_tests {
    use super::fence_needs_presentation;

    #[test]
    fn healthy_surface_does_not_depend_on_z_attachment() {
        assert!(!fence_needs_presentation(false, true, true));
    }

    #[test]
    fn missing_presentation_or_surface_is_recovered() {
        assert!(fence_needs_presentation(false, false, true));
        assert!(fence_needs_presentation(false, true, false));
        assert!(fence_needs_presentation(false, false, false));
    }

    #[test]
    fn hidden_fences_are_not_represented() {
        for presented in [false, true] {
            for has_surface in [false, true] {
                assert!(!fence_needs_presentation(true, presented, has_surface));
            }
        }
    }
}
