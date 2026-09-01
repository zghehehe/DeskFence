//! 数据模型与纯逻辑层（不依赖 Win32，可独立单元测试）

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

// ---------- 布局常量 ----------
pub const TITLE_H: f32 = 26.0;
pub const PAD: f32 = 6.0;
pub const EDGE: f32 = 8.0;
pub const CORNER: f32 = 12.0;
pub const COLLAPSE_W: f32 = 42.0;
pub const SCROLLBAR_W: f32 = 5.0;

/// 栅栏之间的固定间距（新建、拖动、缩放都保持）
pub const GAP: f32 = 12.0;

/// 磁吸阈值：与对齐候选边距离小于该值时吸附过去
pub const SNAP_THRESHOLD: f32 = 12.0;

/// 拖动/缩放吸附网格：水平方向 1 个图标格宽，垂直方向 1 个图标格高
/// （栅栏位置与尺寸都按「1 个图标大小」的步长对齐）。
pub fn grid_x() -> f32 {
    cell_w()
}

pub fn grid_y() -> f32 {
    cell_h()
}

static ICON_SIZE: Mutex<f32> = Mutex::new(32.0);
static DPI_SCALE: Mutex<f32> = Mutex::new(1.0);
/// 图标格留白(逻辑像素,不含图标本身)。
/// 默认 43/54 对应原生 32px 图标格 75x86;运行时由注册表 IconSpacing 覆盖。
static CELL_PAD_X: Mutex<f32> = Mutex::new(43.0);
static CELL_PAD_Y: Mutex<f32> = Mutex::new(54.0);

/// Physical render/layout metrics owned by one fence window.
/// Live windows use explicit metrics so mixed-DPI fences do not share state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpiMetrics {
    pub dpi: u32,
    pub scale: f32,
    pub icon_px: f32,
    pub cell_pad_x: f32,
    pub cell_pad_y: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub label_h: f32,
    pub title_h: f32,
    pub pad: f32,
    pub edge: f32,
    pub corner: f32,
    pub collapse_w: f32,
    pub scrollbar_w: f32,
}

impl DpiMetrics {
    pub fn new(dpi: u32, icon_px: f32, pad_x: f32, pad_y: f32) -> Self {
        let scale = (dpi.max(96) as f32 / 96.0).max(1.0);
        let icon_px = icon_px.clamp(8.0, 256.0);
        Self {
            dpi: dpi.max(96),
            scale,
            icon_px,
            cell_pad_x: pad_x,
            cell_pad_y: pad_y,
            cell_w: icon_px + pad_x * scale,
            // IconVerticalSpacing includes the caption band. Keep the label
            // band explicit so rendering does not add another hidden gap.
            label_h: (pad_y * scale).clamp(24.0 * scale, 54.0 * scale),
            cell_h: icon_px + pad_y * scale,
            title_h: TITLE_H * scale,
            pad: PAD * scale,
            edge: EDGE * scale,
            corner: CORNER * scale,
            collapse_w: COLLAPSE_W * scale,
            scrollbar_w: SCROLLBAR_W * scale,
        }
    }

    pub fn system() -> Self {
        Self::new(
            (dpi_scale() * 96.0).round() as u32,
            icon_size(),
            *CELL_PAD_X.lock().unwrap(),
            *CELL_PAD_Y.lock().unwrap(),
        )
    }
}

/// 与桌面原生图标一致的尺寸（像素），默认 32
pub fn icon_size() -> f32 {
    *ICON_SIZE.lock().unwrap()
}

pub fn set_icon_size(v: f32) {
    if (8.0..=256.0).contains(&v) {
        *ICON_SIZE.lock().unwrap() = v;
    }
}

/// 系统 DPI 缩放(物理像素/96)。图标本体已经是物理像素,但格子留白必须随 DPI 缩放,
/// 否则高分屏上图标名的可用宽度会明显比原生桌面窄、名字更早被截断。
pub fn set_dpi_scale(v: f32) {
    if v > 0.0 {
        *DPI_SCALE.lock().unwrap() = v;
    }
}

pub fn dpi_scale() -> f32 {
    *DPI_SCALE.lock().unwrap()
}

/// 覆盖图标格留白:水平 = IconSpacing - 32,垂直 = IconVerticalSpacing - 32(逻辑像素)。
pub fn set_cell_pads(px: f32, py: f32) {
    if (16.0..=96.0).contains(&px) {
        *CELL_PAD_X.lock().unwrap() = px;
    }
    if (16.0..=96.0).contains(&py) {
        *CELL_PAD_Y.lock().unwrap() = py;
    }
}

/// 当前生效的图标格留白(逻辑像素)。初始化时由实测桌面格距覆盖，
/// 未实测成功时为注册表回退值。
pub fn cell_pads() -> (f32, f32) {
    (*CELL_PAD_X.lock().unwrap(), *CELL_PAD_Y.lock().unwrap())
}

/// chrome 常量按 DPI 缩放后的物理像素值。
/// 栅栏几何必须统一使用缩放后的值：layout 消费的是 DpiMetrics(缩放后)，
/// 构建尺寸时若用逻辑常量会差出一整行空白（150% 缩放下尤其明显）。
pub fn chrome(dpi: f32) -> (f32, f32) {
    (TITLE_H * dpi, PAD * dpi)
}

pub fn cell_w() -> f32 {
    icon_size() + *CELL_PAD_X.lock().unwrap() * dpi_scale()
}

pub fn cell_h() -> f32 {
    icon_size() + *CELL_PAD_Y.lock().unwrap() * dpi_scale()
}

pub fn min_w() -> f32 {
    let (_, pad) = chrome(dpi_scale());
    // 最小宽度 = 1 个图标格（用户要求可缩到只放一个图标）
    cell_w() + pad * 2.0 + 2.0
}

pub fn min_h() -> f32 {
    let (title_h, pad) = chrome(dpi_scale());
    title_h + cell_h() + pad * 2.0 + 2.0
}

/// Snap fence dimensions to the icon-cell grid. The content area (excluding
/// chrome) is rounded to the nearest integer number of cells, so the fence
/// always fits exactly N×M icons without wasted padding.
pub fn snap_fence_size(w: f32, h: f32) -> (f32, f32) {
    let (_, pad) = chrome(dpi_scale());
    let (title_h, _) = chrome(dpi_scale());
    let content_w = (w - pad * 2.0).max(0.0);
    let content_h = (h - title_h - pad * 2.0).max(0.0);
    let cols = ((content_w / cell_w()).round() as usize).max(1);
    let rows = ((content_h / cell_h()).round() as usize).max(1);
    (
        cols as f32 * cell_w() + pad * 2.0 + 2.0,
        title_h + rows as f32 * cell_h() + pad * 2.0 + 2.0,
    )
}

/// 插入式重排:按给定顺序从 (x0,y0) 起依次排放,同排累积宽度,放不下换行
/// (下一行 y = 本行最深底边 + GAP)。纯函数,宽度高度逐项给定。
/// 用于"插入线松手后整链紧凑":[1,2,3] 把 1 插到 2/3 之间 → 2,1,3 依次占位。
pub fn chain_positions(sizes: &[(f32, f32)], x0: f32, y0: f32, row_right: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut x = x0;
    let mut y = y0;
    let mut row_bottom = y0;
    let mut row_first = true;
    for &(w, h) in sizes {
        // 换行以工作区绝对右缘为准(x0 起步时行宽只剩 row_right-x0),
        // 否则链会排到屏幕外,后续夹回又引发推挤
        if !row_first && x + w > row_right {
            x = x0;
            y = row_bottom + GAP;
            row_bottom = y;
        }
        out.push((x, y));
        row_bottom = row_bottom.max(y + h);
        x += w + GAP;
        row_first = false;
    }
    out
}

/// 两个矩形是否相交
pub fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y
}

/// 两矩形（含间距 GAP）是否冲突
pub fn conflicts_gap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w + GAP && a.x + a.w + GAP > b.x && a.y < b.y + b.h + GAP && a.y + a.h + GAP > b.y
}

/// 两矩形之间的最小距离（负值 = 重叠深度）
#[allow(dead_code)] // 单元测试断言使用
pub fn gap_between(a: &Rect, b: &Rect) -> f32 {
    let dx = (a.x - (b.x + b.w)).max(b.x - (a.x + a.w));
    let dy = (a.y - (b.y + b.h)).max(b.y - (a.y + a.h));
    dx.max(dy)
}

/// 把 rect 与 anchor 之间的冲突解除，保持最小间距 GAP。
/// 在 右/左/下/上 四个推出量中选位移最小的一轴推出，
/// 因此无论拖动方向如何，都能自动向最近空闲侧让位（上下左右自适应）。
pub fn push_away(anchor: &Rect, rect: &Rect) -> Rect {
    let mut nr = *rect;
    if !conflicts_gap(anchor, rect) {
        return nr;
    }
    let push_r = anchor.x + anchor.w + GAP - rect.x; // 向右推出量（>0 表示需要右推）
    let push_l = rect.x + rect.w + GAP - anchor.x; // 向左推出量
    let push_d = anchor.y + anchor.h + GAP - rect.y; // 向下推出量
    let push_u = rect.y + rect.h + GAP - anchor.y; // 向上推出量
    let mut best = f32::MAX;
    let mut dx = 0.0;
    let mut dy = 0.0;
    if push_r < best {
        best = push_r;
        dx = push_r;
        dy = 0.0;
    }
    if push_l < best {
        best = push_l;
        dx = -push_l;
        dy = 0.0;
    }
    if push_d < best {
        best = push_d;
        dx = 0.0;
        dy = push_d;
    }
    if push_u < best {
        dx = 0.0;
        dy = -push_u;
    }
    nr.x += dx;
    nr.y += dy;
    nr
}

/// 链式推挤：以 rects[anchor] 为参照，把与其间距不足 GAP 的栅栏
/// 以最小位移推开并连锁传导；不移动 anchor 本身，其它栅栏之间
/// 同样保持 GAP，收敛于无重叠。
#[allow(dead_code)] // 保留给未来「自由模式」推挤，现由单测覆盖
pub fn push_chain(rects: &mut [Rect], anchor: usize) {
    for _ in 0..32 {
        let mut moved = false;
        let a = rects[anchor];
        for i in 0..rects.len() {
            if i == anchor {
                continue;
            }
            let old = rects[i];
            rects[i] = push_away(&a, &old);
            if rects[i] != old {
                moved = true;
            }
        }
        // 双向连锁：被推开的栅栏之间互相推挤，防止单向收敛不足
        for i in 0..rects.len() {
            if i == anchor {
                continue;
            }
            for j in 0..rects.len() {
                if j == anchor || j == i {
                    continue;
                }
                let old = rects[j];
                rects[j] = push_away(&rects[i], &old);
                if rects[j] != old {
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
}

// ---------- 拖拽插入落位(2026-09-02:行内槽位模型,纯几何可单测) ----------

/// 行带聚类:按 y 中心排序,中心间距 > 0.6*min(高)(至少 24) 开新带;
/// 带内按 x 升序。返回各带成员在输入中的下标,带序自上而下。
/// 任意层数通用(三层/四层…只是多几个带)。
pub fn rows_from_rects(rects: &[Rect]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by(|&a, &b| {
        (rects[a].y + rects[a].h * 0.5)
            .partial_cmp(&(rects[b].y + rects[b].h * 0.5))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for idx in order {
        let r = &rects[idx];
        let start_new = match rows.last() {
            Some(row) => {
                let prev = &rects[row[0]];
                let tol = 0.6 * prev.h.min(r.h).max(24.0);
                (r.y + r.h * 0.5) - (prev.y + prev.h * 0.5) > tol
            }
            None => true,
        };
        if start_new {
            rows.push(vec![idx]);
        } else {
            rows.last_mut().unwrap().push(idx);
        }
    }
    for row in rows.iter_mut() {
        row.sort_by(|&a, &b| {
            rects[a]
                .x
                .partial_cmp(&rects[b].x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    rows
}

/// 中心所在行(行 y 中心最近)与行内槽位(中心左侧成员数,0..=行长度)。
pub fn row_slot_of(rects: &[Rect], rows: &[Vec<usize>], center: (f32, f32)) -> (usize, usize) {
    let mut best_row = 0usize;
    let mut bd = f32::MAX;
    for (ri, row) in rows.iter().enumerate() {
        let mid = row.iter().map(|&i| rects[i].y + rects[i].h * 0.5).sum::<f32>()
            / row.len() as f32;
        let d = (center.1 - mid).abs();
        if d < bd {
            bd = d;
            best_row = ri;
        }
    }
    let k = rows
        .get(best_row)
        .map(|row| {
            row.iter()
                .filter(|&&i| rects[i].x + rects[i].w * 0.5 < center.0)
                .count()
        })
        .unwrap_or(0);
    (best_row, k)
}

/// 中心 y 到最近行中心的距离(拖远=自由放置区的判定输入)。
pub fn nearest_row_distance(rects: &[Rect], rows: &[Vec<usize>], y: f32) -> f32 {
    rows.iter()
        .map(|row| {
            let mid = row.iter().map(|&i| rects[i].y + rects[i].h * 0.5).sum::<f32>()
                / row.len() as f32;
            (y - mid).abs()
        })
        .fold(f32::MAX, f32::min)
}

/// 行内插入落位(纯几何)。rects 含被拖者(a_idx)。A 从所在行拔出插入
/// target=(行,位):受影响的两行按"行锚点x + 各自宽度 + GAP"行内重排
/// (锚点=该行原首成员的 x/y),行 y 取原顶;未涉及的行一个像素不动,
/// 尺寸全部保持各自——结构性保证不重叠、不塌行、不乱桌。
/// 返回(逐输入下标的新位置, A 的落点)。
pub fn row_insert_layout(
    rects: &[Rect],
    a_idx: usize,
    target: (usize, usize),
) -> (Vec<(f32, f32)>, (f32, f32)) {
    let rows = rows_from_rects(rects);
    let mut out: Vec<(f32, f32)> = rects.iter().map(|r| (r.x, r.y)).collect();
    let slot = |i: usize| (rects[i].x, rects[i].y);
    let (hr, hj) = rows
        .iter()
        .enumerate()
        .find_map(|(ri, row)| row.iter().position(|&i| i == a_idx).map(|j| (ri, j)))
        .unwrap_or((0, 0));
    let (tr, tk) = target;
    if (hr, hj) == (tr, tk) {
        return (out, slot(a_idx));
    }
    let flow = |out: &mut Vec<(f32, f32)>, members: &[usize], x0: f32, y0: f32| {
        let mut x = x0;
        for &i in members {
            out[i] = (x, y0);
            x += rects[i].w + GAP;
        }
    };
    if tr == hr {
        let row = &rows[hr];
        let mut order: Vec<usize> = row.clone();
        order.remove(hj);
        let k2 = (if tk > hj { tk - 1 } else { tk }).min(order.len());
        let (ax, ay) = row
            .iter()
            .find(|&&i| i != a_idx)
            .map(|&i| slot(i))
            .unwrap_or(slot(a_idx));
        order.insert(k2, a_idx);
        flow(&mut out, &order, ax, ay);
    } else {
        let rest: Vec<usize> = rows[hr].iter().copied().filter(|&i| i != a_idx).collect();
        if !rest.is_empty() {
            let (ax, ay) = slot(rest[0]);
            flow(&mut out, &rest, ax, ay);
        }
        let row_t = &rows[tr.min(rows.len() - 1)];
        let mut order: Vec<usize> = row_t.clone();
        let k2 = tk.min(order.len());
        order.insert(k2, a_idx);
        let (ax, ay) = slot(row_t[0]);
        flow(&mut out, &order, ax, ay);
    }
    let land = out[a_idx];
    (out, land)
}

/// 把栅栏组约束进工作区 [vx,vy,vw,vh]：
/// 先整体平移回区内（保持相对位置），再逐个夹回；
/// 不放大也不缩小尺寸，避免拖动时栅栏被越拖越小。
pub fn fit_to_screen(rects: &mut [Rect], vx: f32, vy: f32, vw: f32, vh: f32) {
    if rects.is_empty() || vw <= 0.0 || vh <= 0.0 {
        return;
    }
    // 1) 整体平移回工作区
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for r in rects.iter() {
        minx = minx.min(r.x);
        miny = miny.min(r.y);
        maxx = maxx.max(r.x + r.w);
        maxy = maxy.max(r.y + r.h);
    }
    let off_x = if minx < vx {
        vx - minx
    } else if maxx > vx + vw {
        (vx + vw - maxx).min(0.0)
    } else {
        0.0
    };
    let off_y = if miny < vy {
        vy - miny
    } else if maxy > vy + vh {
        (vy + vh - maxy).min(0.0)
    } else {
        0.0
    };
    if off_x != 0.0 || off_y != 0.0 {
        for r in rects.iter_mut() {
            r.x += off_x;
            r.y += off_y;
        }
    }
    // 2) 逐个夹回（保留尺寸）
    for r in rects.iter_mut() {
        if r.w > vw {
            r.w = vw;
        }
        if r.h > vh {
            r.h = vh;
        }
        if r.x < vx {
            r.x = vx;
        } else if r.x + r.w > vx + vw {
            r.x = vx + vw - r.w;
        }
        if r.y < vy {
            r.y = vy;
        } else if r.y + r.h > vy + vh {
            r.y = vy + vh - r.h;
        }
    }
}

/// 流式紧密布局（手机小组件式）：按 y 重叠分行、行内按 x 排序，
/// 然后左对齐 + 上对齐 + GAP 排列。原地修改坐标，返回重排后的索引顺序。
/// 只改坐标（必要时夹回工作区宽高），不改尺寸。
#[allow(dead_code)] // 保留给「一键自动排列」类功能，现由单测覆盖
pub fn flow_layout(rects: &mut [Rect], vx: f32, vy: f32, vw: f32, vh: f32) -> Vec<usize> {
    let n = rects.len();
    if n == 0 || vw <= 0.0 || vh <= 0.0 {
        return (0..n).collect();
    }
    // 索引按（中心 y, x）排序
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        let ya = rects[a].y + rects[a].h * 0.5;
        let yb = rects[b].y + rects[b].h * 0.5;
        ya.partial_cmp(&yb)
            .unwrap()
            .then(rects[a].x.partial_cmp(&rects[b].x).unwrap())
    });
    // 分行：y 范围重叠的归为一行（比「中心 y 差 > 全局阈值」更稳健，
    // 矮栅栏拖到矮栅栏下方也能正确换行）
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in idx {
        if let Some(row) = rows.last_mut() {
            let row_top = row.iter().map(|&j| rects[j].y).fold(f32::MAX, f32::min);
            let row_bottom = row
                .iter()
                .map(|&j| rects[j].y + rects[j].h)
                .fold(0.0f32, f32::max);
            let r = &rects[i];
            if r.y < row_bottom && r.y + r.h > row_top {
                row.push(i);
                continue;
            }
        }
        rows.push(vec![i]);
    }
    // 每行按 x 排序 + 流式排列（左对齐 + 上对齐 + GAP）
    let mut order = Vec::with_capacity(n);
    let mut y = vy;
    for mut row in rows {
        row.sort_by(|&a, &b| rects[a].x.partial_cmp(&rects[b].x).unwrap());
        let row_h = row.iter().map(|&j| rects[j].h).fold(0.0f32, f32::max);
        let mut x = vx;
        for j in row {
            let r = &mut rects[j];
            if r.w > vw {
                r.w = vw;
            }
            if r.h > vh {
                r.h = vh;
            }
            r.x = x;
            r.y = y;
            x += r.w + GAP;
            order.push(j);
        }
        y += row_h + GAP;
    }
    order
}

/// 多显示器版含屏:每个矩形按中心点就近夹回对应显示器工作区
/// (必要时缩小尺寸),支持栅栏分布在多台显示器上。
pub fn fit_to_monitors(rects: &mut [Rect], areas: &[(f32, f32, f32, f32)]) {
    if rects.is_empty() || areas.is_empty() {
        return;
    }
    for _ in 0..4 {
        for r in rects.iter_mut() {
            let cx = r.x + r.w * 0.5;
            let cy = r.y + r.h * 0.5;
            let best = areas.iter().min_by(|a, b| {
                let da = area_center_dist(cx, cy, a);
                let db = area_center_dist(cx, cy, b);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
            let Some(&(vx, vy, vw, vh)) = best else {
                return;
            };
            if r.w > vw {
                r.w = vw;
            }
            if r.h > vh {
                r.h = vh;
            }
            if r.x < vx {
                r.x = vx;
            } else if r.x + r.w > vx + vw {
                r.x = vx + vw - r.w;
            }
            if r.y < vy {
                r.y = vy;
            } else if r.y + r.h > vy + vh {
                r.y = vy + vh - r.h;
            }
        }
        // After clamping, re-resolve any overlaps that were introduced.
        let mut moved = false;
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                if conflicts_gap(&rects[i], &rects[j]) {
                    let pushed = push_away(&rects[i], &rects[j]);
                    if pushed != rects[j] {
                        rects[j] = pushed;
                        moved = true;
                    }
                }
            }
        }
        if !moved {
            break;
        }
    }
    // 边界硬约束:上面的解重叠可能把栅栏重新推出屏幕。
    // 最后无条件再夹回一轮(不再解重叠),保证任何情况栅栏都不出四边;
    // 若仍重叠,宁可见重叠也不出屏。
    for r in rects.iter_mut() {
        let cx = r.x + r.w * 0.5;
        let cy = r.y + r.h * 0.5;
        let best = areas.iter().min_by(|a, b| {
            let da = area_center_dist(cx, cy, a);
            let db = area_center_dist(cx, cy, b);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        let Some(&(vx, vy, vw, vh)) = best else {
            return;
        };
        if r.w > vw {
            r.w = vw;
        }
        if r.h > vh {
            r.h = vh;
        }
        if r.x < vx {
            r.x = vx;
        } else if r.x + r.w > vx + vw {
            r.x = vx + vw - r.w;
        }
        if r.y < vy {
            r.y = vy;
        } else if r.y + r.h > vy + vh {
            r.y = vy + vh - r.h;
        }
    }
}

/// 点到工作区中心的距离平方(就近选择显示器)
fn area_center_dist(x: f32, y: f32, a: &(f32, f32, f32, f32)) -> f32 {
    let cx = a.0 + a.2 * 0.5;
    let cy = a.1 + a.3 * 0.5;
    (x - cx) * (x - cx) + (y - cy) * (y - cy)
}

// ---------- 吸附(snap)纯逻辑 ----------

/// 值吸附到最近网格点（grid<=0 时原样返回）
pub fn snap_to_grid(v: f32, grid: f32) -> f32 {
    if grid <= 0.0 {
        return v;
    }
    (v / grid).round() * grid
}

/// 矩形位置吸附到图标网格（尺寸保持不变，仅夹紧最小尺寸）。
#[allow(dead_code)] // 纯逻辑 API，供单元测试与后续缩放吸附使用
pub fn snap_rect_grid(r: &Rect) -> Rect {
    Rect {
        x: snap_to_grid(r.x, grid_x()),
        y: snap_to_grid(r.y, grid_y()),
        w: r.w.max(min_w()),
        h: r.h.max(min_h()),
    }
}

/// 单轴磁吸候选：`value` 是被拖栅栏 left/top 的目标坐标，`guide` 是对齐参考线坐标。
#[derive(Clone, Copy)]
struct SnapCand {
    value: f32,
    guide: f32,
}

/// 收集 x / y 方向的对齐候选边（相邻栅栏四边 ± GAP + 屏幕工作区边缘）。
fn snap_candidates(
    r: &Rect,
    others: &[Rect],
    vx: f32,
    vy: f32,
    vw: f32,
    vh: f32,
) -> (Vec<SnapCand>, Vec<SnapCand>) {
    let mut cand_x: Vec<SnapCand> = vec![
        SnapCand {
            value: vx,
            guide: vx,
        }, // 屏幕左缘
        SnapCand {
            value: vx + vw - r.w,
            guide: vx + vw,
        }, // 屏幕右缘
    ];
    let mut cand_y: Vec<SnapCand> = vec![
        SnapCand {
            value: vy,
            guide: vy,
        }, // 屏幕上缘
        SnapCand {
            value: vy + vh - r.h,
            guide: vy + vh,
        }, // 屏幕下缘
    ];
    for o in others {
        if o.w <= 0.0 || o.h <= 0.0 {
            continue;
        }
        cand_x.push(SnapCand {
            value: o.x,
            guide: o.x,
        }); // 左缘对齐
        cand_x.push(SnapCand {
            value: o.x + o.w - r.w,
            guide: o.x + o.w,
        }); // 右缘对齐
        cand_x.push(SnapCand {
            value: o.x - GAP - r.w,
            guide: o.x,
        }); // 贴到 other 左侧（留 GAP）
        cand_x.push(SnapCand {
            value: o.x + o.w + GAP,
            guide: o.x + o.w,
        }); // 贴到 other 右侧（留 GAP）
        cand_y.push(SnapCand {
            value: o.y,
            guide: o.y,
        }); // 上缘对齐
        cand_y.push(SnapCand {
            value: o.y + o.h - r.h,
            guide: o.y + o.h,
        }); // 下缘对齐
        cand_y.push(SnapCand {
            value: o.y - GAP - r.h,
            guide: o.y,
        }); // 贴到 other 上方（留 GAP）
        cand_y.push(SnapCand {
            value: o.y + o.h + GAP,
            guide: o.y + o.h,
        }); // 贴到 other 下方（留 GAP）
    }
    (cand_x, cand_y)
}

/// 单轴磁吸：返回 (吸附后值, 对齐参考线坐标, 是否吸附)。
/// 即使与候选边距离为 0（本已对齐）也视为「已吸附」，避免再次被网格挪歪。
fn snap_axis(v: f32, candidates: &[SnapCand]) -> (f32, f32, bool) {
    let mut best = v;
    let mut best_guide = v;
    let mut best_d = f32::MAX;
    for c in candidates {
        let d = (c.value - v).abs();
        if d < SNAP_THRESHOLD && d < best_d {
            best_d = d;
            best = c.value;
            best_guide = c.guide;
        }
    }
    (best, best_guide, best_d < f32::MAX)
}

/// 把 rect 磁吸到相邻栅栏的边（含 GAP 间距）与屏幕工作区边缘。
/// x / y 各自独立选择阈值内位移最小的对齐候选，实现「拖到正下方/正右方
/// 保持 GAP 固定间距」以及横竖左右对齐。
#[allow(dead_code)] // 纯逻辑 API，供单元测试使用
pub fn snap_to_neighbors(r: &Rect, others: &[Rect], vx: f32, vy: f32, vw: f32, vh: f32) -> Rect {
    let (cand_x, cand_y) = snap_candidates(r, others, vx, vy, vw, vh);
    let mut nr = *r;
    let (nx, _, _) = snap_axis(r.x, &cand_x);
    let (ny, _, _) = snap_axis(r.y, &cand_y);
    nr.x = nx;
    nr.y = ny;
    nr
}

/// 水平重叠深度（正值 = 重叠量）
#[allow(dead_code)]
fn overlap_x(a: &Rect, b: &Rect) -> f32 {
    (a.x + a.w).min(b.x + b.w) - a.x.max(b.x)
}

/// 垂直重叠深度（正值 = 重叠量）
#[allow(dead_code)]
fn overlap_y(a: &Rect, b: &Rect) -> f32 {
    (a.y + a.h).min(b.y + b.h) - a.y.max(b.y)
}

/// 通过缩小 rect（按图标网格步长）解除与 others 的冲突，位置保持不变。
/// 内部图标会自动换行/缩排以适配更小的栅栏（「变小容纳」）。
/// 缩到最小仍冲突则返回最小尺寸（调用方可再微调位置）。
#[allow(dead_code)]
pub fn shrink_to_fit(r: &Rect, others: &[Rect]) -> Rect {
    let mut nr = *r;
    for _ in 0..32 {
        let mut any = false;
        for o in others {
            if o.w <= 0.0 || o.h <= 0.0 {
                continue;
            }
            if !conflicts_gap(&nr, o) {
                continue;
            }
            any = true;
            let can_w = nr.w - grid_x() >= min_w();
            let can_h = nr.h - grid_y() >= min_h();
            if can_w && can_h {
                // 优先缩重叠更深的一维
                if overlap_x(&nr, o) >= overlap_y(&nr, o) {
                    nr.w -= grid_x();
                } else {
                    nr.h -= grid_y();
                }
            } else if can_w {
                nr.w -= grid_x();
            } else if can_h {
                nr.h -= grid_y();
            } else {
                return nr; // 已到最小，仍冲突
            }
        }
        if !any {
            break;
        }
    }
    nr
}

/// 被拖栅栏自己让位：与 others 冲突时只推自己（其它栅栏位置不变），
/// 再夹回屏幕。用于「挤压后其它栅栏不动、只调整被拖栅栏」。
pub fn avoid_overlap(r: &Rect, others: &[Rect], vx: f32, vy: f32, vw: f32, vh: f32) -> Rect {
    let mut nr = *r;
    for _ in 0..16 {
        let mut moved = false;
        for o in others {
            if o.w <= 0.0 || o.h <= 0.0 {
                continue;
            }
            if conflicts_gap(&nr, o) {
                nr = push_away(o, &nr);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    let mut tmp = [nr];
    fit_to_screen(&mut tmp, vx, vy, vw, vh);
    tmp[0]
}

// ---------- 基础几何 ----------
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[allow(dead_code)]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Convert the bounds supplied by WM_DPICHANGED into the model's screen-space
/// rectangle. Invalid or inverted bounds are rejected instead of producing a
/// zero-sized layered window.
pub fn suggested_rect(left: i32, top: i32, right: i32, bottom: i32) -> Option<Rect> {
    let w = right.checked_sub(left)?;
    let h = bottom.checked_sub(top)?;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Rect {
        x: left as f32,
        y: top as f32,
        w: w as f32,
        h: h as f32,
    })
}

// ---------- 文件条目 ----------
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub ext: String,
    pub category: String,
    /// 最近修改时间(毫秒,1970 纪元);排序用,0=未知
    #[serde(default)]
    pub mtime_ms: u64,
}

// ---------- 分类 ----------
/// 栅栏分类顺序（默认布局从左到右）：软件 → 文件夹 → 文档 → 图片 → 媒体 → 代码 → 压缩包 → 其他。
/// 回收站虚拟图标固定在"软件"栅栏第一位（见 RECYCLE_BIN_PATH）。
pub const CATEGORIES: [&str; 8] = [
    "软件",
    "文件夹",
    "文档",
    "图片",
    "媒体",
    "代码",
    "压缩包",
    "其他",
];

pub const CATEGORY_COLORS: [[f32; 3]; 8] = [
    [0.51, 0.46, 0.86], // 软件 紫
    [0.85, 0.64, 0.27], // 文件夹 琥珀（柔和）
    [0.28, 0.55, 0.92], // 文档 蓝
    [0.34, 0.72, 0.48], // 图片 绿
    [0.88, 0.47, 0.58], // 媒体 粉
    [0.33, 0.72, 0.55], // 代码 绿青
    [0.90, 0.60, 0.32], // 压缩包 橙
    [0.55, 0.56, 0.60], // 其他 灰
];

/// 回收站虚拟条目的路径（Shell 命名空间 CLSID 解析名）。
/// 该条目不对应磁盘文件，由 ui 层注入到扫描结果，固定显示在"软件"栅栏第一位。
pub const RECYCLE_BIN_PATH: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

pub fn is_recycle_bin(path: &str) -> bool {
    path == RECYCLE_BIN_PATH
}

/// 回收站虚拟条目（归类为"软件"，使无配置时也落在软件栅栏）
pub fn recycle_bin_item() -> FileItem {
    FileItem {
        name: "回收站".into(),
        path: RECYCLE_BIN_PATH.into(),
        is_dir: true,
        ext: String::new(),
        category: "软件".into(),
        mtime_ms: 0,
    }
}

fn ext_of(name: &str) -> &str {
    let p = name.rfind('.');
    match p {
        Some(i) if i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

pub fn categorize(name: &str, is_dir: bool) -> String {
    if is_dir {
        return "文件夹".into();
    }
    let ext = ext_of(name).to_lowercase();
    let cat = match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "ico" => "图片",
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "csv" | "rtf"
        | "log" => "文档",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" => {
            "媒体"
        }
        "exe" | "msi" | "lnk" | "bat" | "cmd" | "com" => "软件",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "part" => "压缩包",
        "js" | "ts" | "py" | "rs" | "go" | "c" | "cpp" | "h" | "hpp" | "java" | "cs" | "rb"
        | "php" | "html" | "css" | "json" | "xml" | "yaml" | "yml" | "toml" | "sh" | "md" => {
            "代码"
        }
        _ => "其他",
    };
    cat.to_string()
}

pub fn category_index(cat: &str) -> usize {
    CATEGORIES.iter().position(|c| *c == cat).unwrap_or(7)
}

// ---------- 栅栏 ----------
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fence {
    pub id: u32,
    pub title: String,
    pub category: String,
    pub pinned: Vec<String>,
    /// 用户手工排列顺序；不存在的路径按默认分类/自然排序追加。
    #[serde(default)]
    pub item_order: Vec<String>,
    pub rect: Rect,
    pub collapsed: bool,
    pub scroll_rows: usize,
    pub locked: bool,
    pub hidden: bool,
    /// 用户手动缩放过尺寸：此后高度不再自动收敛到内容（尊重用户意图）。
    #[serde(default)]
    pub manual_size: bool,
    /// 栏内排序模式："常用"(默认,打开次数优先)/"时间"(最近修改)/"名称"/"手动"(拖拽自定义)
    #[serde(default = "default_sort_mode")]
    pub sort_mode: String,
}

pub fn default_sort_mode() -> String {
    "常用".into()
}

// ---------------- 使用频率统计(常用排序) ----------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageEntry {
    pub count: u32,
    pub last_ms: u64,
}

static USAGE: Mutex<Option<HashMap<String, UsageEntry>>> = Mutex::new(None);

fn usage_path() -> std::path::PathBuf {
    config_dir().join("usage.json")
}

fn usage_map() -> std::sync::MutexGuard<'static, Option<HashMap<String, UsageEntry>>> {
    USAGE.lock().unwrap()
}

pub fn load_usage() {
    let loaded: HashMap<String, UsageEntry> = std::fs::read_to_string(usage_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    *usage_map() = Some(loaded);
}

pub fn save_usage() {
    let json =
        serde_json::to_string(usage_map().as_ref().unwrap_or(&HashMap::new())).unwrap_or_default();
    let _ = atomic_write(&usage_path(), &json);
}

/// 记录一次打开(双击/Enter 打开文件时调用)
pub fn record_open(path: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut g = usage_map();
    let map = g.get_or_insert_with(HashMap::new);
    let e = map.entry(path.to_string()).or_default();
    e.count = e.count.saturating_add(1);
    e.last_ms = now;
}

pub fn usage_of(path: &str) -> (u32, u64) {
    let g = usage_map();
    g.as_ref()
        .and_then(|m| m.get(path))
        .map(|e| (e.count, e.last_ms))
        .unwrap_or((0, 0))
}

impl Fence {
    pub fn color(&self) -> [f32; 3] {
        CATEGORY_COLORS[category_index(&self.category)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    None,
    EdgeW,
    EdgeE,
    EdgeN,
    EdgeS,
    CornerNW,
    CornerNE,
    CornerSW,
    CornerSE,
    Title,
    Collapse,
    Scrollbar,
    Icon(usize),
    Blank,
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    pub total_rows: usize,
    #[allow(dead_code)] // 单元测试断言使用
    pub first_row: usize,
    pub first_index: usize,
    pub visible: usize,
}

pub fn layout(fence: &Fence, n_items: usize) -> Layout {
    layout_with_metrics(fence, n_items, &DpiMetrics::system())
}

pub fn layout_with_metrics(fence: &Fence, n_items: usize, metrics: &DpiMetrics) -> Layout {
    let cw = (fence.rect.w - 2.0 * metrics.pad).max(1.0);
    let ch = (fence.rect.h - metrics.title_h - metrics.pad * 2.0).max(1.0);
    let cols = ((cw / metrics.cell_w).floor() as usize).max(1);
    let rows = ((ch / metrics.cell_h).floor() as usize).max(1);
    let total_rows = if n_items == 0 {
        0
    } else {
        (n_items + cols - 1) / cols
    };
    let max_scroll = total_rows.saturating_sub(rows);
    let mut scroll = fence.scroll_rows.min(max_scroll);
    if fence.scroll_rows == 0 {
        scroll = 0;
    }
    let first_index = (scroll * cols).min(n_items);
    let visible = n_items.saturating_sub(first_index).min(rows * cols);
    Layout {
        cols,
        rows,
        total_rows,
        first_row: scroll,
        first_index,
        visible,
    }
}

/// 栅栏内（相对栅栏左上角）坐标命中测试
pub fn hit_test(fence: &Fence, la: &Layout, px: f32, py: f32, n_items: usize) -> Hit {
    hit_test_with_metrics(fence, la, px, py, n_items, &DpiMetrics::system())
}

pub fn hit_test_with_metrics(
    fence: &Fence,
    la: &Layout,
    px: f32,
    py: f32,
    n_items: usize,
    metrics: &DpiMetrics,
) -> Hit {
    let w = fence.rect.w;
    let h = fence.rect.h;
    if px < 0.0 || py < 0.0 || px >= w || py >= h {
        return Hit::None;
    }
    // 倒三角小块本身优先(小块内点击=操作菜单);小块之外的角/边热区
    // 仍归缩放拖拽。小块几何与 render.rs draw_title 保持一致:
    // 26×16,距右缘 3,垂直中心=标题文字中心
    let chip_cy = (4.0 + TITLE_H - 2.0) * 0.5;
    let chip_x0 = w - 3.0 - 26.0;
    if px >= chip_x0 && px <= w - 3.0 && py >= chip_cy - 8.0 && py <= chip_cy + 8.0 {
        return Hit::Collapse;
    }
    // 角落优先
    if px < metrics.corner && py < metrics.corner {
        return Hit::CornerNW;
    }
    if px < metrics.corner && py >= h - metrics.corner {
        return Hit::CornerSW;
    }
    if px >= w - metrics.corner && py < metrics.corner {
        return Hit::CornerNE;
    }
    if px >= w - metrics.corner && py >= h - metrics.corner {
        return Hit::CornerSE;
    }
    // 有滚动条时,标题栏以下的滚动条轨道区域命中滚动条;
    // 最外围 3px 仍归边缘缩放热区(边缘优先,便于抓住边调整大小)
    if la.total_rows > la.rows
        && py >= metrics.title_h
        && px >= w - metrics.scrollbar_w - 6.0 * metrics.scale
        && px < w - 3.0 * metrics.scale
    {
        return Hit::Scrollbar;
    }
    if px < metrics.edge {
        return Hit::EdgeW;
    }
    if px >= w - metrics.edge {
        return Hit::EdgeE;
    }
    if py < metrics.edge {
        return Hit::EdgeN;
    }
    if py >= h - metrics.edge {
        return Hit::EdgeS;
    }
    if py < metrics.title_h {
        return Hit::Title;
    }
    // 内容区
    if fence.collapsed {
        return Hit::Blank;
    }
    let cx = px - metrics.pad;
    let cy = py - metrics.title_h - metrics.pad;
    if cx < 0.0 || cy < 0.0 {
        return Hit::Blank;
    }
    let col = (cx / metrics.cell_w).floor() as usize;
    let row = (cy / metrics.cell_h).floor() as usize;
    if col >= la.cols || row >= la.rows {
        return Hit::Blank;
    }
    let idx = la.first_index + row * la.cols + col;
    if idx < n_items {
        Hit::Icon(idx)
    } else {
        Hit::Blank
    }
}

/// 根据边/角拖动量计算新矩形并夹紧最小尺寸。
/// 西/北边缩放夹到最小尺寸时固定远边(右/下),避免栅栏整体漂移。
pub fn apply_resize(r: &Rect, edges: &[char], dx: f32, dy: f32) -> Rect {
    let mut nr = *r;
    if edges.contains(&'w') {
        let ddx = dx.min(r.w - min_w());
        nr.w = (r.w - ddx).max(min_w());
        nr.x = r.x + ddx;
    }
    if edges.contains(&'e') {
        nr.w = (nr.w + dx).max(min_w());
    }
    if edges.contains(&'n') {
        let ddy = dy.min(r.h - min_h());
        nr.h = (r.h - ddy).max(min_h());
        nr.y = r.y + ddy;
    }
    if edges.contains(&'s') {
        nr.h = (nr.h + dy).max(min_h());
    }
    nr
}

/// 图标在栅栏内的绘制位置（相对左上角）
pub fn cell_pos(la: &Layout, index: usize) -> (f32, f32) {
    cell_pos_with_metrics(la, index, &DpiMetrics::system())
}

pub fn cell_pos_with_metrics(la: &Layout, index: usize, metrics: &DpiMetrics) -> (f32, f32) {
    let rel = index - la.first_index;
    let row = rel / la.cols;
    let col = rel % la.cols;
    let x = metrics.pad + col as f32 * metrics.cell_w;
    let y = metrics.title_h + metrics.pad + row as f32 * metrics.cell_h;
    (x, y)
}

pub fn cell_rect(la: &Layout, index: usize) -> Rect {
    cell_rect_with_metrics(la, index, &DpiMetrics::system())
}

pub fn cell_rect_with_metrics(la: &Layout, index: usize, metrics: &DpiMetrics) -> Rect {
    let (x, y) = cell_pos_with_metrics(la, index, metrics);
    Rect {
        x,
        y,
        w: metrics.cell_w,
        h: metrics.cell_h,
    }
}

pub fn ease_out_cubic(progress: f32) -> f32 {
    let p = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - p).powi(3)
}

pub fn interpolate_point(from: (f32, f32), to: (f32, f32), progress: f32) -> (f32, f32) {
    let eased = ease_out_cubic(progress);
    (
        from.0 + (to.0 - from.0) * eased,
        from.1 + (to.1 - from.1) * eased,
    )
}

pub fn newly_added_paths(old: &[FileItem], new: &[FileItem]) -> Vec<String> {
    let old_paths: std::collections::HashSet<&str> =
        old.iter().map(|item| item.path.as_str()).collect();
    new.iter()
        .filter(|item| !old_paths.contains(item.path.as_str()))
        .map(|item| item.path.clone())
        .collect()
}

/// 将 `dragged_paths` 对应项作为一个块插入 `target_slot`。
///
/// `target_slot` 是从 `original` 删除所有拖动项后的列表槽位，范围为
/// `0..=remaining.len()`；超界值会夹紧到末尾。拖动块始终按它们在 `original`
/// 中的相对顺序排列，重复或不存在的路径不会产生额外条目。
pub fn reorder_paths_as_block(
    original: &[String],
    dragged_paths: &[String],
    target_slot: usize,
) -> Vec<String> {
    if original.is_empty() || dragged_paths.is_empty() {
        return original.to_vec();
    }
    let dragged: std::collections::HashSet<&str> =
        dragged_paths.iter().map(String::as_str).collect();
    let block: Vec<String> = original
        .iter()
        .filter(|path| dragged.contains(path.as_str()))
        .cloned()
        .collect();
    if block.is_empty() {
        return original.to_vec();
    }
    let mut remaining: Vec<String> = original
        .iter()
        .filter(|path| !dragged.contains(path.as_str()))
        .cloned()
        .collect();
    let slot = target_slot.min(remaining.len());
    remaining.splice(slot..slot, block);
    remaining
}

pub fn indices_between(la: &Layout, a: usize, b: usize, n_items: usize) -> Vec<usize> {
    if n_items == 0 {
        return Vec::new();
    }
    let lo = a.min(b).min(n_items - 1);
    let hi = a.max(b).min(n_items - 1);
    (lo..=hi)
        .filter(|i| *i >= la.first_index && *i < la.first_index + la.visible)
        .collect()
}

pub fn indices_in_rect(la: &Layout, rect: &Rect, n_items: usize) -> Vec<usize> {
    if rect.w.abs() < f32::EPSILON || rect.h.abs() < f32::EPSILON || n_items == 0 {
        return Vec::new();
    }
    let left = rect.x.min(rect.x + rect.w);
    let right = rect.x.max(rect.x + rect.w);
    let top = rect.y.min(rect.y + rect.h);
    let bottom = rect.y.max(rect.y + rect.h);
    let mut out = Vec::new();
    for row in 0..la.rows {
        for col in 0..la.cols {
            let idx = la.first_index + row * la.cols + col;
            if idx >= n_items || idx >= la.first_index + la.visible {
                continue;
            }
            let cell = cell_rect(la, idx);
            if cell.x < right && cell.x + cell.w > left && cell.y < bottom && cell.y + cell.h > top
            {
                out.push(idx);
            }
        }
    }
    out
}

#[allow(dead_code)]
pub fn max_scroll(_fence: &Fence, la: &Layout) -> usize {
    la.total_rows.saturating_sub(la.rows)
}

// ---------- 配置持久化 ----------
pub fn config_dir() -> std::path::PathBuf {
    std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("DeskFence")
}

pub fn config_path() -> std::path::PathBuf {
    config_dir().join("config.json")
}

pub fn settings_path() -> std::path::PathBuf {
    config_dir().join("settings.json")
}

/// 用户偏好（独立于栅栏配置持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 对齐模式:"auto"=固定间隔自动对齐(默认) / "grid"=图标格倍数步进 / "free"=自由移动
    pub align_mode: String,
    /// 渲染模式:"transparent"=透明窗口(兼容动态壁纸) /
    /// "precise"=精确模式(不透明壁纸底+GDI ClearType 文字,与原生逐像素一致;默认)。
    /// 运行中检测到动态壁纸或壁纸捕获连续失败时,ui 层会自动把配置回退为
    /// transparent(渲染模式菜单里能看到当前实际档位)。
    #[serde(default = "default_render_mode")]
    pub render_mode: String,
    /// 自动分类:true=按固定 8 类(软件/文件夹/文档/图片/媒体/代码/压缩包/其他)
    /// 自动归类(默认);false=自定义分类模式,不按扩展名归类,文件只进被拖入
    /// 的栅栏,未分配的文件集中显示在"未分类"栅栏。
    #[serde(default = "default_auto_category")]
    pub auto_category: bool,
    /// 桌面状态(2026-08-27 起持久化):"normal"=栅栏显示(默认) /
    /// "zen"=纯净态(栅栏与原生图标都隐藏,只剩壁纸) /
    /// "native"=原生桌面(原生图标接管,栅栏隐藏)。托盘菜单切换时落盘,
    /// 启动按此恢复。
    #[serde(default = "default_desktop_state")]
    pub desktop_state: String,
    /// z 守卫(默认开):否决外部进程对栅栏窗口 z 序的改写。显示桌面/
    /// 最小化批次会把栅栏压到桌面宿主之下(壁纸后面),只翻隐藏位挡不住
    /// (2026-08-28 wdprobe 实测)。置 false 回退为纯自愈行为,供降级排查。
    #[serde(default = "default_z_guard")]
    pub z_guard: bool,
    /// 常显栅栏边框线(默认关):开=全部栅栏常显边框/标题/角手柄,便于观察
    /// 布局;关=无边框常显(悬停或拖拽时才浮现)。托盘菜单切换即落盘。
    #[serde(default)]
    pub show_chrome: bool,
}

pub fn default_desktop_state() -> String {
    "normal".into()
}
pub fn default_render_mode() -> String {
    "precise".into()
}
pub fn default_auto_category() -> bool {
    true
}
pub fn default_z_guard() -> bool {
    true
}

/// 自定义分类模式的兜底类别:未被任何栅栏收纳的文件都在这里,保证不"隐身"
pub const UNCATEGORIZED: &str = "未分类";

thread_local! {
    static AUTO_CATEGORY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}
pub fn set_auto_category(v: bool) {
    AUTO_CATEGORY.with(|c| c.set(v));
}
pub fn auto_category() -> bool {
    AUTO_CATEGORY.with(|c| c.get())
}

thread_local! {
    static PINNED_PATHS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}
/// 重建"已被某栅栏收纳(pinned)"的全局路径表:自定义模式下用于
/// 排除已分配文件 / 生成"未分类"列表
pub fn rebuild_pinned_registry(fences: &[Fence]) {
    let mut set = std::collections::HashSet::new();
    for f in fences {
        for p in &f.pinned {
            set.insert(p.clone());
        }
    }
    PINNED_PATHS.with(|r| *r.borrow_mut() = set);
    rebuild_category_registry(fences);
}
fn pinned_elsewhere(path: &str) -> bool {
    PINNED_PATHS.with(|r| r.borrow().contains(path))
}

thread_local! {
    static FENCE_CATEGORIES: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}
/// 重建"现有栅栏类别"表:自定义模式下判断文件是否还有类别栅栏可归
pub fn rebuild_category_registry(fences: &[Fence]) {
    let mut set = std::collections::HashSet::new();
    for f in fences {
        if !f.category.is_empty() {
            set.insert(f.category.clone());
        }
    }
    FENCE_CATEGORIES.with(|r| *r.borrow_mut() = set);
}
fn has_category_fence(cat: &str) -> bool {
    FENCE_CATEGORIES.with(|r| r.borrow().contains(cat))
}
impl Default for Settings {
    fn default() -> Self {
        Settings {
            align_mode: "auto".into(),
            render_mode: default_render_mode(),
            auto_category: default_auto_category(),
            desktop_state: default_desktop_state(),
            z_guard: default_z_guard(),
            show_chrome: false,
        }
    }
}
pub fn load_settings() -> Settings {
    let p = settings_path();
    if !p.exists() {
        return Settings::default();
    }
    let raw = match std::fs::read_to_string(&p) {
        Ok(r) => r,
        Err(_) => return Settings::default(),
    };
    // 新版字段;读取失败则按旧版 bool 迁移(true→auto, false→free)
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => s,
        Err(_) => {
            let legacy = serde_json::from_str::<serde_json::Value>(&raw).ok();
            let mode = legacy
                .as_ref()
                .and_then(|v| v.get("auto_align"))
                .and_then(|b| b.as_bool())
                .map(|b| if b { "auto" } else { "free" })
                .unwrap_or("auto");
            Settings {
                align_mode: mode.into(),
                render_mode: default_render_mode(),
                auto_category: default_auto_category(),
                desktop_state: default_desktop_state(),
                z_guard: default_z_guard(),
            show_chrome: false,
            }
        }
    }
}
fn atomic_write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    #[cfg(windows)]
    {
        let from: Vec<u16> = tmp
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let to: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                windows::core::PCWSTR::from_raw(from.as_ptr()),
                windows::core::PCWSTR::from_raw(to.as_ptr()),
                windows::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, path)
    }
}

pub fn save_settings(s: &Settings) {
    let json = serde_json::to_string_pretty(s).unwrap_or_default();
    let _ = atomic_write(&settings_path(), &json);
}

/// DeskFence 接管会话标记。它只表示本程序曾临时隐藏过原生桌面图标，
/// 不保存、不修改 Explorer 的图标位置、排序或任何桌面文件数据。
pub fn icons_marker_path() -> std::path::PathBuf {
    config_dir().join("icons_marker.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IconsMarker {
    pid: u32,
    #[serde(default)]
    version: u32,
}

pub fn save_icons_marker(pid: u32) {
    let json = serde_json::to_string_pretty(&IconsMarker { pid, version: 1 }).unwrap_or_default();
    let _ = atomic_write(&icons_marker_path(), &json);
}

pub fn load_icons_marker() -> Option<u32> {
    std::fs::read_to_string(icons_marker_path())
        .ok()
        .and_then(|s| serde_json::from_str::<IconsMarker>(&s).ok())
        .map(|m| m.pid)
}

pub fn clear_icons_marker() {
    let _ = std::fs::remove_file(icons_marker_path());
    let _ = std::fs::remove_file(icons_marker_path().with_extension("json.tmp"));
}

/// 局部链式对齐(自动档核心):被拖栅栏(anchor)原地不动,
/// 同一水平带内的左邻居向左排、右邻居向右排,相邻间距恒为 GAP;
/// 不同行/远处栅栏不受影响——落点即所得(想在左就在左,想在右就在右)。
pub fn align_local_chain(rects: &mut [Rect], anchor: usize, vx: f32, vy: f32, vw: f32, vh: f32) {
    if rects.is_empty() || anchor >= rects.len() {
        return;
    }
    let _ = vh;
    let a = rects[anchor];
    let center = |r: &Rect| r.x + r.w * 0.5;
    let same_row = |r: &Rect| r.y < a.y + a.h + GAP * 0.5 && a.y < r.y + r.h + GAP * 0.5;
    // 按中心点分左右(避免与锚点横向重叠者漏分类),同排之外不动
    let mut lefts: Vec<usize> = (0..rects.len())
        .filter(|i| *i != anchor && same_row(&rects[*i]) && center(&rects[*i]) < center(&a))
        .collect();
    lefts.sort_by(|x, y| {
        rects[*y]
            .x
            .partial_cmp(&rects[*x].x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rights: Vec<usize> = (0..rects.len())
        .filter(|i| *i != anchor && same_row(&rects[*i]) && center(&rects[*i]) >= center(&a))
        .collect();
    rights.sort_by(|x, y| {
        rects[*x]
            .x
            .partial_cmp(&rects[*y].x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let _ = vy;
    // 左链:贴锚排左;放不下时换行到下方
    let mut cursor = a.x;
    let mut wrap_bottom = a.y + a.h;
    for i in lefts {
        let desired = cursor - GAP - rects[i].w;
        if desired < vx {
            rects[i].x = vx;
            rects[i].y = wrap_bottom + GAP;
            wrap_bottom = rects[i].y + rects[i].h;
        } else {
            rects[i].x = desired;
        }
        cursor = rects[i].x;
    }
    // 右链:贴锚排右;放不下时换行到下方
    let mut cursor = a.x + a.w;
    let mut wrap_y = a.y + a.h + GAP;
    for i in rights {
        let desired = cursor + GAP;
        if desired + rects[i].w > vx + vw {
            rects[i].x = (vx + vw - rects[i].w).max(vx);
            rects[i].y = wrap_y;
            wrap_y = rects[i].y + rects[i].h + GAP;
            cursor = rects[i].x + rects[i].w;
        } else {
            rects[i].x = desired;
            cursor = desired + rects[i].w;
        }
    }
}

pub fn snap_gap_to_neighbors(r: &Rect, others: &[Rect], threshold: f32) -> (f32, bool) {
    let mut best: Option<(f32, f32)> = None; // (吸附x, 距离)
    let same_row = |o: &Rect| o.y < r.y + r.h + GAP * 0.5 && r.y < o.y + o.h + GAP * 0.5;
    for o in others {
        if !same_row(o) {
            continue;
        }
        // 放在 o 右侧: x = o.x + o.w + GAP
        let cand_r = o.x + o.w + GAP;
        let d_r = (r.x - cand_r).abs();
        if d_r < threshold && best.map_or(true, |(_, d)| d_r < d) {
            best = Some((cand_r, d_r));
        }
        // 放在 o 左侧: x = o.x - GAP - r.w
        let cand_l = o.x - GAP - r.w;
        let d_l = (r.x - cand_l).abs();
        if d_l < threshold && best.map_or(true, |(_, d)| d_l < d) {
            best = Some((cand_l, d_l));
        }
    }
    match best {
        Some((x, _)) => (x, true),
        None => (r.x, false),
    }
}

pub fn auto_layout(rects: &mut [Rect], vx: f32, vy: f32, vw: f32, vh: f32) {
    if rects.is_empty() || vw <= 0.0 || vh <= 0.0 {
        return;
    }
    let mut x = vx;
    let mut y = vy;
    let mut row_h = 0.0;
    for r in rects.iter_mut() {
        if r.w > vw {
            r.w = vw;
        }
        if r.h > vh {
            r.h = vh;
        }
        if x + r.w > vx + vw && x > vx {
            x = vx;
            y += row_h + GAP;
            row_h = 0.0;
        }
        r.x = x;
        r.y = y;
        x += r.w + GAP;
        row_h = row_h.max(r.h);
    }
}

pub fn load_config() -> Vec<Fence> {
    let p = config_path();
    if !p.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Fence>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(fences: &[Fence]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(fences).unwrap_or_default();
    atomic_write(&config_path(), &json)
}

pub fn build_global_config(files: &[FileItem]) -> Vec<Fence> {
    let mut map: HashMap<&str, Vec<&FileItem>> = HashMap::new();
    for f in files {
        map.entry(f.category.as_str()).or_default().push(f);
    }
    let mut out = Vec::new();
    let mut id = 1u32;
    for cat in CATEGORIES {
        let list = map.get(cat).cloned().unwrap_or_default();
        if list.is_empty() {
            continue;
        }
        // 默认高度 4 行(一屏可上下放两排栅栏);宽度 2 列,项数不足 5 时收窄为 1 列
        let cols = if list.len() < 5 { 1usize } else { 2usize };
        let rows = 4usize;
        let (title_h, pad) = chrome(dpi_scale());
        let w = cell_w() * cols as f32 + pad * 2.0 + 2.0;
        let h = title_h + rows as f32 * cell_h() + pad * 2.0 + 2.0;
        out.push(Fence {
            id,
            title: cat.to_string(),
            category: cat.to_string(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: Rect {
                x: 16.0 + (id as f32 - 1.0) * (w + 20.0),
                y: 60.0,
                w,
                h,
            },
            collapsed: false,
            scroll_rows: 0,
            locked: false,
            hidden: false,
            manual_size: false,
            sort_mode: default_sort_mode(),
        });
        id += 1;
    }
    out
}

/// 栅栏展示列表：分类文件 + 各自 pinned 的文件（去重），按目录优先、名称排序
fn natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();
    loop {
        match (ia.peek(), ib.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = String::new();
                let mut nb = String::new();
                while ia.peek().is_some_and(|c| c.is_ascii_digit()) {
                    na.push(ia.next().unwrap());
                }
                while ib.peek().is_some_and(|c| c.is_ascii_digit()) {
                    nb.push(ib.next().unwrap());
                }
                let oa = na.parse::<u128>().unwrap_or(u128::MAX);
                let ob = nb.parse::<u128>().unwrap_or(u128::MAX);
                if oa != ob {
                    return oa.cmp(&ob);
                }
                if na.len() != nb.len() {
                    return na.len().cmp(&nb.len());
                }
            }
            (Some(ca), Some(cb)) => {
                let la = ca.to_lowercase().next().unwrap_or(*ca);
                let lb = cb.to_lowercase().next().unwrap_or(*cb);
                ia.next();
                ib.next();
                if la != lb {
                    return la.cmp(&lb);
                }
            }
        }
    }
}

pub fn display_list(fence: &Fence, all: &[FileItem]) -> Vec<FileItem> {
    let mut out: Vec<FileItem> = Vec::new();
    // 分类显示规则(两种模式统一):
    // · 类别栅栏:按类别匹配 + 未被拖去别的栅栏(pinned 优先于类别)
    // · 自建空类别栅栏:只显示拖入(pinned)的文件
    // · "未分类"(仅自定义模式):既没被收纳、其类别也没有对应栅栏的文件
    // 自定义模式下切换不打乱现状——已建类别栅栏继续按类显示,
    // 只是新建类别栅栏停止、无类可归的进"未分类"。
    let _ = auto_category();
    if fence.category == UNCATEGORIZED {
        if !auto_category() {
            for f in all.iter() {
                if !pinned_elsewhere(&f.path) && !has_category_fence(&f.category) {
                    out.push(f.clone());
                }
            }
        }
    } else {
        for f in all.iter() {
            if !fence.category.is_empty()
                && f.category == fence.category
                && !fence.pinned.contains(&f.path)
                && !pinned_elsewhere(&f.path)
            {
                out.push(f.clone());
            }
        }
    }
    for p in &fence.pinned {
        if let Some(f) = all.iter().find(|f| &f.path == p) {
            if !out.iter().any(|x| x.path == f.path) {
                out.push(f.clone());
            }
        }
        // 文件已不存在的 pinned 条目：不再造占位(原生桌面不会显示不存在的文件)，
        // 持久化时由清理逻辑剔除
    }
    // 回收站虚拟图标永远排第一位（不可被排序/拖动改变）
    out.sort_by(|a, b| {
        match (is_recycle_bin(&a.path), is_recycle_bin(&b.path)) {
            (true, true) => return std::cmp::Ordering::Equal,
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        let fallback = || {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| natural_name_cmp(&a.name, &b.name))
        };
        match fence.sort_mode.as_str() {
            "常用" => {
                let (ca, la) = usage_of(&a.path);
                let (cb, lb) = usage_of(&b.path);
                cb.cmp(&ca).then_with(|| lb.cmp(&la)).then_with(fallback)
            }
            "时间" => b.mtime_ms.cmp(&a.mtime_ms).then_with(fallback),
            "名称" => natural_name_cmp(&a.name, &b.name),
            // "手动"及未知模式：按用户拖拽顺序
            _ => {
                let ai = fence.item_order.iter().position(|p| p == &a.path);
                let bi = fence.item_order.iter().position(|p| p == &b.path);
                match (ai, bi) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => fallback(),
                }
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_keeps_drop_position_and_fixed_gaps() {
        // 屏幕 1920x1208,5 栏 244 宽;把第 2 栏拖到 x=1000(右侧)
        let mut rects: Vec<Rect> = (0..5)
            .map(|i| Rect {
                x: 16.0 + i as f32 * 256.0,
                y: 60.0,
                w: 244.0,
                h: 704.0,
            })
            .collect();
        rects[1] = Rect {
            x: 1000.0,
            y: 60.0,
            w: 244.0,
            h: 704.0,
        };
        align_local_chain(&mut rects, 1, 0.0, 0.0, 1920.0, 1208.0);
        // 锚点(被拖栏)落点保留
        assert_eq!(rects[1].x, 1000.0);
        // 所有窗口都在屏幕内
        for r in &rects {
            assert!(r.x >= -0.5 && r.x + r.w <= 1920.5, "off-screen x={}", r.x);
        }
        // 同排内相邻间距要么 12 要么被屏幕边界允许
        let mut row: Vec<&Rect> = rects.iter().filter(|r| r.y == 60.0).collect();
        row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
        for w in row.windows(2) {
            let gap = w[1].x - (w[0].x + w[0].w);
            assert!((gap - GAP).abs() < 0.01, "gap {} != {}", gap, GAP);
        }
    }

    #[test]
    fn chain_wraps_without_stacking() {
        // 屏幕只够放 3 栏:第 4+ 栏应换行而不是堆叠
        let mut rects: Vec<Rect> = (0..5)
            .map(|i| Rect {
                x: 16.0 + i as f32 * 20.0,
                y: 60.0,
                w: 300.0,
                h: 200.0,
            })
            .collect();
        align_local_chain(&mut rects, 0, 0.0, 0.0, 1000.0, 1208.0);
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let a = &rects[i];
                let b = &rects[j];
                let overlap =
                    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                assert!(!overlap, "fence {} overlaps {} ({:?} {:?})", i, j, a, b);
            }
            assert!(rects[i].x >= -0.5 && rects[i].x + rects[i].w <= 1000.5);
        }
    }

    #[test]
    fn chain_wrap_then_return_restores_neighbor_from_snapshot() {
        // 复现"拖到最右把邻居挤到下方,拖回来邻居还原"的场景:
        // 新预览管线每次都从按下快照重算,被换行的邻居必然回到原位
        let snapshot = vec![
            Rect {
                x: 400.0,
                y: 60.0,
                w: 380.0,
                h: 300.0,
            }, // 锚
            Rect {
                x: 8.0,
                y: 60.0,
                w: 380.0,
                h: 300.0,
            }, // 左邻居 B
        ];
        // 锚拖到 x=380:B 放不下(380-12-380<0)→ 换行到锚下方
        let mut f1 = snapshot.clone();
        f1[0].x = 380.0;
        align_local_chain(&mut f1, 0, 0.0, 0.0, 800.0, 1208.0);
        assert!(!intersects(&f1[0], &f1[1]));
        assert!(
            f1[1].y >= f1[0].y + f1[0].h,
            "B 应换行到锚下方, got {:?}",
            f1[1]
        );
        // 锚拖回原位:从同一快照重算,B 完全还原(旧实现里 B 会永久卡在下方)
        let mut f2 = snapshot.clone();
        f2[0].x = 400.0;
        align_local_chain(&mut f2, 0, 0.0, 0.0, 800.0, 1208.0);
        assert_eq!(f2[1], snapshot[1], "邻居应还原到快照位置");
    }

    #[test]
    fn gap_snap_magnet() {
        let r = Rect {
            x: 300.0,
            y: 60.0,
            w: 244.0,
            h: 704.0,
        };
        let others = [Rect {
            x: 700.0,
            y: 60.0,
            w: 244.0,
            h: 704.0,
        }];
        // 靠近"放在 other 左侧"的吸附位(700-12-244=444)时应吸附
        let (x, snapped) = snap_gap_to_neighbors(&Rect { x: 448.0, ..r }, &others, 18.0);
        assert!(snapped && (x - 444.0).abs() < 0.01);
        // 远离时不吸附
        let (x2, snapped2) = snap_gap_to_neighbors(&Rect { x: 100.0, ..r }, &others, 18.0);
        assert!(!snapped2 && x2 == 100.0);
    }

    fn fence(id: u32) -> Fence {
        Fence {
            id,
            title: "t".into(),
            category: "图片".into(),
            pinned: Vec::new(),
            item_order: Vec::new(),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 240.0,
            },
            collapsed: false,
            scroll_rows: 0,
            locked: false,
            hidden: false,
            manual_size: false,
            sort_mode: default_sort_mode(),
        }
    }

    #[test]
    fn per_fence_metrics_keep_layout_and_hits_independent() {
        let f = fence(1);
        let m96 = DpiMetrics::new(96, 32.0, 43.0, 54.0);
        let m144 = DpiMetrics::new(144, 48.0, 43.0, 54.0);
        let a = layout_with_metrics(&f, 20, &m96);
        let b = layout_with_metrics(&f, 20, &m144);
        assert!(a.cols > b.cols);
        let (x, y) = cell_pos_with_metrics(&a, a.first_index, &m96);
        assert_eq!(
            hit_test_with_metrics(&f, &a, x + m96.edge + 1.0, y + 1.0, 20, &m96),
            Hit::Icon(a.first_index)
        );
        assert_ne!(m96.cell_w, m144.cell_w);
        assert_ne!(m96.title_h, m144.title_h);
    }

    #[test]
    fn categorize_works() {
        assert_eq!(categorize("a.png", false), "图片");
        assert_eq!(categorize("b.exe", false), "软件");
        assert_eq!(categorize("c", false), "其他");
        assert_eq!(categorize("d", true), "文件夹");
        assert_eq!(categorize("e.LNK", false), "软件");
    }

    #[test]
    fn layout_basic() {
        let f = fence(1);
        let la = layout(&f, 20);
        assert_eq!(la.cols, ((200.0 - 12.0) / cell_w()) as usize); // 默认留白 43 -> cell_w 75 -> 2 列
        assert!(la.total_rows >= 1);
        assert!(la.first_index <= 20);
        assert!(la.visible >= 1);
    }

    #[test]
    fn resize_clamps_min() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            w: 200.0,
            h: 240.0,
        };
        let nr = apply_resize(&r, &['e'], -500.0, 0.0);
        assert!(nr.w >= min_w());
        let nr2 = apply_resize(&r, &['s'], -500.0, 0.0);
        assert!(nr2.h >= min_h());
    }

    #[test]
    fn resize_west_moves_x() {
        let r = Rect {
            x: 100.0,
            y: 20.0,
            w: 200.0,
            h: 240.0,
        };
        let nr = apply_resize(&r, &['w'], 30.0, 0.0);
        assert_eq!(nr.x, 130.0);
        assert!((nr.w - 170.0).abs() < 0.001);
    }

    #[test]
    fn resize_west_pins_far_edge_when_clamped() {
        // 向左拖过头:宽度夹到最小值,右边缘必须保持不动
        let r = Rect {
            x: 100.0,
            y: 20.0,
            w: 200.0,
            h: 240.0,
        };
        let nr = apply_resize(&r, &['w'], 5000.0, 0.0);
        assert!((nr.w - min_w()).abs() < 0.001, "w={}", nr.w);
        assert!(
            ((nr.x + nr.w) - (r.x + r.w)).abs() < 0.001,
            "far edge drifted: {:?}",
            nr
        );
    }

    #[test]
    fn resize_north_pins_bottom_when_clamped() {
        let r = Rect {
            x: 100.0,
            y: 20.0,
            w: 200.0,
            h: 240.0,
        };
        let nr = apply_resize(&r, &['n'], 0.0, 5000.0);
        assert!((nr.h - min_h()).abs() < 0.001, "h={}", nr.h);
        assert!(
            ((nr.y + nr.h) - (r.y + r.h)).abs() < 0.001,
            "bottom drifted: {:?}",
            nr
        );
    }

    #[test]
    fn resize_west_grows_leftward_keeps_far_edge() {
        let r = Rect {
            x: 100.0,
            y: 20.0,
            w: 200.0,
            h: 240.0,
        };
        let nr = apply_resize(&r, &['w'], -40.0, 0.0);
        assert!((nr.w - 240.0).abs() < 0.001);
        assert!(((nr.x + nr.w) - (r.x + r.w)).abs() < 0.001);
    }

    #[test]
    fn fit_to_monitors_clamps_into_nearest_area() {
        // 两个工作区:主屏 (0,0,1920,1080),副屏 (1920,0,1920,1080)
        let areas = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 1920.0, 1080.0)];
        let mut rects = vec![
            Rect {
                x: 2500.0,
                y: 100.0,
                w: 200.0,
                h: 300.0,
            }, // 副屏,越右界
            Rect {
                x: -500.0,
                y: 100.0,
                w: 200.0,
                h: 300.0,
            }, // 主屏,越左界
        ];
        fit_to_monitors(&mut rects, &areas);
        assert!(rects[0].x + rects[0].w <= 3840.0 + 0.01 && rects[0].x >= 1920.0 - 0.01);
        assert!(rects[1].x >= -0.01 && rects[1].x + rects[1].w <= 1920.01);
        // 过大尺寸应缩小到所在工作区
        let mut big = vec![Rect {
            x: 100.0,
            y: 100.0,
            w: 4000.0,
            h: 300.0,
        }];
        fit_to_monitors(&mut big, &areas);
        assert!(big[0].w <= 1920.0 + 0.01);
    }

    #[test]
    fn hit_test_edges() {
        let f = fence(1);
        let la = layout(&f, 20);
        assert_eq!(hit_test(&f, &la, 1.0, 50.0, 20), Hit::EdgeW);
        assert_eq!(hit_test(&f, &la, 199.0 - 1.0, 50.0, 20), Hit::EdgeE);
        assert_eq!(hit_test(&f, &la, 50.0, 1.0, 20), Hit::EdgeN);
        assert_eq!(hit_test(&f, &la, 50.0, 239.0, 20), Hit::EdgeS);
        assert_eq!(hit_test(&f, &la, 1.0, 1.0, 20), Hit::CornerNW);
        assert_eq!(hit_test(&f, &la, 199.0, 239.0, 20), Hit::CornerSE);
        assert_eq!(hit_test(&f, &la, 100.0, 10.0, 20), Hit::Title);
        assert_eq!(hit_test(&f, &la, 5.0, 5.0, 0), Hit::CornerNW);
    }

    #[test]
    fn scroll_clamps() {
        let mut f = fence(1);
        // 50 个图标，宽 200 -> cols=2 -> total_rows=25, rows 大约 (240-26-12)/76=2
        let la = layout(&f, 50);
        let mx = max_scroll(&f, &la);
        f.scroll_rows = 1000;
        let la2 = layout(&f, 50);
        assert_eq!(la2.first_row, mx.min(1000));
        assert!(la2.first_index <= 50);
        assert!(la2.visible >= 1);
    }

    #[test]
    fn arrival_easing_and_interpolation_are_clamped() {
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 0.0001);
        assert_eq!(
            interpolate_point((10.0, 20.0), (110.0, 220.0), 0.0),
            (10.0, 20.0)
        );
        assert_eq!(
            interpolate_point((10.0, 20.0), (110.0, 220.0), 1.0),
            (110.0, 220.0)
        );
    }

    #[test]
    fn newly_added_paths_preserves_new_scan_order() {
        let item = |path: &str| FileItem {
            name: path.to_string(),
            path: path.to_string(),
            is_dir: false,
            ext: String::new(),
            category: "其他".to_string(),
            mtime_ms: 0,
        };
        let old = vec![item("a"), item("b")];
        let new = vec![item("b"), item("c"), item("a"), item("d")];
        assert_eq!(newly_added_paths(&old, &new), vec!["c", "d"]);
    }

    #[test]
    fn reorder_paths_as_block_preserves_original_relative_order() {
        let original: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let dragged: Vec<String> = ["e", "b", "d"].into_iter().map(str::to_string).collect();

        assert_eq!(
            reorder_paths_as_block(&original, &dragged, 1),
            ["a", "b", "d", "e", "c", "f"].map(str::to_string)
        );
    }

    #[test]
    fn reorder_paths_as_block_supports_start_end_and_clamps_slot() {
        let original: Vec<String> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let dragged: Vec<String> = ["b", "d"].into_iter().map(str::to_string).collect();

        assert_eq!(
            reorder_paths_as_block(&original, &dragged, 0),
            ["b", "d", "a", "c", "e"].map(str::to_string)
        );
        assert_eq!(
            reorder_paths_as_block(&original, &dragged, usize::MAX),
            ["a", "c", "e", "b", "d"].map(str::to_string)
        );
    }

    #[test]
    fn reorder_paths_as_block_ignores_missing_and_duplicate_drag_paths() {
        let original: Vec<String> = ["a", "b", "c"].into_iter().map(str::to_string).collect();
        let dragged: Vec<String> = ["missing", "b", "b"]
            .into_iter()
            .map(str::to_string)
            .collect();

        assert_eq!(
            reorder_paths_as_block(&original, &dragged, 2),
            ["a", "c", "b"].map(str::to_string)
        );
        assert_eq!(
            reorder_paths_as_block(&original, &["missing".to_string()], 0),
            original
        );
    }

    #[test]
    fn selection_geometry_handles_visible_cells() {
        let mut f = fence(1);
        f.rect.w = cell_w() * 3.0 + PAD * 2.0;
        f.rect.h = TITLE_H + cell_h() * 2.0 + PAD * 2.0;
        let la = layout(&f, 6);
        assert_eq!(indices_between(&la, 0, 4, 6), vec![0, 1, 2, 3, 4]);
        let r = Rect {
            x: PAD,
            y: TITLE_H + PAD,
            w: cell_w() * 2.0,
            h: cell_h(),
        };
        assert_eq!(indices_in_rect(&la, &r, 6), vec![0, 1]);
        assert_eq!(cell_rect(&la, 0).x, PAD);
    }

    #[test]
    fn display_list_dedup() {
        let f = fence(1);
        let all = vec![
            FileItem {
                name: "a.png".into(),
                path: r"C:\x\a.png".into(),
                is_dir: false,
                ext: "png".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
            FileItem {
                name: "b.png".into(),
                path: r"C:\x\b.png".into(),
                is_dir: false,
                ext: "png".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
        ];
        let mut f2 = f.clone();
        f2.pinned.push(r"C:\x\a.png".into());
        let dl = display_list(&f2, &all);
        assert_eq!(dl.len(), 2);
    }

    #[test]
    fn natural_sort_places_numeric_names_in_explorer_order() {
        let f = fence(1);
        let all = vec![
            FileItem {
                name: "item10.txt".into(),
                path: "C:\\item10.txt".into(),
                is_dir: false,
                ext: "txt".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
            FileItem {
                name: "item2.txt".into(),
                path: "C:\\item2.txt".into(),
                is_dir: false,
                ext: "txt".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
        ];
        let listed = display_list(&f, &all);
        assert_eq!(listed[0].name, "item2.txt");
        assert_eq!(listed[1].name, "item10.txt");
    }

    #[test]
    fn display_list_respects_persisted_item_order() {
        // item_order 仅在"手动"排序模式下生效(默认已是"常用")
        let mut f = fence(1);
        f.sort_mode = "手动".into();
        let all = vec![
            FileItem {
                name: "a.png".into(),
                path: "C:\\a.png".into(),
                is_dir: false,
                ext: "png".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
            FileItem {
                name: "b.png".into(),
                path: "C:\\b.png".into(),
                is_dir: false,
                ext: "png".into(),
                category: "图片".into(),
                mtime_ms: 0,
            },
        ];
        f.item_order = vec!["C:\\b.png".into(), "C:\\a.png".into()];
        let listed = display_list(&f, &all);
        assert_eq!(
            listed.iter().map(|it| it.name.as_str()).collect::<Vec<_>>(),
            vec!["b.png", "a.png"]
        );
    }

    #[test]
    fn display_list_sorts_dir_first_name() {
        let mut f = fence(1);
        f.category = "其他".into();
        let all = vec![
            FileItem {
                name: "zeta.txt".into(),
                path: r"C:\x\zeta.txt".into(),
                is_dir: false,
                ext: "txt".into(),
                category: "其他".into(),
                mtime_ms: 0,
            },
            FileItem {
                name: "alpha".into(),
                path: r"C:\x\alpha".into(),
                is_dir: true,
                ext: String::new(),
                category: "其他".into(),
                mtime_ms: 0,
            },
            FileItem {
                name: "beta.txt".into(),
                path: r"C:\x\beta.txt".into(),
                is_dir: false,
                ext: "txt".into(),
                category: "其他".into(),
                mtime_ms: 0,
            },
        ];
        let dl = display_list(&f, &all);
        let names: Vec<&str> = dl.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta.txt", "zeta.txt"]);
    }

    #[test]
    fn global_config_stable() {
        let g = build_global_config(&[]);
        assert!(g.is_empty());
    }

    #[test]
    fn icon_size_syncs_cells() {
        // 每个测试独立复位 DPI/留白,避免并行测试互相影响
        set_dpi_scale(1.0);
        set_cell_pads(43.0, 54.0);
        set_icon_size(48.0);
        assert_eq!(icon_size(), 48.0);
        assert!((cell_w() - 91.0).abs() < 0.001); // 48 + 43
        assert!((cell_h() - 102.0).abs() < 0.001); // 48 + 54
        set_icon_size(32.0);
        assert!((cell_w() - 75.0).abs() < 0.001); // 32 + 43
        set_dpi_scale(1.5);
        assert!((cell_w() - 96.5).abs() < 0.001); // 32 + 43*1.5,留白随 DPI 缩放
        set_dpi_scale(1.0);
    }

    #[test]
    fn suggested_rect_preserves_position_and_size() {
        assert_eq!(
            suggested_rect(-1920, 80, -1520, 380),
            Some(Rect {
                x: -1920.0,
                y: 80.0,
                w: 400.0,
                h: 300.0,
            })
        );
        assert_eq!(suggested_rect(10, 20, 10, 30), None);
        assert_eq!(suggested_rect(10, 20, 30, 20), None);
    }

    #[test]
    fn push_chain_right_pushes_neighbor() {
        let mut rects = vec![
            Rect {
                x: 100.0,
                y: 100.0,
                w: 200.0,
                h: 100.0,
            },
            Rect {
                x: 400.0,
                y: 100.0,
                w: 200.0,
                h: 100.0,
            },
        ];
        // 沿 x 拖动 anchor 直到与 rects[1] 冲突
        rects[0] = Rect {
            x: 250.0,
            y: 100.0,
            w: 200.0,
            h: 100.0,
        };
        push_chain(&mut rects, 0);
        let a = rects[0];
        let b = rects[1];
        assert!(!intersects(&a, &b), "overlap {:?} {:?}", a, b);
        assert!(
            gap_between(&a, &b) >= GAP - 0.01,
            "gap too small {:?} {:?}",
            a,
            b
        );
    }

    #[test]
    fn push_chain_pushes_left_when_closer() {
        // 锚在右侧、栅栏在其左侧重叠：应向左推（最小位移自适应，不限于拖动方向）
        let mut rects = vec![
            Rect {
                x: 500.0,
                y: 100.0,
                w: 100.0,
                h: 100.0,
            },
            Rect {
                x: 440.0,
                y: 100.0,
                w: 100.0,
                h: 100.0,
            },
        ];
        push_chain(&mut rects, 0);
        let a = rects[0];
        let b = rects[1];
        assert!(!intersects(&a, &b), "overlap {:?} {:?}", a, b);
        assert!(
            gap_between(&a, &b) >= GAP - 0.01,
            "gap too small {:?} {:?}",
            a,
            b
        );
        assert!(b.x < a.x, "should push left, got {:?} {:?}", a, b);
    }

    #[test]
    fn push_chain_diagonal_resolves() {
        // 对角重叠：任选一轴推出即可解除，结果不得有残留重叠
        let mut rects = vec![
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            Rect {
                x: 60.0,
                y: 60.0,
                w: 100.0,
                h: 100.0,
            },
        ];
        push_chain(&mut rects, 0);
        assert!(!intersects(&rects[0], &rects[1]), "{:?}", rects);
        assert!(
            gap_between(&rects[0], &rects[1]) >= GAP - 0.01,
            "{:?}",
            rects
        );
    }

    #[test]
    fn push_chain_keeps_anchor_still() {
        let mut rects = vec![
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            Rect {
                x: -50.0,
                y: -50.0,
                w: 120.0,
                h: 120.0,
            },
        ];
        push_chain(&mut rects, 0);
        assert_eq!(
            rects[0],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        assert!(!intersects(&rects[0], &rects[1]));
    }

    #[test]
    fn chain_positions_compact_and_wrap() {
        // [A,B,C] 等宽从 x=0 排:间距 GAP 累积
        let slots = chain_positions(
            &[(100.0, 50.0), (100.0, 50.0), (100.0, 50.0)],
            0.0,
            0.0,
            1000.0,
        );
        assert_eq!(slots[0], (0.0, 0.0));
        assert_eq!(slots[1], (112.0, 0.0));
        assert_eq!(slots[2], (224.0, 0.0));
        // 宽度不够换行:第三项 224+100 > 250 → 换到 y=50+GAP
        let slots = chain_positions(
            &[(100.0, 50.0), (100.0, 50.0), (100.0, 50.0)],
            0.0,
            0.0,
            250.0,
        );
        assert_eq!(slots[2], (0.0, 62.0));
        // 不等高换行:行底取最深者
        let slots = chain_positions(
            &[(100.0, 80.0), (100.0, 30.0), (100.0, 30.0)],
            0.0,
            0.0,
            250.0,
        );
        assert_eq!(slots[2], (0.0, 92.0));
        // 换行以绝对右缘为准:x0=100 时行宽只剩 150,第二项就换行
        let slots = chain_positions(&[(100.0, 50.0), (100.0, 50.0)], 100.0, 0.0, 250.0);
        assert_eq!(slots[0], (100.0, 0.0));
        assert_eq!(slots[1], (100.0, 62.0));
    }

    fn fit_to_screen_clamps_without_scaling() {
        // 整体超出屏幕时：不再缩放尺寸，平移+夹回即可
        let mut rects = vec![
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 1850.0,
                y: 100.0,
                w: 200.0,
                h: 300.0,
            },
        ];
        fit_to_screen(&mut rects, 0.0, 0.0, 1920.0, 1080.0);
        for r in &rects {
            assert!((r.w - 200.0).abs() < 0.01, "should not scale {:?}", r);
            assert!((r.h - 300.0).abs() < 0.01, "should not scale {:?}", r);
            assert!(r.x >= -0.01, "out of left {:?}", r);
            assert!(r.x + r.w <= 1920.01, "out of right {:?}", r);
            assert!(r.y >= -0.01, "out of top {:?}", r);
            assert!(r.y + r.h <= 1080.01, "out of bottom {:?}", r);
        }
    }

    #[test]
    fn fit_to_screen_shifts_into_view() {
        let mut rects = vec![Rect {
            x: -500.0,
            y: -300.0,
            w: 200.0,
            h: 120.0,
        }];
        fit_to_screen(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert!(
            rects[0].x >= -0.01 && rects[0].y >= -0.01,
            "shift failed {:?}",
            rects[0]
        );
    }

    #[test]
    fn settle_scenario_no_overlap() {
        // 复现场景：base 贴着屏幕右缘(1770..1920)，新建栅栏 1810..2010 交叠
        let mut rects = vec![
            Rect {
                x: 1770.0,
                y: 70.0,
                w: 150.0,
                h: 300.0,
            },
            Rect {
                x: 1810.0,
                y: 110.0,
                w: 200.0,
                h: 260.0,
            },
        ];
        push_chain(&mut rects, 0);
        push_chain(&mut rects, 1);
        fit_to_screen(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert!(
            !intersects(&rects[0], &rects[1]),
            "still overlap {:?} {:?}",
            rects[0],
            rects[1]
        );
        for r in &rects {
            assert!(r.x >= -0.01, "out of left {:?}", r);
            assert!(r.x + r.w <= 1920.01, "out of right {:?}", r);
        }
    }

    #[test]
    fn auto_layout_packs_left_to_right_wraps() {
        let mut rects = vec![
            Rect {
                x: 999.0,
                y: 999.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 300.0,
            },
        ];
        // 工作区宽 500：放两个一行（200+12+200=412），第三个换行（412+12+200=624>500）
        auto_layout(&mut rects, 0.0, 0.0, 500.0, 1040.0);
        // 左对齐：第一列 x=0
        assert!((rects[0].x - 0.0).abs() < 0.01, "{:?}", rects);
        assert!((rects[1].x - (200.0 + GAP)).abs() < 0.01, "{:?}", rects);
        // 第三个换行：x=0, y=300+GAP
        assert!((rects[2].x - 0.0).abs() < 0.01, "{:?}", rects);
        assert!((rects[2].y - (300.0 + GAP)).abs() < 0.01, "{:?}", rects);
        // 间距保持
        assert!(gap_between(&rects[0], &rects[1]) >= GAP - 0.01);
        assert!(gap_between(&rects[1], &rects[2]) >= GAP - 0.01);
    }

    #[test]
    fn auto_layout_single_fence_stays_origin() {
        let mut rects = vec![Rect {
            x: 500.0,
            y: 500.0,
            w: 200.0,
            h: 300.0,
        }];
        auto_layout(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(
            rects[0],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 300.0
            }
        );
    }

    #[test]
    fn settings_default_auto_align_on() {
        let s = Settings::default();
        assert_eq!(s.align_mode, "auto");
    }

    #[test]
    fn snap_to_grid_rounds() {
        assert_eq!(snap_to_grid(13.0, 8.0), 16.0);
        assert_eq!(snap_to_grid(12.0, 8.0), 16.0); // 1.5 格 → 远离零舍入到 2 格
        assert_eq!(snap_to_grid(-3.0, 8.0), 0.0);
        assert_eq!(snap_to_grid(13.0, 0.0), 13.0);
    }

    #[test]
    fn snap_rect_grid_clamps_min() {
        let r = Rect {
            x: 13.0,
            y: 21.0,
            w: 5.0,
            h: 9.0,
        };
        let s = snap_rect_grid(&r);
        // 网格 = 图标格（grid_x/grid_y），13/21 都吸附到 0
        assert_eq!(s.x, snap_to_grid(13.0, grid_x()));
        assert_eq!(s.y, snap_to_grid(21.0, grid_y()));
        assert!(s.w >= min_w());
        assert!(s.h >= min_h());
    }

    #[test]
    fn shrink_to_fit_shrinks_but_not_below_min() {
        let other = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 300.0,
        };
        // 被拖栅栏与 other 部分重叠：尽力缩小（位置不变、不小于最小尺寸）
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 200.0,
            h: 300.0,
        };
        let s = shrink_to_fit(&r, &[other]);
        assert_eq!(s.x, 100.0, "位置应保持不变");
        assert_eq!(s.y, 100.0);
        assert!(s.w >= min_w() - 0.01, "不能小于最小宽度 {:?}", s);
        assert!(s.h >= min_h() - 0.01, "不能小于最小高度 {:?}", s);
        assert!(s.w <= r.w && s.h <= r.h, "应缩小而非放大 {:?}", s);
    }

    #[test]
    fn shrink_then_avoid_resolves_overlap() {
        let other = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 300.0,
        };
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 200.0,
            h: 300.0,
        };
        let shrunk = shrink_to_fit(&r, &[other]);
        let final_r = avoid_overlap(&shrunk, &[other], 0.0, 0.0, 1920.0, 1040.0);
        assert!(
            !conflicts_gap(&final_r, &other),
            "shrink+avoid 后仍冲突 {:?}",
            final_r
        );
    }

    #[test]
    fn shrink_to_fit_untouched_when_no_conflict() {
        let other = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 300.0,
        };
        let r = Rect {
            x: 300.0,
            y: 300.0,
            w: 200.0,
            h: 300.0,
        };
        let s = shrink_to_fit(&r, &[other]);
        assert_eq!(s, r);
    }

    #[test]
    fn avoid_overlap_pushes_self_only() {
        let other = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 300.0,
        };
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 200.0,
            h: 300.0,
        };
        let s = avoid_overlap(&r, &[other], 0.0, 0.0, 1920.0, 1040.0);
        assert!(
            !conflicts_gap(&s, &other),
            "should resolve conflict {:?}",
            s
        );
        assert!(s.x >= -0.01 && s.x + s.w <= 1920.01, "in screen {:?}", s);
    }

    #[test]
    fn snap_below_neighbor_keeps_gap() {
        let other = Rect {
            x: 40.0,
            y: 40.0,
            w: 200.0,
            h: 300.0,
        };
        // 拖到 other 正下方，y 距 40+300+12=352 差 5，应吸附
        let r = Rect {
            x: 40.0,
            y: 347.0,
            w: 200.0,
            h: 300.0,
        };
        let s = snap_to_neighbors(&r, &[other], 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(s.y, 352.0, "{:?}", s);
        assert_eq!(s.x, 40.0);
    }

    #[test]
    fn snap_right_neighbor_keeps_gap() {
        let other = Rect {
            x: 40.0,
            y: 40.0,
            w: 200.0,
            h: 300.0,
        };
        // 拖到 other 正右方，x 距 40+200+12=252 差 4，应吸附
        let r = Rect {
            x: 248.0,
            y: 40.0,
            w: 200.0,
            h: 300.0,
        };
        let s = snap_to_neighbors(&r, &[other], 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(s.x, 252.0, "{:?}", s);
        assert_eq!(s.y, 40.0);
    }

    #[test]
    fn snap_ignores_far_neighbor() {
        let other = Rect {
            x: 40.0,
            y: 40.0,
            w: 200.0,
            h: 300.0,
        };
        let r = Rect {
            x: 400.0,
            y: 400.0,
            w: 200.0,
            h: 300.0,
        };
        let s = snap_to_neighbors(&r, &[other], 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(s.x, 400.0);
        assert_eq!(s.y, 400.0);
    }

    #[test]
    fn snap_to_screen_edge() {
        let r = Rect {
            x: 4.0,
            y: 6.0,
            w: 200.0,
            h: 300.0,
        };
        let s = snap_to_neighbors(&r, &[], 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(s.x, 0.0);
        assert_eq!(s.y, 0.0);
    }

    #[test]
    fn flow_layout_same_row_sorts_by_x() {
        // 乱序但 y 重叠 → 同行，按 x 左对齐流式排列
        let mut rects = vec![
            Rect {
                x: 280.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 520.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 40.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
        ];
        let order = flow_layout(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(order, vec![2, 0, 1]);
        assert_eq!(rects[2].x, 0.0);
        assert_eq!(rects[0].x, 212.0); // 200 + GAP
        assert_eq!(rects[1].x, 424.0);
        assert_eq!(rects[0].y, 0.0);
        assert_eq!(rects[1].y, 0.0);
        assert_eq!(rects[2].y, 0.0);
    }

    #[test]
    fn flow_layout_new_row_below() {
        // 第三个栅栏在下方（y 不重叠）→ 换行到第二行，与上行保持 GAP
        let mut rects = vec![
            Rect {
                x: 40.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 280.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 40.0,
                y: 372.0,
                w: 200.0,
                h: 300.0,
            },
        ];
        flow_layout(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(rects[2].y, 312.0); // 300 + GAP
        assert_eq!(rects[2].x, 0.0);
        assert_eq!(rects[0].y, 0.0);
        assert_eq!(rects[1].y, 0.0);
        assert_eq!(rects[1].x, 212.0);
    }

    #[test]
    fn flow_layout_short_below_short_new_row() {
        // 矮栅栏拖到矮栅栏下方：即使中心 y 差不大，y 不重叠也应换行
        let mut rects = vec![
            Rect {
                x: 40.0,
                y: 60.0,
                w: 200.0,
                h: 120.0,
            },
            Rect {
                x: 280.0,
                y: 60.0,
                w: 200.0,
                h: 120.0,
            },
            Rect {
                x: 40.0,
                y: 200.0,
                w: 200.0,
                h: 120.0,
            },
        ];
        flow_layout(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert_eq!(rects[2].y, 132.0); // 120 + GAP
        assert_eq!(rects[2].x, 0.0);
        assert_eq!(rects[0].y, 0.0);
        assert_eq!(rects[1].x, 212.0);
    }

    #[test]
    fn flow_layout_keeps_gap() {
        let mut rects = vec![
            Rect {
                x: 40.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 280.0,
                y: 60.0,
                w: 200.0,
                h: 300.0,
            },
            Rect {
                x: 40.0,
                y: 372.0,
                w: 200.0,
                h: 300.0,
            },
        ];
        flow_layout(&mut rects, 0.0, 0.0, 1920.0, 1040.0);
        assert!((gap_between(&rects[0], &rects[1]) - GAP).abs() < 0.01);
        assert!((gap_between(&rects[0], &rects[2]) - GAP).abs() < 0.01);
    }

    #[test]
    fn scrollbar_hit_when_overflow() {
        let mut f = fence(1);
        f.rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 240.0,
        };
        let la = layout(&f, 50);
        assert!(la.total_rows > la.rows);
        let h = hit_test(&f, &la, 194.0, 60.0, 50);
        assert_eq!(h, Hit::Scrollbar);
        let la2 = layout(&f, 4);
        assert!(la2.total_rows <= la2.rows);
        let h2 = hit_test(&f, &la2, 194.0, 60.0, 4);
        assert_ne!(h2, Hit::Scrollbar);
    }

    // ---------- 拖拽落位(2026-09-02 行内槽位模型) ----------

    fn rr(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// 两行基准布局(7 成员,被拖者 A 固定 a_idx=6):
    /// 上行 [M0(0,0,244) M1(256,0,244) M2(512,0,132)],
    /// 下行 [M3(0,112,244) M4(256,112,132) M5(400,112,132)]
    fn two_row_layout(a: Rect) -> Vec<Rect> {
        vec![
            rr(0.0, 0.0, 244.0, 100.0), // 0 M0
            rr(256.0, 0.0, 244.0, 100.0), // 1 M1
            rr(512.0, 0.0, 132.0, 100.0), // 2 M2
            rr(0.0, 112.0, 244.0, 100.0), // 3 M3
            rr(256.0, 112.0, 132.0, 100.0), // 4 M4
            rr(400.0, 112.0, 132.0, 100.0), // 5 M5
            a,                          // 6 A(被拖者)
        ]
    }

    fn assert_no_overlap(rects: &[Rect], pos: &[(f32, f32)]) {
        // 用移动后的位置重新聚类,再逐行检查相邻成员无重叠
        let moved: Vec<Rect> = rects
            .iter()
            .zip(pos)
            .map(|(r, (x, y))| rr(*x, *y, r.w, r.h))
            .collect();
        let rows = rows_from_rects(&moved);
        for row in &rows {
            for w in row.windows(2) {
                let (a, b) = (&moved[w[0]], &moved[w[1]]);
                assert!(
                    b.x >= a.x + a.w,
                    "overlap: ({},{}) vs ({},{})",
                    a.x,
                    a.y,
                    b.x,
                    b.y
                );
            }
        }
    }

    #[test]
    fn insert_same_row_before_member_rotates() {
        // A(700,0) 在上行末尾,插到 M2(512) 之前(槽位2):
        // A 接管 M2 的槽(512),M2 右移到行尾新槽(656);下行不动
        let rects = two_row_layout(rr(700.0, 0.0, 132.0, 100.0));
        let (pos, land) = row_insert_layout(&rects, 6, (0, 2));
        assert_eq!(land, (512.0, 0.0));
        assert_eq!(pos[0], (0.0, 0.0));
        assert_eq!(pos[1], (256.0, 0.0));
        assert_eq!(pos[2], (656.0, 0.0));
        assert_eq!(pos[6], (512.0, 0.0));
        // 下行不动
        assert_eq!(pos[3], (0.0, 112.0));
        assert_eq!(pos[4], (256.0, 112.0));
        assert_eq!(pos[5], (400.0, 112.0));
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn insert_top_to_bottom_before_member() {
        // 上→下:A(700,0) 插到下行 M4(256) 之前(行1槽位1):
        // 上行 A 在行尾拔出不挪任何人;下行 [M3,A,M4,M5] @ 0/256/400/544
        let rects = two_row_layout(rr(700.0, 0.0, 132.0, 100.0));
        let (pos, land) = row_insert_layout(&rects, 6, (1, 1));
        assert_eq!(land, (256.0, 112.0));
        assert_eq!(pos[0], (0.0, 0.0));
        assert_eq!(pos[1], (256.0, 0.0));
        assert_eq!(pos[2], (512.0, 0.0));
        assert_eq!(pos[3], (0.0, 112.0));
        assert_eq!(pos[4], (400.0, 112.0));
        assert_eq!(pos[5], (544.0, 112.0));
        assert_eq!(pos[6], (256.0, 112.0));
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn insert_bottom_to_top_after_member() {
        // 下→上:A(700,112) 插到上行 M1(256) 之后(行0槽位2):
        // A 接管 M2 的槽(512),M2 右移;下行 A 在行尾拔出,其余原位
        let rects = two_row_layout(rr(700.0, 112.0, 132.0, 100.0));
        let (pos, land) = row_insert_layout(&rects, 6, (0, 2));
        assert_eq!(land, (512.0, 0.0));
        assert_eq!(pos[0], (0.0, 0.0));
        assert_eq!(pos[1], (256.0, 0.0));
        assert_eq!(pos[2], (656.0, 0.0));
        assert_eq!(pos[3], (0.0, 112.0));
        assert_eq!(pos[4], (256.0, 112.0));
        assert_eq!(pos[5], (400.0, 112.0));
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn insert_bottom_to_top_before_first() {
        // 下→上:A(700,112) 插到上行 M0 之前(行0槽位0):
        // 上行全体右移 [A,M0,M1,M2] @ 0/144/400/656;下行不动
        let rects = two_row_layout(rr(700.0, 112.0, 132.0, 100.0));
        let (pos, land) = row_insert_layout(&rects, 6, (0, 0));
        assert_eq!(land, (0.0, 0.0));
        assert_eq!(pos[0], (144.0, 0.0));
        assert_eq!(pos[1], (400.0, 0.0));
        assert_eq!(pos[2], (656.0, 0.0));
        assert_eq!(pos[3], (0.0, 112.0));
        assert_eq!(pos[4], (256.0, 112.0));
        assert_eq!(pos[5], (400.0, 112.0));
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn insert_at_row_end_extends_row() {
        // A(0,300) 独占一行,插到下行行尾(行1槽位3):
        // A 落在 M5 之后 (544,112);原行只有 A,拔出无影响
        let rects = two_row_layout(rr(0.0, 300.0, 132.0, 100.0));
        let (pos, land) = row_insert_layout(&rects, 6, (1, 3));
        assert_eq!(land, (544.0, 112.0));
        assert_eq!(pos[3], (0.0, 112.0));
        assert_eq!(pos[4], (256.0, 112.0));
        assert_eq!(pos[5], (400.0, 112.0));
        assert_eq!(pos[6], (544.0, 112.0));
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn three_rows_move_middle_to_third() {
        // 三层:中行 A(256,212) 插到第三行 G(512) 之前:
        // 第三行 [A,G] @ 512/656;中行另一成员 M1 不动
        let rects = vec![
            rr(0.0, 212.0, 244.0, 100.0), // 0 M1(中行)
            rr(512.0, 424.0, 132.0, 100.0), // 1 G(第三行)
            rr(256.0, 212.0, 132.0, 100.0), // 2 A(中行)
        ];
        let (pos, land) = row_insert_layout(&rects, 2, (2, 0));
        assert_eq!(land, (512.0, 424.0));
        assert_eq!(pos[1], (656.0, 424.0)); // G 右移一格
        assert_eq!(pos[0], (0.0, 212.0)); // 中行成员不动
        assert_no_overlap(&rects, &pos);
    }

    #[test]
    fn row_slot_counts_left_members() {
        let rects = two_row_layout(rr(700.0, 0.0, 132.0, 100.0));
        let rows = rows_from_rects(&rects);
        assert_eq!(rows.len(), 2);
        // 行内任意点:中心在 M4 与 M5 之间(394) → 行1槽位2
        assert_eq!(row_slot_of(&rects, &rows, (394.0, 162.0)), (1, 2));
        // 中心在 M0 左侧 → 行0槽位0
        assert_eq!(row_slot_of(&rects, &rows, (10.0, 50.0)), (0, 0));
        // 距行中心很远 = 自由区
        assert!(nearest_row_distance(&rects, &rows, 2000.0) > 300.0);
    }
}
