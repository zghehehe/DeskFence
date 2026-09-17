//! DPI 与桌面格距度量（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
//! 系统/窗口 DPI、桌面图标尺寸与 SysListView32 格距探测（粘性缓存）、
//! 行列保持 sidecar（layout_cells.json）读写、DpiMetrics 组装。
//! 叶子模块——只依赖 std / windows crate / crate::model / crate::shell /
//! crate::hosts / crate::logging，不依赖 crate::ui。
//! 注意：本模块 dpi_scale() 是 GetDpiForSystem 的实时读取，与 model::dpi_scale()
//! （boot 缓存值）语义不同，勿合并。

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hosts::desktop_listview;
use crate::logging::log;
use crate::model;
use crate::shell;

/// 系统 DPI 缩放系数（进程已 SetProcessDPIAware，坐标系为物理像素）
pub(crate) fn dpi_scale() -> f32 {
    unsafe { GetDpiForSystem() as f32 / 96.0 }.max(1.0)
}

/// 行列保持 sidecar(2026-09-16):config.json 存像素矩形,图标尺寸/机器
/// 变化后像素沿用会让行列数漂移("高度5行"的根源)。这里记下保存配置时的
/// 格距,启动时与当前格距不同则按行列等比换算(rescale_rects_to_cells)。
fn layout_cells_path() -> std::path::PathBuf {
    model::config_dir().join("layout_cells.json")
}

pub(crate) fn load_layout_cells() -> Option<(f32, f32)> {
    let t = std::fs::read_to_string(layout_cells_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    Some((v["cell_w"].as_f64()? as f32, v["cell_h"].as_f64()? as f32))
}

pub(crate) fn save_layout_cells() {
    let (cw, ch) = (model::cell_w(), model::cell_h());
    let json = format!("{{\n  \"cell_w\": {cw},\n  \"cell_h\": {ch}\n}}\n");
    // sidecar 写失败=下次启动行列保持静默失效(图标尺寸/机器变化后行列
    // 漂移),留一行现场;仅启动/尺寸同步时回写,不会刷屏。
    if let Err(e) = std::fs::write(layout_cells_path(), &json) {
        log(&format!("layout cells sidecar save failed: {e}"));
    }
}

/// 与桌面图标一致的物理像素尺寸：优先实测桌面列表视图的图标格距
/// （LVM_GETITEMSPACING 返回值即格宽/格高，跨进程可用、不受注册表值过期影响）。
/// 注意:水平方向格宽可直接拆分；垂直格高包含标题带，不能把它
/// 当作纯图标留白再次相加。失败时回退到当前注册表值。
pub(crate) fn current_icon_size() -> f32 {
    if let Some((cell_w_px, cell_h_px)) = probe_desktop_item_spacing() {
        // 图标本体:注册表 IconSize(Explorer 在 Ctrl+滚轮时会写入;
        // 缺失=Windows 默认中图标 48,见 shell::desktop_icon_size)×DPI。
        // 用实测格距反推留白,保证格宽格高与原生逐像素一致
        // (垂直格高含文字区,不能用注册表 IconVerticalSpacing 直接算)
        let icon = shell::desktop_icon_size() * dpi_scale();
        if (16.0..=256.0).contains(&icon) && cell_w_px > icon && cell_h_px > icon {
            let pad_x = ((cell_w_px - icon) / dpi_scale()).clamp(16.0, 96.0);
            // Explorer's vertical spacing includes the caption band. Keep the
            // measured cell height instead of adding the icon size twice.
            // 下限钳 MIN_PAD_Y:标签带放不下时下一行会压住上一行文字
            let pad_y = ((cell_h_px - icon) / dpi_scale()).clamp(model::MIN_PAD_Y, 96.0);
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
pub(crate) fn metrics_for_window(hwnd: HWND) -> model::DpiMetrics {
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
/// 图标格距探测缓存:键=(注册表 IconSize, 系统 DPI),值=(格距, 残余偏移)
type SpacingCache = Option<((f32, u32), (f32, f32))>;
static ITEM_SPACING_CACHE: Mutex<SpacingCache> = Mutex::new(None);

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
    // SAFETY: lv 是现查的现存桌面列表视图；跨进程消息 LVM_GETITEMSPACING
    // 经 SendMessageTimeoutW 同步发送，res 是栈 [out] 槽位（超时即失败
    // 返回 0，不悬等）；返回值按 LOWORD/HIWORD 解码。
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
