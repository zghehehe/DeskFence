//! Direct2D + DirectWrite 渲染层：栅栏卡片、图标、标题、滚动条

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;

use windows::Win32::Foundation::{BOOL, COLORREF, HANDLE, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_GDI_CLASSIC,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, DrawTextW,
    GetDC, GetDIBits, ReleaseDC, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA,
    BACKGROUND_MODE, BITMAPINFO, BLENDFUNCTION, DIB_RGB_COLORS, DT_CENTER, DT_EDITCONTROL,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_WORDBREAK, HBITMAP, HBRUSH, HDC, HGDIOBJ, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, UpdateLayeredWindow, DI_NORMAL, HICON, ULW_ALPHA,
};

use crate::model::{self, Fence, FileItem, Layout};
use crate::shell;

pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub dc: HDC,
    pub dib: HBITMAP,
    pub old: HGDIOBJ,
    /// DIB 首像素指针(BGRA,预乘)。精确模式下 GDI 画完文字后用它统一置 alpha=255。
    pub bits: *mut std::ffi::c_void,
    pub target: ID2D1DCRenderTarget,
}

// bits 是纯内存指针,仅在 state 互斥锁内于 UI 线程访问;COM 接口本身可跨线程传递。
unsafe impl Send for Surface {}

pub struct Renderer {
    pub factory: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub title_fmt: IDWriteTextFormat,
    pub name_fmt: IDWriteTextFormat,
    /// 标题栏折叠箭头专用(比标题字大一档,更醒目)
    /// 系统浅色主题(决定文字/阴影配色)
    pub light: bool,
    /// Windows 系统强调色(悬停描边/滚动条)
    pub accent: [f32; 3],
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

fn rounded(r: D2D_RECT_F, rad: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: r,
        radiusX: rad,
        radiusY: rad,
    }
}

pub fn as_brush<'a>(s: &'a ID2D1SolidColorBrush) -> &'a ID2D1Brush {
    unsafe { &*(s as *const ID2D1SolidColorBrush as *const ID2D1Brush) }
}

unsafe fn brush(rt: &ID2D1DCRenderTarget, c: &D2D1_COLOR_F) -> Option<ID2D1SolidColorBrush> {
    rt.CreateSolidColorBrush(c, None).ok()
}

unsafe fn create_text_format(
    dw: &IDWriteFactory,
    family_name: &str,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Option<IDWriteTextFormat> {
    let family = shell::wide(family_name);
    let locale = shell::wide("zh-CN");
    dw.CreateTextFormat(
        PCWSTR::from_raw(family.as_ptr()),
        None,
        weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        PCWSTR::from_raw(locale.as_ptr()),
    )
    .ok()
}

/// GDI 图标标题字号(lfHeight 负值 = 字符单元像素高) → DWrite em 字号。
/// GDI 的 |lfHeight| 对应 ascent+descent 的字符格高,而 DWrite SetFontSize 是
/// em 高;同一数字直接传给 DWrite 会让名字比原生桌面大约 1/3。用字体实际
/// metrics 换算才能与原生逐像素对齐(Segoe UI≈1.33,微软雅黑≈1.32)。
unsafe fn gdi_px_to_em(_dw: &IDWriteFactory, _family: &str, px: f32) -> f32 {
    // GDI lfHeight(负值=字符高,已含 DPI 缩放) 与 DWrite em 的正确换算:
    // 渲染目标为 96 DPI(1 DIP=1px)时,DIP 字号 = |lfHeight|,直接使用。
    // (曾错误地除以字体 (ascent+descent)/em 比率,导致名字比原生小约 30%)
    px.clamp(6.0, 64.0)
}

impl Renderer {
    pub fn new() -> Option<Renderer> {
        unsafe {
            let factory: ID2D1Factory =
                match D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) {
                    Ok(f) => f,
                    Err(_) => return None,
                };
            let dw: IDWriteFactory =
                match DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) {
                    Ok(f) => f,
                    Err(_) => return None,
                };
            // UI state updates this global scale from GetDpiForWindow before a
            // renderer rebuild. The model is currently process-global, so all
            // fences share the DPI of the window that most recently changed.
            let text_scale = model::dpi_scale();
            let (icon_family, icon_px, icon_weight) = shell::desktop_icon_font();
            let title_fmt = create_text_format(
                &dw,
                "Segoe UI",
                11.0 * text_scale,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
            )?;
            // 图标名与原生桌面一致:desktop_icon_font 返回 GDI 字符格像素高
            // (lfHeight 负值,已含 DPI 缩放),DWrite 需按字体 metrics 换算成
            // em 字号,否则名字比原生大约 1/3(且不能再乘 DPI,避免双重放大)
            let name_em = gdi_px_to_em(&dw, &icon_family, icon_px.clamp(8.0, 48.0));
            let name_fmt =
                create_text_format(&dw, &icon_family, name_em, DWRITE_FONT_WEIGHT(icon_weight))?;
            let light = shell::is_light_theme();
            let accent = shell::system_accent();
            Some(Renderer {
                factory,
                dw,
                title_fmt,
                name_fmt,
                light,
                accent,
            })
        }
    }

    /// 重读系统主题与强调色(定时器里调用,主题切换后即时跟随)
    pub fn refresh_theme(&mut self) {
        self.light = shell::is_light_theme();
        self.accent = shell::system_accent();
    }
}

/// 为栅栏窗口创建 DIB + DCRenderTarget
pub fn create_surface(factory: &ID2D1Factory, w: u32, h: u32) -> Option<Surface> {
    unsafe {
        let hdc_dst = GetDC(HWND(0));
        if hdc_dst.0 == 0 {
            return None;
        }
        let dc = CreateCompatibleDC(hdc_dst);
        ReleaseDC(HWND(0), hdc_dst);
        if dc.0 == 0 {
            return None;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize =
            std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        {
            let old = SelectObject(dc, dib);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            if let Ok(target) = factory.CreateDCRenderTarget(&props) {
                let rc = RECT {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: h as i32,
                };
                if target.BindDC(dc, &rc).is_ok() {
                    return Some(Surface {
                        w,
                        h,
                        dc,
                        dib,
                        old,
                        bits,
                        target,
                    });
                }
            }
            SelectObject(dc, old);
            let _ = DeleteObject(dib);
        }
        let _ = DeleteDC(dc);
        None
    }
}

/// 从 HICON 提取 size x size premultiplied BGRA 像素
pub fn icon_pixels(hicon: HICON, size: u32) -> Option<Vec<u8>> {
    unsafe {
        let hdc_screen = GetDC(HWND(0));
        if hdc_screen.0 == 0 {
            return None;
        }
        let dc = CreateCompatibleDC(hdc_screen);
        ReleaseDC(HWND(0), hdc_screen);
        if dc.0 == 0 {
            return None;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize =
            std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size as i32;
        bmi.bmiHeader.biHeight = -(size as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Ok(hbm) = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        {
            let old = SelectObject(dc, hbm);
            let _ = DrawIconEx(
                dc,
                0,
                0,
                hicon,
                size as i32,
                size as i32,
                0,
                HBRUSH(0),
                DI_NORMAL,
            );
            let out = {
                let src = std::slice::from_raw_parts(bits as *const u8, (size * size * 4) as usize);
                src.to_vec()
            };
            SelectObject(dc, old);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(dc);
            return Some(out);
        }
        let _ = DeleteDC(dc);
        None
    }
}

/// Read an exact-size Shell HBITMAP into top-down 32-bit BGRA pixels.
fn bitmap_pixels(bitmap: HBITMAP, size: u32) -> Option<Vec<u8>> {
    unsafe {
        let hdc = GetDC(HWND(0));
        if hdc.0 == 0 {
            let _ = DeleteObject(bitmap);
            return None;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize =
            std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size as i32;
        bmi.bmiHeader.biHeight = -(size as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;
        let mut out = vec![0u8; (size * size * 4) as usize];
        let lines = GetDIBits(
            hdc,
            bitmap,
            0,
            size,
            Some(out.as_mut_ptr() as *mut std::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(HWND(0), hdc);
        let _ = DeleteObject(bitmap);
        if lines == size as i32 {
            Some(out)
        } else {
            None
        }
    }
}

pub type IconBuffer = Vec<u8>;

/// 精确模式壁纸快照:桌面宿主窗口(Progman/WorkerW)的合成像素。
/// 屏幕坐标系,origin 为宿主左上角; fences 用它裁剪出"背后壁纸"作为不透明底。
pub struct WallpaperPixels {
    pub px: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

/// 精确模式下交给 GDI 绘制的图标名作业(客户区坐标)
pub struct GdiLabelJob {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// comctl32 v6 的 DrawShadowText 原型(ListView 图标名同款绘制 API)。
/// 注意:不能静态链接导入——exe 未嵌 SxS 清单时加载器绑到 comctl32 v5.82,
/// 该导出不存在,整个进程会以 STATUS_ENTRYPOINT_NOT_FOUND 拒绝启动。
/// 因此这里运行时 GetProcAddress,拿不到就降级为两遍 DrawTextW。
type DrawShadowTextProc = unsafe extern "system" fn(
    HDC,
    *const u16, // LPCWSTR pszText
    i32,        // cch
    *const RECT,
    u32,      // dwFlags
    COLORREF, // crText
    COLORREF, // crShadow(高字节=阴影不透明度)
    i32,      // ixOffset
    i32,      // iyOffset
) -> i32;

static DRAW_SHADOW_TEXT: OnceLock<Option<DrawShadowTextProc>> = OnceLock::new();

fn draw_shadow_text_proc() -> Option<DrawShadowTextProc> {
    *DRAW_SHADOW_TEXT.get_or_init(|| unsafe {
        use windows::core::PCSTR;
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let name = shell::wide("comctl32.dll");
        let dll = LoadLibraryW(PCWSTR::from_raw(name.as_ptr())).ok()?;
        let proc = GetProcAddress(dll, PCSTR(b"DrawShadowText\0".as_ptr()))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            DrawShadowTextProc,
        >(proc))
    })
}

/// ink 常驻精确模式:在透明表面的标签矩形上烘焙 GDI ClearType 文字。
/// 过程(与旧精确模式同渲染器、同参数,输出逐位同源):
/// 1) 把矩形内像素垫成"真实背景色"种子 = 表面覆盖层预乘色 P 合成到
///    不透明快照壁纸色 W 上(P + W*(1-a),即 hover/选中/边框压在壁纸上的
///    straight 结果——与旧模式 D2D 先把覆盖层画到不透明壁纸底上完全等价);
/// 2) 用同一 DrawShadowText/ClearType 对种子画字(边缘色与原生同源);
/// 3) RGB 与种子有差异的像素=墨水,置 alpha=255;其余像素恢复垫种子前
///    的原状(透明底+覆盖层)——背景透出实时壁纸。
/// 壁纸切换期间种子暂为旧快照(墨水边缘色停在旧底色版本),与原生过渡
/// 期行为一致;快照重捕获完成后 refresh 即换新种子。
/// fence_x/fence_y 为栅栏呈现位置(与 present 的取整一致),用于把
/// 标签矩形映射到快照坐标系。
pub fn gdi_draw_labels_seeded(
    s: &Surface,
    jobs: &[GdiLabelJob],
    wp: Option<&WallpaperPixels>,
    fence_x: i32,
    fence_y: i32,
) {
    if jobs.is_empty() {
        return;
    }
    let Some(lf) = shell::icon_title_logfont() else {
        return;
    };
    let shadow_fn = draw_shadow_text_proc();
    let sw = s.w as usize;
    let sh = s.h as usize;
    if s.bits.is_null() || sw == 0 || sh == 0 {
        return;
    }
    unsafe {
        let hf = CreateFontIndirectW(&lf as *const _);
        if hf.is_invalid() {
            return;
        }
        let old = SelectObject(s.dc, HGDIOBJ(hf.0));
        let old_bk: i32 = SetBkMode(s.dc, TRANSPARENT);
        let fmt = DT_CENTER | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS | DT_NOPREFIX;
        let flags = fmt.0;
        let bits = std::slice::from_raw_parts_mut(s.bits as *mut u8, sw * sh * 4);
        for job in jobs {
            let w16 = shell::wide(&job.text);
            if w16.len() <= 1 {
                continue;
            }
            let rc = RECT {
                left: job.x.round() as i32,
                top: job.y.round() as i32,
                right: (job.x + job.w).round() as i32,
                bottom: (job.y + job.h).round() as i32,
            };
            if rc.right <= rc.left || rc.bottom <= rc.top {
                continue;
            }
            let text = &w16[..w16.len() - 1];
            let x0 = rc.left.max(0) as usize;
            let y0 = rc.top.max(0) as usize;
            let x1 = (rc.right as usize).min(sw);
            let y1 = (rc.bottom as usize).min(sh);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let rw = x1 - x0;
            let rh = y1 - y0;
            let mut pre: Vec<u8> = vec![0u8; rw * rh * 4];
            let mut seed: Vec<u8> = vec![0u8; rw * rh * 4];
            // 垫种子:pre=垫前原状(预乘),seed=覆盖层合成到壁纸上的 straight 色
            for yy in 0..rh {
                let srow = (y0 + yy) * sw + x0;
                let wy = fence_y + (y0 + yy) as i32 - wp.map(|w| w.origin_y).unwrap_or(0);
                for xx in 0..rw {
                    let so = (srow + xx) * 4;
                    let po = (yy * rw + xx) * 4;
                    pre[po] = bits[so];
                    pre[po + 1] = bits[so + 1];
                    pre[po + 2] = bits[so + 2];
                    pre[po + 3] = bits[so + 3];
                    // 快照壁纸色(直色);无快照或超出覆盖范围按黑种子兜底
                    let wx = fence_x + (x0 + xx) as i32 - wp.map(|w| w.origin_x).unwrap_or(0);
                    let (wb, wg, wr) = match wp {
                        Some(w)
                            if wx >= 0
                                && wy >= 0
                                && (wx as u32) < w.w
                                && (wy as u32) < w.h =>
                        {
                            let wo = ((wy as u32 * w.w + wx as u32) * 4) as usize;
                            (w.px[wo], w.px[wo + 1], w.px[wo + 2])
                        }
                        _ => (0u8, 0u8, 0u8),
                    };
                    let inv = 255 - pre[po + 3] as u32;
                    let sb = (pre[po] as u32 + (wb as u32 * inv + 127) / 255).min(255) as u8;
                    let sg = (pre[po + 1] as u32 + (wg as u32 * inv + 127) / 255).min(255) as u8;
                    let sr = (pre[po + 2] as u32 + (wr as u32 * inv + 127) / 255).min(255) as u8;
                    seed[po] = sb;
                    seed[po + 1] = sg;
                    seed[po + 2] = sr;
                    seed[po + 3] = 255;
                    bits[so] = sb;
                    bits[so + 1] = sg;
                    bits[so + 2] = sr;
                    bits[so + 3] = 255;
                }
            }
            if let Some(draw) = shadow_fn {
                // 原生桌面图标名:白字 + 1px 偏移黑色阴影(高字节=阴影不透明度,约 55%)
                let _ = draw(
                    s.dc,
                    text.as_ptr(),
                    text.len() as i32,
                    &rc,
                    flags,
                    COLORREF(0x00FF_FFFF),
                    COLORREF(0x8C00_0000),
                    1,
                    1,
                );
            } else {
                // 降级(无 comctl32 v6):黑色偏移一遍 + 白色正文一遍
                let mut shifted = RECT {
                    left: rc.left + 1,
                    top: rc.top + 1,
                    right: rc.right + 1,
                    bottom: rc.bottom + 1,
                };
                let mut main_rc = rc;
                let mut buf = text.to_vec();
                SetTextColor(s.dc, COLORREF(0x0000_0000));
                let _ = DrawTextW(s.dc, &mut buf, &mut shifted, fmt);
                let mut buf2 = text.to_vec();
                SetTextColor(s.dc, COLORREF(0x00FF_FFFF));
                let _ = DrawTextW(s.dc, &mut buf2, &mut main_rc, fmt);
            }
            // 墨水判定与还原,分三类(2026-08-26 阴影墨水化):
            // 1) RGB==种子:未画到 → 恢复垫种子前的原状(隐形底/覆盖层);
            // 2) RGB 全通道变暗:阴影像素(纯黑按覆盖率压暗) → 反推覆盖率
            //    c=1-D/S,输出"纯黑+c"的背景无关墨水(叠加表面覆盖层贡献)。
            //    覆盖率与种子取值无关——旧种子提取的覆盖率先行正确,换壁纸
            //    瞬间阴影即精确,重烘焙时阴影不再变化(消除"1s 后阴影跳变");
            // 3) 其余(变亮/混合):字形墨水(白字+ClearType 边缘需要底色
            //    知识) → 保持烘焙 alpha=255;重烘焙仅 1px 边缘换色,不可感知。
            for yy in 0..rh {
                let srow = (y0 + yy) * sw + x0;
                for xx in 0..rw {
                    let so = (srow + xx) * 4;
                    let po = (yy * rw + xx) * 4;
                    let db = bits[so] as i32;
                    let dg = bits[so + 1] as i32;
                    let dr = bits[so + 2] as i32;
                    let sb = seed[po] as i32;
                    let sg = seed[po + 1] as i32;
                    let sr = seed[po + 2] as i32;
                    if db == sb && dg == sg && dr == sr {
                        bits[so] = pre[po];
                        bits[so + 1] = pre[po + 1];
                        bits[so + 2] = pre[po + 2];
                        bits[so + 3] = pre[po + 3];
                        continue;
                    }
                    if db < sb && dg < sg && dr < sr {
                        // 阴影:覆盖率反推(三通道平均,黑色阴影逐通道一致;
                        // db<sb 已保证 S>=1 无除零)
                        let cov = ((1.0 - db as f32 / sb as f32)
                            + (1.0 - dg as f32 / sg as f32)
                            + (1.0 - dr as f32 / sr as f32))
                            / 3.0;
                        let a = (cov.clamp(0.0, 1.0) * 255.0).round() as u32;
                        // 墨水叠在表面覆盖层之上:保留其贡献
                        // premult_out = P*(1-c);alpha_out = 1-(1-a_p)(1-c)
                        let inv = 255 - a;
                        let pa = pre[po + 3] as u32;
                        let out_a = 255 - (255 - pa) * inv / 255;
                        bits[so] = (pre[po] as u32 * inv / 255) as u8;
                        bits[so + 1] = (pre[po + 1] as u32 * inv / 255) as u8;
                        bits[so + 2] = (pre[po + 2] as u32 * inv / 255) as u8;
                        bits[so + 3] = out_a as u8;
                    } else {
                        bits[so + 3] = 255;
                    }
                }
            }
        }
        SetBkMode(s.dc, BACKGROUND_MODE(old_bk as u32));
        SelectObject(s.dc, old);
        let _ = DeleteObject(HGDIOBJ(hf.0));
    }
}

/// 图标像素缓存。主路径按目标物理像素直接向 Shell 请求原生 bitmap；
/// 旧 HICON 路径仅作兼容降级，避免低分辨率图标被强制放大。
pub fn get_icon_buffer(
    cache: &mut HashMap<String, IconBuffer>,
    path: &str,
    target_px: f32,
) -> IconBuffer {
    let px = target_px.round().clamp(16.0, 256.0) as u32;
    let cache_key = format!("{path}\0{px}");
    if let Some(b) = cache.get(&cache_key) {
        return b.clone();
    }
    let t0 = std::time::Instant::now();
    let buf = shell::get_system_icon_hicon(path, px)
        .and_then(|h| {
            let out = icon_pixels(h, px);
            unsafe {
                let _ = DestroyIcon(h);
            }
            out
        })
        .or_else(|| shell::get_icon_bitmap(path, px).and_then(|b| bitmap_pixels(b, px)))
        .or_else(|| {
            shell::get_icon_hicon(path).and_then(|h| {
                let out = icon_pixels(h, px);
                unsafe {
                    let _ = DestroyIcon(h);
                }
                out
            })
        })
        .unwrap_or_default();
    ICON_EXTRACT_MS.fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
    ICON_EXTRACT_COUNT.fetch_add(1, Ordering::Relaxed);
    let _ = cache.insert(cache_key, buf.clone());
    buf
}

/// 图标提取(缓存未命中)累计耗时/次数,启动诊断"首帧慢"用
pub static ICON_EXTRACT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ICON_EXTRACT_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// 绘制整个栅栏到表面。
/// 默认完全透明(与原生桌面一致);悬停或拖动时浮现极淡卡片与标题/滚动条等 chrome。
/// ink 常驻渲染(2026-08-26):栅栏背景不烙壁纸快照,铺 1/255 隐形底
/// (ULW 按逐像素 alpha 命中,保证鼠标可点)——真壁纸从窗口底下逐帧透出,
/// 换壁纸时背景与桌面同帧跟随(DWM 合成),栅栏无需任何"换底"动作。
/// 图标名统一收集为作业,由调用方在 EndDraw 后用 gdi_draw_labels_seeded
/// 烘焙 GDI ClearType:有快照用真实种子(精确模式,逐位同原生),无快照
/// 黑种子兜底(透明模式降级态,引擎/几何仍同原生)。
#[allow(clippy::too_many_arguments)]
pub fn draw_fence(
    rt: &ID2D1DCRenderTarget,
    r: &Renderer,
    fence: &Fence,
    metrics: &model::DpiMetrics,
    layout: &Layout,
    items: &[FileItem],
    icon_cache: &mut HashMap<String, IconBuffer>,
    hover_idx: Option<usize>,
    selected_paths: &std::collections::HashSet<String>,
    focused_path: Option<&str>,
    fence_hovered: bool,
    active: bool,
    marquee: Option<(f32, f32, f32, f32)>,
    // 正在就地重命名的文件路径:该成员的图标名标签由编辑框替代,不绘制
    // (与原生一致,避免标签从编辑框底下透出)
    hide_label: Option<&str>,
    // 正在入场动画中的文件路径:不绘制其墨水(图标+标签),但布局槽位保留——
    // 新文件"先落在桌面、再飞入栅栏"期间栅栏里不能提前露脸,落地(动画结束
    // 后 refresh_fence)才显形
    hide_arrivals: &[String],
) -> Vec<GdiLabelJob> {
    let mut jobs: Vec<GdiLabelJob> = Vec::new();
    let w = fence.rect.w;
    let h = fence.rect.h;
    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));

        // 原生桌面外观:默认(未悬停/未激活)完全干净,只显示图标与名字本身,
        // 边框/标题/手柄全部悬停或拖拽时才浮现(见下方 show_chrome)。
        // 关键:分层窗口按逐像素 alpha 做命中测试,alpha=0 的区域会点击穿透,
        // 因此整个栅栏矩形必须铺一层 alpha=1/255 的"不可见底"(视觉无感知,但鼠标可命中)。
        if let Some(bg) = brush(
            rt,
            &if r.light {
                color(1.0, 1.0, 1.0, 1.0 / 255.0)
            } else {
                color(0.0, 0.0, 0.0, 1.0 / 255.0)
            },
        ) {
            let rr = rounded(rect(0.0, 0.0, w, h), 7.0);
            rt.FillRoundedRectangle(&rr, as_brush(&bg));
        }
        let show_chrome = fence_hovered || active;
        // 无边框常显(2026-08-26):默认完全隐身,悬停/拖拽时才浮现边框+四角
        // 手柄+标题。命中与缩放热区均为位置判定,不受边框可见性影响;悬停
        // 信号复用 fence_hover(400ms 延迟提交,与"关菜单闪屏"修复同源保护)。
        if show_chrome {
            let border = if r.light {
                color(0.08, 0.09, 0.12, 0.30)
            } else {
                color(1.0, 1.0, 1.0, 0.34)
            };
            if let Some(b) = brush(rt, &border) {
                let rr = rounded(rect(0.5, 0.5, w - 0.5, h - 0.5), 7.0);
                rt.DrawRoundedRectangle(&rr, as_brush(&b), 1.0, None);
            }
            // 四角 L 形手柄:提示角部可拖拽缩放(分栏控件惯例),物理像素对齐
            let handle = if r.light {
                color(0.08, 0.09, 0.12, 0.55)
            } else {
                color(1.0, 1.0, 1.0, 0.62)
            };
            if let Some(hb) = brush(rt, &handle) {
                let len = 8.0 * metrics.scale;
                let t = metrics.scale.max(1.0);
                // 左上
                rt.FillRectangle(&rect(0.5, 0.5, len, 0.5 + t), as_brush(&hb));
                rt.FillRectangle(&rect(0.5, 0.5, 0.5 + t, len), as_brush(&hb));
                // 右上
                rt.FillRectangle(&rect(w - len, 0.5, w - 0.5, 0.5 + t), as_brush(&hb));
                rt.FillRectangle(&rect(w - 0.5 - t, 0.5, w - 0.5, len), as_brush(&hb));
                // 左下
                rt.FillRectangle(&rect(0.5, h - 0.5 - t, len, h - 0.5), as_brush(&hb));
                rt.FillRectangle(&rect(0.5, h - len, 0.5 + t, h - 0.5), as_brush(&hb));
                // 右下
                rt.FillRectangle(&rect(w - len, h - 0.5 - t, w - 0.5, h - 0.5), as_brush(&hb));
                rt.FillRectangle(&rect(w - 0.5 - t, h - len, w - 0.5, h - 0.5), as_brush(&hb));
            }
            let fill = if r.light {
                color(1.0, 1.0, 1.0, 0.10)
            } else {
                color(0.06, 0.07, 0.09, 0.12)
            };
            if let Some(bg) = brush(rt, &fill) {
                let rr = rounded(rect(0.0, 0.0, w, h), 7.0);
                rt.FillRoundedRectangle(&rr, as_brush(&bg));
            }
            draw_title(rt, r, fence, w, h, true);
        }

        if !fence.collapsed {
            let accent = fence.color();
            for i in 0..layout.visible {
                let real = layout.first_index + i;
                let Some(item) = items.get(real) else { break };
                if hide_arrivals.iter().any(|p| p == &item.path) {
                    // 入场动画中:槽位保留(几何稳定),墨水不画
                    continue;
                }
                let (ix, iy) = model::cell_pos_with_metrics(layout, real, metrics);
                let hovered = hover_idx == Some(real);
                let selected = selected_paths.contains(&item.path);
                let focused = focused_path == Some(item.path.as_str());
                draw_item(
                    rt, r, item, ix, iy, metrics, icon_cache, hovered, selected, focused, accent,
                    &mut jobs,
                    hide_label == Some(item.path.as_str()),
                );
            }
            if layout.total_rows > layout.rows && show_chrome {
                draw_scrollbar(rt, r, fence, layout, w, h);
            }
            if let Some((x0, y0, x1, y1)) = marquee {
                let l = x0.min(x1);
                let t = y0.min(y1);
                let rr = x0.max(x1);
                let bb = y0.max(y1);
                if let Some(fill) = brush(rt, &color(r.accent[0], r.accent[1], r.accent[2], 0.14)) {
                    rt.FillRectangle(&rect(l, t, rr, bb), as_brush(&fill));
                }
                if let Some(edge) = brush(rt, &color(r.accent[0], r.accent[1], r.accent[2], 0.9)) {
                    rt.DrawRectangle(
                        &rect(l + 0.5, t + 0.5, rr - 0.5, bb - 0.5),
                        as_brush(&edge),
                        1.0,
                        None,
                    );
                }
            }
        }
        let _ = rt.EndDraw(None, None);
    }
    jobs
}

/// 带描边的文字:先在上下左右四个方向用描边色画一遍(形成轮廓),
/// 再画主色。保证任意壁纸(浅色/深色/花哨)上文字都可读。
fn draw_text_shadow(
    rt: &ID2D1DCRenderTarget,
    txt: &str,
    format: &IDWriteTextFormat,
    b: &ID2D1Brush,
    shadow: &ID2D1Brush,
    r: D2D_RECT_F,
) {
    let w = shell::wide(txt);
    if w.len() > 1 {
        unsafe {
            let sr = rect(r.left + 1.0, r.top + 1.0, r.right + 1.0, r.bottom + 1.0);
            rt.DrawText(
                &w[..w.len() - 1],
                format,
                &sr,
                shadow,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_GDI_CLASSIC,
            );
            rt.DrawText(
                &w[..w.len() - 1],
                format,
                &r,
                b,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_GDI_CLASSIC,
            );
        }
    }
}

/// 标签截断缓存:名字+宽度+盒高+行数上限 -> 实际显示文本(含省略号)。
/// 盒高随 DPI 缩放,键里带上它可避免改缩放后残留旧截断。
static LABEL_TRIM_CACHE: OnceLock<Mutex<HashMap<(String, u32, u32, u32), String>>> =
    OnceLock::new();
fn label_cache() -> &'static Mutex<HashMap<(String, u32, u32, u32), String>> {
    LABEL_TRIM_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 行数超限时逐步截断并补省略号,直到恰好 max_lines 行。
pub fn trim_to_lines(
    dw: &IDWriteFactory,
    txt: &str,
    fmt: &IDWriteTextFormat,
    width: f32,
    box_h: f32,
    max_lines: u32,
) -> String {
    let key = (txt.to_string(), width as u32, box_h as u32, max_lines);
    if let Some(cached) = label_cache().lock().unwrap().get(&key) {
        return cached.clone();
    }
    let mut cur = txt.to_string();
    let fits = |text: &str| -> bool {
        let w16 = shell::wide(text);
        if w16.len() <= 1 {
            return true;
        }
        // 与绘制同源:GDI 经典度量,保证截断判定与实际渲染一致
        unsafe {
            dw.CreateGdiCompatibleTextLayout(
                &w16[..w16.len() - 1],
                fmt,
                width,
                4096.0,
                1.0,
                None,
                BOOL(0),
            )
        }
        .map(|l| {
            let mut m = Default::default();
            unsafe {
                let _ = l.GetMetrics(&mut m);
            }
            m.lineCount <= max_lines
        })
        .unwrap_or(true)
    };
    if fits(&cur) {
        label_cache()
            .lock()
            .unwrap()
            .insert(key.clone(), cur.clone());
        return cur;
    }
    let ell = "…";
    loop {
        let chars: Vec<char> = cur.chars().collect();
        if chars.len() <= 2 {
            break;
        }
        let cut = (chars.len() - 2).max(1);
        let mut next: String = chars[..cut].iter().collect();
        next.push_str(ell);
        if fits(&next) {
            cur = next;
            break;
        }
        cur = next;
    }
    label_cache().lock().unwrap().insert(key, cur.clone());
    cur
}

/// 绘制对齐参考线到全屏透明 overlay：透明背景 + 竖线(gx)/横线(gy)。
/// ghost: 内部图标拖拽残影(半透明图标+名字跟随鼠标,原生桌面拖图标即此效果)，
/// 坐标为 overlay 本地坐标。
pub struct ArrivalFrame {
    pub icon: Vec<u8>,
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub target_x: f32,
    pub target_y: f32,
    pub trail: Vec<(f32, f32, f32)>,
}

pub fn draw_guides(
    rt: &ID2D1DCRenderTarget,
    w: f32,
    h: f32,
    icon_px: f32,
    scale: f32,
    label_w: f32,
    gx: Option<f32>,
    gy: Option<f32>,
    insert_line: Option<(f32, f32, f32, f32)>,
    ghost: Option<(&[u8], &str, f32, f32)>,
    arrivals: &[ArrivalFrame],
) -> Vec<GdiLabelJob> {
    let mut jobs: Vec<GdiLabelJob> = Vec::new();
    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
        if let Some(x) = gx {
            if let Some(b) = brush(rt, &color(0.30, 0.62, 1.0, 0.95)) {
                rt.FillRectangle(&rect(x, 0.0, x + 1.0, h), as_brush(&b));
            }
        }
        if let Some(y) = gy {
            if let Some(b) = brush(rt, &color(0.30, 0.62, 1.0, 0.95)) {
                rt.FillRectangle(&rect(0.0, y, w, y + 1.0), as_brush(&b));
            }
        }
        // 插入指示线:线体垂直于排列方向(竖线=水平邻居之间,横线=上下邻居之间),
        // 两端各一段垂直小端点,宽度 2.5px
        if let Some((lx, ly, lw, lh)) = insert_line {
            if let Some(b) = brush(rt, &color(0.30, 0.62, 1.0, 0.95)) {
                rt.FillRectangle(&rect(lx, ly, lx + lw, ly + lh), as_brush(&b));
                let cap = 6.0;
                if lw <= lh {
                    // 竖线:上下端点向左右延伸
                    rt.FillRectangle(&rect(lx - cap, ly, lx + lw + cap, ly + lw), as_brush(&b));
                    let yb = ly + lh - lw;
                    rt.FillRectangle(&rect(lx - cap, yb, lx + lw + cap, yb + lw), as_brush(&b));
                } else {
                    // 横线:左右端点向上下延伸
                    rt.FillRectangle(&rect(lx, ly - cap, lx + lh, ly + lh + cap), as_brush(&b));
                    let xb = lx + lw - lh;
                    rt.FillRectangle(&rect(xb, ly - cap, xb + lh, ly + lh + cap), as_brush(&b));
                }
            }
        }
        for arrival in arrivals {
            let cs = icon_px;
            if let Some(target) = brush(rt, &color(0.30, 0.62, 1.0, 0.78)) {
                rt.DrawRectangle(
                    &rect(
                        arrival.target_x - 4.5,
                        arrival.target_y - 4.5,
                        arrival.target_x + cs + 4.5,
                        arrival.target_y + cs + 4.5,
                    ),
                    as_brush(&target),
                    2.0,
                    None,
                );
            }
            for &(tx, ty, alpha) in &arrival.trail {
                if let Some(trail) = brush(rt, &color(0.30, 0.62, 1.0, alpha)) {
                    let rr = rounded(rect(tx, ty, tx + cs * 0.24, ty + cs * 0.24), cs * 0.12);
                    rt.FillRoundedRectangle(&rr, as_brush(&trail));
                }
            }
            if !arrival.icon.is_empty() {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let px = cs as u32;
                let size = D2D_SIZE_U {
                    width: px,
                    height: px,
                };
                if let Ok(bmp) = rt.CreateBitmap(
                    size,
                    Some(arrival.icon.as_ptr() as *const _),
                    px * 4,
                    &props,
                ) {
                    let dr = rect(arrival.x, arrival.y, arrival.x + cs, arrival.y + cs);
                    rt.DrawBitmap(
                        &bmp,
                        Some(&dr),
                        0.92,
                        D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                    );
                }
            }
            if !arrival.name.is_empty() {
                // 入场动画图标名:与静态/原生同几何(标签顶=图标底+2逻辑px)、同 GDI
                // ClearType。overlay 是逐像素透明表面,作为作业返回给调用方用 GDI
                // 绘制并做局部 alpha 修复(见 gdi_draw_labels_transparent)。
                let cx = arrival.x + cs * 0.5;
                jobs.push(GdiLabelJob {
                    text: arrival.name.clone(),
                    x: cx - label_w * 0.5,
                    y: arrival.y + cs + 2.0 * scale,
                    w: label_w,
                    h: 54.0 * scale,
                });
            }
        }
        if let Some((icon, name, x, y)) = ghost {
            let cs = icon_px;
            if !icon.is_empty() {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let px = cs as u32;
                let size = D2D_SIZE_U {
                    width: px,
                    height: px,
                };
                if let Ok(bmp) =
                    rt.CreateBitmap(size, Some(icon.as_ptr() as *const _), px * 4, &props)
                {
                    // 半透明残影:与原生一致 ~65% 不透明度,锚点在鼠标左上
                    let dr = rect(x, y, x + cs, y + cs);
                    rt.DrawBitmap(
                        &bmp,
                        Some(&dr),
                        0.65,
                        D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                    );
                }
            }
            if !name.is_empty() {
                // 拖拽残影图标名:与静态/原生同几何(标签顶=图标底+2逻辑px)、同
                // GDI ClearType 绘制——拖到原生桌面图标上可与原生完全重合。
                let cx = x + cs * 0.5;
                jobs.push(GdiLabelJob {
                    text: name.to_string(),
                    x: cx - label_w * 0.5,
                    y: y + cs + 2.0 * scale,
                    w: label_w,
                    h: 54.0 * scale,
                });
            }
        }
        let _ = rt.EndDraw(None, None);
    }
    jobs
}

/// overlay(逐像素透明表面)上的 GDI 图标名绘制:与 gdi_draw_labels 同一路
/// DrawShadowText/ClearType,但 alpha 只在"有墨水"的像素置 255(GDI 不写 alpha,
/// 全置 255 会破坏表面的透明背景与 D2D 已画内容的预乘 alpha)。
pub fn gdi_draw_labels_transparent(s: &Surface, jobs: &[GdiLabelJob]) {
    if jobs.is_empty() {
        return;
    }
    let Some(lf) = shell::icon_title_logfont() else {
        return;
    };
    unsafe {
        let hf = CreateFontIndirectW(&lf as *const _);
        if hf.is_invalid() {
            return;
        }
        let old = SelectObject(s.dc, HGDIOBJ(hf.0));
        let old_bk: i32 = SetBkMode(s.dc, TRANSPARENT);
        let fmt = DT_CENTER | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS | DT_NOPREFIX;
        let flags = fmt.0;
        for job in jobs {
            let w16 = shell::wide(&job.text);
            if w16.len() <= 1 {
                continue;
            }
            let rc = RECT {
                left: job.x.round() as i32,
                top: job.y.round() as i32,
                right: (job.x + job.w).round() as i32,
                bottom: (job.y + job.h).round() as i32,
            };
            if rc.right <= rc.left || rc.bottom <= rc.top {
                continue;
            }
            let text = &w16[..w16.len() - 1];
            if let Some(draw) = draw_shadow_text_proc() {
                let _ = draw(
                    s.dc,
                    text.as_ptr(),
                    text.len() as i32,
                    &rc,
                    flags,
                    COLORREF(0x00FF_FFFF),
                    COLORREF(0x8C00_0000),
                    1,
                    1,
                );
            } else {
                let mut shifted = RECT {
                    left: rc.left + 1,
                    top: rc.top + 1,
                    right: rc.right + 1,
                    bottom: rc.bottom + 1,
                };
                let mut main_rc = rc;
                let mut buf = text.to_vec();
                SetTextColor(s.dc, COLORREF(0x0000_0000));
                let _ = DrawTextW(s.dc, &mut buf, &mut shifted, fmt);
                let mut buf2 = text.to_vec();
                SetTextColor(s.dc, COLORREF(0x00FF_FFFF));
                let _ = DrawTextW(s.dc, &mut buf2, &mut main_rc, fmt);
            }
            // 局部 alpha 修复:仅本作业矩形内,RGB 非零而 alpha 为 0 的像素
            // (即 GDI 刚画的文字/阴影)置为不透明
            let x0 = rc.left.max(0) as usize;
            let y0 = rc.top.max(0) as usize;
            let x1 = (rc.right as usize).min(s.w as usize);
            let y1 = (rc.bottom as usize).min(s.h as usize);
            if s.bits.is_null() || x1 <= x0 || y1 <= y0 {
                continue;
            }
            let bits = std::slice::from_raw_parts_mut(
                s.bits as *mut u8,
                (s.w as usize) * (s.h as usize) * 4,
            );
            for yy in y0..y1 {
                let row = yy * s.w as usize;
                for xx in x0..x1 {
                    let o = (row + xx) * 4;
                    if bits[o + 3] == 0 && (bits[o] | bits[o + 1] | bits[o + 2]) != 0 {
                        bits[o + 3] = 255;
                    }
                }
            }
        }
        SetBkMode(s.dc, BACKGROUND_MODE(old_bk as u32));
        SelectObject(s.dc, old);
        let _ = DeleteObject(HGDIOBJ(hf.0));
    }
}

fn draw_title(
    rt: &ID2D1DCRenderTarget,
    r: &Renderer,
    fence: &Fence,
    w: f32,
    _h: f32,
    show_chrome: bool,
) {
    unsafe {
        let accent = fence.color();
        // A thin category rail is easier to scan than a large colored card.
        if let Some(a) = brush(
            rt,
            &color(
                accent[0],
                accent[1],
                accent[2],
                if show_chrome { 0.95 } else { 0.72 },
            ),
        ) {
            let rr = rounded(rect(7.0, 7.0, 10.0, model::TITLE_H - 7.0), 1.5);
            rt.FillRoundedRectangle(&rr, as_brush(&a));
        }
        let main_c = if r.light {
            color(0.10, 0.11, 0.14, if show_chrome { 0.96 } else { 0.78 })
        } else {
            color(0.95, 0.96, 0.98, if show_chrome { 0.97 } else { 0.82 })
        };
        let shadow_c = if r.light {
            color(1.0, 1.0, 1.0, 0.72)
        } else {
            color(0.0, 0.0, 0.0, 0.58)
        };
        if let (Some(t), Some(ts)) = (brush(rt, &main_c), brush(rt, &shadow_c)) {
            draw_text_shadow(
                rt,
                &fence.title,
                &r.title_fmt,
                as_brush(&t),
                as_brush(&ts),
                rect(18.0, 4.0, w - model::COLLAPSE_W - 6.0, model::TITLE_H - 2.0),
            );
            {
                // 箭头始终可见(未悬停时其余 chrome 仍隐藏)。
                // 可点击提示:半透明深色圆角小条(与壁纸底都搭),
                // 内画宽扁三角(宽:高=2:1),垂直中心与左侧栅栏名同水平线。
                // 展开态三角向下,折叠态向右。
                // 视觉小块比点击热区(COLLAPSE_W)小,好点的同时不显笨重
                let cy = (4.0 + model::TITLE_H - 2.0) * 0.5;
                let zone = rect(w - 3.0 - 26.0, cy - 8.0, w - 3.0, cy + 8.0);
                let zone_rr = rounded(zone, 3.0);
                // 底色浅灰:肉眼可见即可(不抢图标名的视觉)
                if let Some(bb) = brush(
                    rt,
                    &color(0.05, 0.06, 0.09, if show_chrome { 0.26 } else { 0.16 }),
                ) {
                    rt.FillRoundedRectangle(&zone_rr, as_brush(&bb));
                }
                if let Some(sb) = brush(
                    rt,
                    &color(1.0, 1.0, 1.0, if show_chrome { 0.42 } else { 0.30 }),
                ) {
                    rt.DrawRoundedRectangle(&zone_rr, as_brush(&sb), 1.0, None);
                }
                // 宽扁三角:宽 16,高 8(2:1),与左侧栅栏名同水平线
                let cx = (zone.left + zone.right) * 0.5;
                let tw = 16.0;
                let th = 8.0;
                let tri = if fence.collapsed {
                    // 向右:左中、右上、右下
                    vec![
                        (cx - tw * 0.5, cy),
                        (cx + tw * 0.5, cy - th * 0.5),
                        (cx + tw * 0.5, cy + th * 0.5),
                    ]
                } else {
                    // 向下:顶左、顶右、底中
                    vec![
                        (cx - tw * 0.5, cy - th * 0.5),
                        (cx + tw * 0.5, cy - th * 0.5),
                        (cx, cy + th * 0.5),
                    ]
                };
                if tri.len() == 3 {
                    if let Ok(geo) = r.factory.CreatePathGeometry() {
                        if let Ok(sink) = geo.Open() {
                            let _ = sink.BeginFigure(
                                D2D_POINT_2F { x: tri[0].0, y: tri[0].1 },
                                windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED,
                            );
                            let _ = sink.AddLine(D2D_POINT_2F {
                                x: tri[1].0,
                                y: tri[1].1,
                            });
                            let _ = sink.AddLine(D2D_POINT_2F {
                                x: tri[2].0,
                                y: tri[2].1,
                            });
                            let _ = sink.EndFigure(
                                windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED,
                            );
                            let _ = sink.Close();
                            if let Some(ab) = brush(rt, &color(0.95, 0.96, 0.98, 0.95)) {
                                let _ = rt.FillGeometry(&geo, as_brush(&ab), None);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_item(
    rt: &ID2D1DCRenderTarget,
    r: &Renderer,
    item: &FileItem,
    x: f32,
    y: f32,
    metrics: &model::DpiMetrics,
    icon_cache: &mut HashMap<String, IconBuffer>,
    hovered: bool,
    selected: bool,
    focused: bool,
    accent: [f32; 3],
    jobs: &mut Vec<GdiLabelJob>,
    hide_label: bool,
) {
    unsafe {
        let cs = metrics.icon_px;
        if hovered || selected {
            // Explorer 风格状态层：hover 很淡，选择态使用系统强调色。
            let hc = if selected {
                color(
                    r.accent[0],
                    r.accent[1],
                    r.accent[2],
                    if r.light { 0.24 } else { 0.34 },
                )
            } else if r.light {
                color(0.0, 0.0, 0.0, 0.07)
            } else {
                color(1.0, 1.0, 1.0, 0.13)
            };
            if let Some(h) = brush(rt, &hc) {
                // 原生桌面选中/悬停为直角矩形，不使用圆角
                rt.FillRectangle(
                    &rect(
                        x + 2.0,
                        y + 2.0,
                        x + metrics.cell_w - 2.0 * metrics.scale,
                        y + metrics.cell_h - 2.0 * metrics.scale,
                    ),
                    as_brush(&h),
                );
            }
            if focused {
                if let Some(b) = brush(rt, &color(r.accent[0], r.accent[1], r.accent[2], 0.95)) {
                    rt.DrawRectangle(
                        &rect(
                            x + 2.5,
                            y + 2.5,
                            x + metrics.cell_w - 2.5 * metrics.scale,
                            y + metrics.cell_h - 2.5 * metrics.scale,
                        ),
                        as_brush(&b),
                        1.0,
                        None,
                    );
                }
            }
        }
        let cached = get_icon_buffer(icon_cache, &item.path, metrics.icon_px);
        if !cached.is_empty() {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let px = cs as u32;
            let size = D2D_SIZE_U {
                width: px,
                height: px,
            };
            let bmp = rt.CreateBitmap(size, Some(cached.as_ptr() as *const _), px * 4, &props);
            if let Ok(bmp) = bmp {
                let ic = cs;
                let offx = (metrics.cell_w - ic) / 2.0;
                let icon_top = 6.5 * metrics.scale;
                let dr = rect(x + offx, y + icon_top, x + offx + ic, y + icon_top + ic);
                // Buffer and destination are both cs x cs. Avoid a second linear sampling
                // pass, which softened high-contrast edges in native Shell icons.
                rt.DrawBitmap(
                    &bmp,
                    Some(&dr),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );
            }
        } else {
            if let Some(p) = brush(rt, &color(accent[0], accent[1], accent[2], 0.5)) {
                let rr = rounded(rect(x + 8.0, y + 8.0, x + cs + 8.0, y + 8.0 + cs), 4.0);
                rt.FillRoundedRectangle(&rr, as_brush(&p));
            }
        }
        // 与原生一致:平时 2 行+省略号,选中展开 4 行;位置=图标底起
        let max_lines = if selected { 4u32 } else { 2u32 };
        // 行高随 DPI 缩放,否则 150%+ 下第二/四行会被裁掉
        let label_h = (max_lines as f32 * 24.0 + 6.0) * metrics.scale;
        // Explorer 桌面图标名矩形顶 = 图标底 + 2 逻辑px(实测 150% 下 3 物理px)。
        // 图标本体从 y + 6.5*scale 开始,故此处显式从图标底再偏 2*scale,
        // 保证任意 DPI 下文字首行与原生完全重合。
        let label_top = y + 6.5 * metrics.scale + cs + 2.0 * metrics.scale;
        let lr = rect(
            x + metrics.scale,
            label_top,
            x + metrics.cell_w - metrics.scale,
            label_top + label_h,
        );
        let txt = display_name(&item.name);
        // 文字统一交给 GDI ClearType(DrawShadowText,截断按 GDI 经典度量):
        // 有快照时用真实壁纸种子(逐位同原生),无快照时黑种子兜底(引擎/几何
        // 仍与原生一致,仅 ClearType 边缘色近似)。
        if hide_label {
            // 就地重命名中:标签由编辑框替代(与原生一致),只画图标
            return;
        }
        let trimmed = trim_to_lines(
            &r.dw,
            &txt,
            &r.name_fmt,
            lr.right - lr.left,
            lr.bottom - lr.top,
            max_lines,
        );
        jobs.push(GdiLabelJob {
            text: trimmed,
            x: lr.left,
            y: lr.top,
            w: lr.right - lr.left,
            h: lr.bottom - lr.top,
        });
    }
}

fn draw_scrollbar(
    rt: &ID2D1DCRenderTarget,
    r: &Renderer,
    fence: &Fence,
    layout: &Layout,
    w: f32,
    h: f32,
) {
    unsafe {
        let content_top = model::TITLE_H + model::PAD;
        let content_bot = h - model::PAD;
        let track_h = content_bot - content_top;
        let thumb_h =
            (track_h * layout.rows as f32 / layout.total_rows as f32).clamp(12.0, track_h);
        let max = (layout.total_rows - layout.rows) as f32;
        let pos = if max > 0.0 {
            (fence.scroll_rows as f32 / max).min(1.0)
        } else {
            0.0
        };
        let thumb_top = content_top + pos * (track_h - thumb_h);
        let sbx = w - model::SCROLLBAR_W - 3.0;
        let track_c = if r.light {
            color(0.0, 0.0, 0.0, 0.08)
        } else {
            color(1.0, 1.0, 1.0, 0.10)
        };
        if let Some(t) = brush(rt, &track_c) {
            let rr = rounded(rect(sbx, content_top, w - 3.0, content_bot), 2.5);
            rt.FillRoundedRectangle(&rr, as_brush(&t));
        }
        let ac = r.accent;
        if let Some(s) = brush(rt, &color(ac[0], ac[1], ac[2], 0.85)) {
            let rr = rounded(rect(sbx, thumb_top, w - 3.0, thumb_top + thumb_h), 2.5);
            rt.FillRoundedRectangle(&rr, as_brush(&s));
        }
    }
}

pub fn display_name(name: &str) -> String {
    // 去掉 .lnk 后缀;省略号由 DWrite 原生裁剪处理
    name.strip_suffix(".lnk").unwrap_or(name).to_string()
}

/// 将表面推送到分层窗口（栅栏是顶层 WS_EX_LAYERED 窗口，必须用 UpdateLayeredWindow）
pub fn present_surface(surface: &Surface, hwnd: HWND, x: i32, y: i32) -> bool {
    unsafe {
        let size = SIZE {
            cx: surface.w as i32,
            cy: surface.h as i32,
        };
        let pos = POINT { x, y };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            hwnd,
            HDC::default(),
            Some(&pos),
            Some(&size),
            surface.dc,
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .is_ok()
    }
}

pub fn present_existing_surface(surface: &Surface, hwnd: HWND, x: i32, y: i32) -> bool {
    present_surface(surface, hwnd, x, y)
}

pub fn release_surface(s: Surface) {
    unsafe {
        let _ = SelectObject(s.dc, s.old);
        let _ = DeleteObject(s.dib);
        let _ = DeleteDC(s.dc);
    }
}
