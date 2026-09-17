//! 壁纸快照的纯逻辑与缓存 IO（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
//! 像素比对纯核（黑帧判定/回退判定/栅栏覆盖区增量比对，集成测试经本模块直测）
//! 与 wallpaper.bin 持久化缓存读写。
//! 叶子模块——只依赖 std / crate::model / crate::render / crate::logging，不依赖
//! crate::ui；壁纸捕获与跟随/追赶调度（state 耦合）仍在 ui.rs。

use crate::logging::log;
use crate::model::{self, Fence};
use crate::render;

/// 比较新旧快照内容。**带每通道 8 的容差**:PrintWindow 捕获的壁纸亮度
/// 存在 ~4% 的时序波动(ICC/伽马路径),逐字节严格比较会把波动当成
/// "壁纸变了",触发无谓的全量重绘——栅栏区域整面 4% 亮度先跳再回,
/// 正是用户看到的"闪"。真换壁纸是整图替换,容差不影响判别。
/// 黑帧判定(纯核,2026-09-16 提取供集成测试):前 4096 像素全是不透明黑
/// 且尺寸足够大 = 壁纸切换过渡期的 DWM 暂态黑帧。这种帧绝不能计入
/// wallpaper_fails(会堆积触发"回退透明"误落盘)。空缓冲不算(空迭代器
/// all() 全真, vacuous-true 会让"零字节捕获"逃避失败计数)。
pub fn is_black_frame(px: &[u8], w: u32, h: u32) -> bool {
    !px.is_empty()
        && w > 64
        && h > 64
        && px
            .as_chunks::<4>()
            .0
            .iter()
            .take(4096)
            .all(|c| c[0] == 0 && c[1] == 0 && c[2] == 0 && c[3] == 255)
}

/// 精确模式→透明回退判定(纯核):必须"当前没有任何可用快照"且失败计数
/// 达阈值且过了 10s 启动宽限——登录早期/壁纸切换过渡期的瞬态失败绝不能
/// 把"精确"误落盘成"透明"。
pub fn capture_fallback_due(has_snapshot: bool, fails: u32, elapsed_ms: u64) -> bool {
    !has_snapshot && fails >= 2 && elapsed_ms > 10_000
}

/// 比较新旧快照在"栅栏覆盖区域"内是否有实质变化(每通道 8 容差,理由:
/// PrintWindow 捕获亮度存在 ~4% 时序波动,严格比较会把波动当成变化)。
/// 栅栏区域之外的变化(如动态时钟壁纸的分钟跳动)不影响渲染——ink 常驻
/// 下快照只作标签种子,栅栏外的壁纸像素从不参与任何绘制——因此不触发
/// 重绘与缓存落盘,避免时钟壁纸下的每分钟空转(全量重绘+9MB 落盘+闪风险)。
/// 宿主几何(数量/尺寸/原点)变化仍视为整体变化;无栅栏时退化为全图比较。
pub fn wallpaper_changed_under_fences(
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

pub fn px_differs(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return true;
    }
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        if pa[0].abs_diff(pb[0]) > 8 || pa[1].abs_diff(pb[1]) > 8 || pa[2].abs_diff(pb[2]) > 8 {
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

pub(crate) fn load_wallpaper_cache() -> Option<Vec<render::WallpaperPixels>> {
    let caps = load_wallpaper_cache_inner();
    // 缓存文件在却解析不过(损坏/旧版格式)→每次启动都得现场捕获、首帧
    // 变慢,无日志则无从排查;文件不存在=首次启动,保持静默。
    if caps.is_none() && wallpaper_cache_path().exists() {
        log("wallpaper cache present but invalid, ignored");
    }
    caps
}

fn load_wallpaper_cache_inner() -> Option<Vec<render::WallpaperPixels>> {
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
