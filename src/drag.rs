//! 拖拽/交互子系统(2026-09-08 从 ui.rs 原样搬出,纯搬家不改行为):
//! 鼠标按下/移动/松开、残影与插入线预览、缩放、框选、滚轮、双击、
//! 键盘微调、OLE 拖出/落入回调、settle 归一与邻居整理。
//! 本模块属于 ui.rs 拆分增量;与 ui.rs 双向依赖(同 crate 内合法)。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    TRACKMOUSEEVENT_FLAGS, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT,
    VK_UP,
};
// windows 0.52 未导出的 WinEvent 标志,按 WinUser.h 补定义
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::menu::{dispatch_desktop_command, fence_menu};
use crate::model::{self, Fence, Hit, Rect};
use crate::ole;
use crate::render;
use crate::selfheal::*;
use crate::shell;

use crate::rename::*;
use crate::ui::*;

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
    pub(crate) assign: Vec<(u32, (f32, f32))>,
    /// 被拖者落点
    pub(crate) land: (f32, f32),
    /// 指示线 (x, y, w, h)
    pub(crate) line: (f32, f32, f32, f32),
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

/// 方向键微调栅栏位置(光标悬停在栅栏上时生效;Ctrl = 1px 微调,否则按图标网格步进)
pub(crate) fn nudge_fence(fence_id: u32, vk: u32) {
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

pub(crate) fn dispatch_file_key(fence_id: u32, packed: usize) {
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
    Some(InsertPlan { assign, land, line })
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
pub(crate) fn ensure_guide_window(s: &mut UiState) {
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
pub(crate) fn refresh_guide(s: &mut UiState) {
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
    for (animation, icon) in animation_meta.iter().zip(animation_icons) {
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
pub(crate) fn update_overlay() {
    let mut s = state().lock().unwrap();
    if s.drag_ghost.is_none() && s.arrival_animations.is_empty() && s.insert_line.is_none() {
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
pub(crate) fn update_ghost(x: f32, y: f32) {
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
pub(crate) fn refit_auto_fence_heights() {
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
pub(crate) fn push_settle(fences: &mut [Fence], areas: &[(f32, f32, f32, f32)]) {
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
pub(crate) fn track_mouse_leave(hwnd: HWND) {
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

pub(crate) fn handle_mousemove(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
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
                                let Some(fence) = s.fences.iter().find(|f| f.id == fence_id) else {
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

pub(crate) fn handle_lbuttondown(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
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

pub(crate) fn is_edge(h: &Hit) -> bool {
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

pub(crate) fn edges_of(h: Hit) -> [char; 2] {
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
///
/// 返回是否发生了变化(调用方据此重绘);悬停在回收站上时不重排(松手即删除)。
pub(crate) fn update_ghost_preview(s: &mut UiState, fence_id: u32, x: f32, y: f32) -> bool {
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
pub(crate) fn rollback_ghost_preview(s: &mut UiState) {
    if let Some(prev) = s.ghost_preview.take() {
        if let Some(f) = s.fences.iter_mut().find(|f| f.id == prev.fence_id) {
            f.item_order = prev.original;
            f.sort_mode = prev.original_sort_mode;
        }
    }
}

/// 拖出释放点(屏幕坐标)是否落在某个非源栅栏的回收站图标上。
pub(crate) fn release_on_recycle_bin_screen(
    s: &UiState,
    sx: f32,
    sy: f32,
    source_fence: u32,
) -> bool {
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
            model::hit_test_with_metrics(fence, &lay, cx, cy, items.len(), &metrics)
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

/// DBLCLK 登记待打开时的光标位(UP 出口位移判定用,见 handle_lbuttonup 出口)
static PENDING_POS: Mutex<Option<(f32, f32)>> = Mutex::new(None);

pub(crate) fn handle_lbuttonup(_hwnd: HWND, fence_id: u32, x: f32, y: f32) {
    let now_ms = resize_now_ms();
    // 交换出"上一次图标 UP"的时间:本 UP 与它的间隔=双击/慢击判定依据
    let prev_up_ms = last_icon_up_ms().swap(now_ms, Ordering::Relaxed) as i64;
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
                if moved || drag.dragged_out {
                    // 拖动过=非打开意图:取消 DBLCLK 登记的待打开(2026-09-09
                    // 用户实测"图标移动后还打开文件"——第二次点击判成双击
                    // 登记待打开后按住拖动再松手,UP 出口的无条件执行会误开;
                    // 9/7 重构(1d007b4"UP 出口无条件执行")留下的洞)
                    *pending_open().lock().unwrap() = None;
                }
                let slow_rename =
                    now_ms as i64 - prev_up_ms > unsafe { GetDoubleClickTime() } as i64;
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
        // 拖拽结束使用与自愈相同的受限锚点归位,不得越过首个可见普通
        // 外来窗。传入实际 HWND 排除自身;没有安全锚点则保持当前位置。
        {
            let s = state().lock().unwrap();
            let hosts = desktop_hosts();
            let target = s
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| (f.hidden, f.rect));
            if let Some((hidden, rect)) = target {
                if !hidden {
                    if let Some(host) = host_for_rect(&rect, &hosts) {
                        if let Some(fh) = s.windows.get(&fence_id) {
                            if let Some(after) = band_attach_anchor(host.hwnd, *fh) {
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
    // 出口执行前先做位移判定:登记时光标位与当前光标差超 8px=登记后
    // 拖动过=移动意图,取消打开(不改判快/慢,只否决"拖动后误开")
    if let Some((px, py)) = *PENDING_POS.lock().unwrap() {
        let (cx, cy) = screen_cursor();
        if (cx - px) * (cx - px) + (cy - py) * (cy - py) > 64.0 {
            PENDING_POS.lock().unwrap().take();
            pending_open().lock().unwrap().take();
            return;
        }
    }
    PENDING_POS.lock().unwrap().take();
    if let Some(p) = pending_open().lock().unwrap().take() {
        std::thread::spawn(move || open_item(&p));
    }
}

pub(crate) fn handle_dblclk(fence_id: u32, x: f32, y: f32) {
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
            // 慢双击=取消打开进入改名)。见 pending_open 注释。登记时光标
            // 位一并记录:UP 时位移超阈值=登记后按住拖动过=移动意图,取消
            // (2026-09-09 重写:按 drag 状态判定的旧取消不可达——DBLCLK
            // 取代第二次 DOWN,末次 UP 无 drag 状态,审查项 E1)
            *PENDING_POS.lock().unwrap() = Some(screen_cursor());
            *pending_open().lock().unwrap() = Some(p);
        }
    }
}

pub(crate) fn handle_rbuttonup(hwnd: HWND, fence_id: u32, x: f32, y: f32) {
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
pub(crate) fn handle_wheel(fence_id: u32, delta: i32) {
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

pub(crate) fn handle_setcursor(hwnd: HWND, fence_id: u32) {
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
        if let Ok(hc) = LoadCursorW(None, PCWSTR::from_raw(cid as *const u16)) {
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

pub(crate) fn open_item(path: &str) {
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

// 双击判定用(原 ui.rs extern 块同款声明,拖拽侧独立持有)
extern "system" {
    pub(crate) fn GetDoubleClickTime() -> u32;
}
