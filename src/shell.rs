//! Shell 集成：桌面扫描、文件图标、打开文件、系统右键菜单

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{ComInterface, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, DIB_RGB_COLORS, LOGFONTW,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, IBindCtx, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_PublicDesktop, IContextMenu, IContextMenu2, IContextMenu3,
    IShellFolder, IShellItemImageFactory, IShellView, SHBindToParent, SHCreateItemFromParsingName,
    SHFileOperationW, SHGetDesktopFolder, SHGetFileInfoW, SHGetImageList, SHGetKnownFolderPath,
    SHParseDisplayName, ShellExecuteExW, ShellExecuteW, CMF_NORMAL, CMINVOKECOMMANDINFOEX,
    FOF_ALLOWUNDO, FOF_RENAMEONCOLLISION, FO_COPY, FO_DELETE, GCS_VERBW, KF_FLAG_DEFAULT,
    SHELLEXECUTEINFOW, SHFILEINFOW, SHFILEOPSTRUCTW, SHGFI_ADDOVERLAYS, SHGFI_DISPLAYNAME,
    SHGFI_ICON, SHGFI_LARGEICON, SHGFI_OVERLAYINDEX, SHGFI_SYSICONINDEX, SIIGBF_ICONONLY,
    SIIGBF_SCALEUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, EnumWindows, FindWindowExW, FindWindowW,
    GetForegroundWindow, GetMenuItemCount, GetMenuItemID, GetSystemMetrics, GetWindowRect,
    GetWindowThreadProcessId, InsertMenuW, MF_BYPOSITION, PostMessageW, SendMessageTimeoutW,
    SetForegroundWindow, SystemParametersInfoW, TrackPopupMenu, HICON, HMENU, MENU_ITEM_FLAGS,
    MF_CHECKED, MF_POPUP, MF_SEPARATOR, SMTO_ABORTIFHUNG, SM_CXICON,
    SPI_GETICONTITLELOGFONT, SW_SHOWNORMAL,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_RETURNCMD, WM_NULL,
};

use crate::model::FileItem;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// shell 模块日志(转 ui::log 落盘)
fn log(line: &str) {
    crate::ui::log(line);
}

/// 读取系统桌面原生图标尺寸（像素）：HKCU\...\Bags\1\Desktop\IconSize，失败回退 32
pub fn desktop_icon_size() -> f32 {
    let mut v: u32 = 0;
    let mut sz = std::mem::size_of::<u32>() as u32;
    let key = wide(r"Software\Microsoft\Windows\Shell\Bags\1\Desktop");
    let val = wide("IconSize");
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut ty),
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_ok()
    };
    if ok && (8..=256).contains(&v) {
        v as f32
    } else {
        32.0
    }
}

/// 读取 WindowMetrics\IconSpacing / IconVerticalSpacing（REG_SZ，如 "-1130"）。
/// 两个值都转换为相对于 32px 图标的逻辑留白；缺失或无效时使用
/// 紧凑的保守回退，避免把失效的系统值放大成栅栏内的大块空白。
pub fn desktop_cell_pads() -> (f32, f32) {
    let read = |name: &str| -> Option<f32> {
        let key = wide(r"Control Panel\Desktop\WindowMetrics");
        let val = wide(name);
        let mut buf = [0u16; 32];
        let mut sz = (buf.len() * 2) as u32;
        let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(key.as_ptr()),
                PCWSTR::from_raw(val.as_ptr()),
                RRF_RT_REG_SZ,
                Some(&mut ty),
                Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                Some(&mut sz),
            )
            .is_ok()
        };
        if !ok {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        let v = s.trim().parse::<i32>().ok()?;
        let px = (v.unsigned_abs() as f32) / 15.0; // twips → 像素
        let pad = px - 32.0; // 扣除 32px 基准图标，得到留白
        Some(pad.clamp(16.0, 96.0))
    };
    (
        read("IconSpacing").unwrap_or(32.0),
        read("IconVerticalSpacing").unwrap_or(40.0),
    )
}

/// Explorer 高级设置里的 DWORD 值
fn explorer_dword(sub: &str, name: &str) -> Option<u32> {
    let key = wide(sub);
    let val = wide(name);
    let mut v: u32 = 0;
    let mut sz = std::mem::size_of::<u32>() as u32;
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut ty),
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_ok()
    };
    ok.then_some(v)
}

/// 与 Explorer 一致的"显示隐藏文件"设置（Hidden == 1）
pub fn show_hidden_files() -> bool {
    explorer_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        "Hidden",
    ) == Some(1)
}

/// 与 Explorer 一致的"显示受保护的操作系统文件"设置（ShowSuperHidden == 1，默认隐藏）
pub fn show_super_hidden() -> bool {
    explorer_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        "ShowSuperHidden",
    ) == Some(1)
}
/// Read Explorer's configured desktop icon caption font.
pub fn desktop_icon_font() -> (String, f32, i32) {
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETICONTITLELOGFONT,
            std::mem::size_of::<LOGFONTW>() as u32,
            Some(&mut lf as *mut LOGFONTW as *mut std::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
    };
    if !ok {
        return ("Segoe UI".into(), 12.0, 400);
    }
    let end = lf
        .lfFaceName
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(lf.lfFaceName.len());
    let family = String::from_utf16_lossy(&lf.lfFaceName[..end]);
    // LOGFONT.lfHeight 负值本身即为像素字符高度（已随 DPI 缩放，如 96DPI 下 -12≈9pt），
    // 不能再按磅值换算乘 DPI，否则字号被双重放大
    let px = if lf.lfHeight < 0 {
        -(lf.lfHeight as f32)
    } else if lf.lfHeight > 0 {
        (lf.lfHeight as f32).abs()
    } else {
        12.0
    };
    (
        if family.is_empty() {
            "Segoe UI".into()
        } else {
            family
        },
        px.clamp(8.0, 48.0),
        lf.lfWeight,
    )
}

/// 桌面目录（已知文件夹优先，回退 USERPROFILE\Desktop）
pub fn desktop_dir() -> Option<std::path::PathBuf> {
    let p: windows::core::PWSTR = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) {
            Ok(pw) if !pw.is_null() => pw,
            _ => {
                return std::env::var("USERPROFILE")
                    .ok()
                    .map(|u| std::path::PathBuf::from(u).join("Desktop"));
            }
        }
    };
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe {
        CoTaskMemFree(Some(p.as_ptr() as *const _));
    }
    if s.is_empty() {
        std::env::var("USERPROFILE")
            .ok()
            .map(|u| std::path::PathBuf::from(u).join("Desktop"))
    } else {
        Some(std::path::PathBuf::from(s))
    }
}

fn public_desktop_dir() -> Option<std::path::PathBuf> {
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_PublicDesktop, KF_FLAG_DEFAULT, None).ok()? };
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe {
        CoTaskMemFree(Some(p.as_ptr() as *const _));
    }
    if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    }
}

/// Ask Shell for the same user-facing name Explorer uses (for example, hide .lnk).
fn shell_display_name(path: &std::path::Path, fallback: &str) -> String {
    let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let w = wide(&path.to_string_lossy());
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            size_of_val(&sfi) as u32,
            SHGFI_DISPLAYNAME,
        ) != 0
    };
    if !ok {
        return fallback.to_string();
    }
    let end = sfi
        .szDisplayName
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(sfi.szDisplayName.len());
    let name = String::from_utf16_lossy(&sfi.szDisplayName[..end]);
    if name.is_empty() {
        fallback.to_string()
    } else {
        name
    }
}

/// 扫描桌面目录下的文件。过滤规则与 Explorer 原生桌面完全一致：
/// 1) 永远排除 desktop.ini 与 Office 锁文件(~$ 开头,仅在文档打开期间存在);
/// 2) 按用户设置排除隐藏/受保护的系统文件;
/// 3) 用户桌面与公共桌面同名冲突时只显示用户桌面的那份(Explorer 同名只显一条)。
/// 显示名解析(SHGFI_DISPLAYNAME)是逐文件 shell 调用(每个 ~15-25ms),
/// 54 个文件串行要 ~1.3s,这里按 4 线程并行缩到 ~300ms;各线程独立 STA COM。
pub fn scan_desktop() -> Vec<FileItem> {
    let mut out = scan_desktop_raw();
    finalize_scan(&mut out);
    out
}

/// 显示名解析 + 同名去重 + 排序(Explorer 桌面按显示名排序,必须在解析后)。
pub fn finalize_scan(files: &mut Vec<FileItem>) {
    finalize_scan_with(files, None);
}

/// 同上,但带预填显示名缓存(冷启动优化):缓存命中的条目直接跳过
/// SHGFI 解析。显示名缓存值必须是 resolve_display_names 的原样输出,
/// 去重/排序结果才能与全解析启动逐位一致。
pub fn finalize_scan_with(
    files: &mut Vec<FileItem>,
    prefill: Option<&std::collections::HashMap<String, String>>,
) {
    resolve_display_names(files, prefill);
    dedup_by_display_name(files);
    files.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
}

/// 只做文件系统枚举+属性过滤,不做任何 shell 调用(name=文件系统原名)。
/// 快(~20ms),可安全在启动关键路径上先行,shell 部分交给后台线程。
pub fn scan_desktop_raw() -> Vec<FileItem> {
    let mut out = Vec::new();
    if let Some(dir) = desktop_dir() {
        out.extend(scan_desktop_dir(&dir));
    }
    if let Some(dir) = public_desktop_dir() {
        if !desktop_dir()
            .map(|d| {
                d.to_string_lossy()
                    .eq_ignore_ascii_case(&dir.to_string_lossy())
            })
            .unwrap_or(false)
        {
            out.extend(scan_desktop_dir(&dir));
        }
    }
    out
}

fn scan_desktop_dir(dir: &std::path::Path) -> Vec<FileItem> {
    use std::os::windows::fs::MetadataExt;
    let mut out = Vec::new();
    let show_hidden = show_hidden_files();
    let show_super = show_super_hidden();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let raw_name = entry.file_name().to_string_lossy().to_string();
        let Ok(md) = entry.metadata() else { continue };
        let attrs = md.file_attributes();
        if raw_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        // Office 打开文档期间产生的属主锁文件,对用户无意义
        if raw_name.starts_with("~$") {
            continue;
        }
        if attrs & FILE_ATTRIBUTE_SYSTEM.0 != 0 && !show_super {
            continue;
        }
        if attrs & FILE_ATTRIBUTE_HIDDEN.0 != 0 && !show_hidden {
            continue;
        }
        let is_dir = md.is_dir();
        let ext = if is_dir {
            String::new()
        } else {
            entry
                .path()
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        let cat = crate::model::categorize(&raw_name, is_dir);
        let path = entry.path().to_string_lossy().to_string();
        out.push(FileItem {
            name: raw_name,
            path,
            is_dir,
            ext,
            category: cat,
            mtime_ms: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        });
    }
    out
}

/// 并行解析显示名(SHGFI_DISPLAYNAME,.lnk 的显示名可能与文件名不同,
/// 且受"隐藏已知扩展名"设置影响)。失败时保留原名(fallback=当前 name)。
/// prefill:path→显示名 命中表——命中的条目跳过解析并直接采用值(与上次
/// 启动的解析输出逐位一致,保证去重/排序确定性);未命中照旧走 shell。
pub fn resolve_display_names(
    files: &mut [FileItem],
    prefill: Option<&std::collections::HashMap<String, String>>,
) {
    const THREADS: usize = 4;
    let n = files.len();
    if n == 0 {
        return;
    }
    let threads = THREADS.min(n);
    let per = (n + threads - 1) / threads;
    std::thread::scope(|scope| {
        let mut rest = files;
        let mut handles = Vec::new();
        for t in 0..threads {
            let take = if t == threads - 1 {
                rest.len()
            } else {
                per.min(rest.len())
            };
            let (chunk, tail) = rest.split_at_mut(take);
            rest = tail;
            handles.push(scope.spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                for f in chunk.iter_mut() {
                    if let Some(hit) = prefill.and_then(|m| m.get(&f.path)) {
                        f.name = hit.clone();
                        continue;
                    }
                    let p = std::path::PathBuf::from(&f.path);
                    let fallback = f.name.clone();
                    f.name = shell_display_name(&p, &fallback);
                }
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
    });
}

/// 与 Explorer 一致:显示名(不区分大小写)冲突时用户桌面优先(原始列表
/// 已按用户桌面在前排序),后扫到的跳过。
fn dedup_by_display_name(files: &mut Vec<FileItem>) {
    let mut seen: HashSet<String> = HashSet::new();
    files.retain(|f| seen.insert(f.name.to_lowercase()));
}

/// 并行预热图标像素缓存:启动时把桌面全部路径按主屏图标像素先提取,
/// 首帧栅栏绘制全部命中缓存(SHGFI 对 .lnk/exe 单个可达 ~180ms,
/// 串行 22 个要 ~1.6s)。返回 map 键与 render::get_icon_buffer 一致,直接合并。
pub fn prewarm_icon_cache(
    paths: &[String],
    px: f32,
) -> std::collections::HashMap<String, Vec<u8>> {
    const THREADS: usize = 4;
    let n = paths.len();
    let mut merged = std::collections::HashMap::new();
    if n == 0 {
        return merged;
    }
    let threads = THREADS.min(n);
    let per = (n + threads - 1) / threads;
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for t in 0..threads {
            let slice = &paths[t * per..((t + 1) * per).min(n)];
            handles.push(scope.spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                let mut local: std::collections::HashMap<String, Vec<u8>> =
                    std::collections::HashMap::new();
                for p in slice {
                    crate::render::get_icon_buffer(&mut local, p, px);
                }
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
                local
            }));
        }
        for h in handles {
            if let Ok(local) = h.join() {
                merged.extend(local);
            }
        }
    });
    merged
}

/// Select the smallest image that is at least the target size. If every image is smaller,
/// select the largest available image instead.
fn select_image_size<I>(sizes: I, target_px: u32) -> Option<u32>
where
    I: IntoIterator<Item = u32>,
{
    let mut smallest_at_least = None;
    let mut largest_below = None;
    for size in sizes.into_iter().filter(|size| *size > 0) {
        if size >= target_px {
            smallest_at_least = Some(smallest_at_least.map_or(size, |best: u32| best.min(size)));
        } else {
            largest_below = Some(largest_below.map_or(size, |best: u32| best.max(size)));
        }
    }
    smallest_at_least.or(largest_below)
}

/// Get the same system image-list icon Explorer uses, including normal Shell overlays.
/// The list is chosen so its source image is never smaller than the target when possible.
pub fn get_system_icon_hicon(path: &str, target_px: u32) -> Option<HICON> {
    // 首选:SHGFI_ICON|SHGFI_ADDOVERLAYS 一步拿到"已合成快捷方式箭头"的完整图标,
    // 尺寸随系统 DPI 缩放(150% 下即 48px,与原生桌面一致)。要求与目标尺寸吻合,
    // 否则会有二次缩放导致模糊。
    let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let w = wide(path);
    let sys_icon_px = unsafe { GetSystemMetrics(SM_CXICON) } as u32;
    if sys_icon_px == target_px {
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR::from_raw(w.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut sfi),
                size_of_val(&sfi) as u32,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_ADDOVERLAYS,
            )
        } != 0;
        if ok && sfi.hIcon.0 != 0 {
            return Some(sfi.hIcon);
        }
    }
    // 兜底:按"实测尺寸"挑最接近目标 px 的 shell 图像列表档位。
    // 不能按逻辑档位名挑(150% DPI 下 SHIL_LARGE 实际是 48px、EXTRALARGE 是 72px,
    // 按名字挑会拿 72px 图标再缩到 48 → 模糊)。
    let mut sfi2: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi2),
            size_of_val(&sfi2) as u32,
            SHGFI_SYSICONINDEX | SHGFI_ADDOVERLAYS | SHGFI_OVERLAYINDEX,
        ) != 0
    };
    if !ok || sfi2.iIcon < 0 {
        return None;
    }
    let raw = sfi2.iIcon as u32;
    let image_index = (raw & 0x00FF_FFFF) as i32;
    let overlay_index = (raw >> 24) & 0xFF;
    let draw_flags = ILD_TRANSPARENT.0 as u32 | (overlay_index << 8); // INDEXTOOVERLAYMASK
    const KINDS: [u32; 4] = [
        windows::Win32::UI::Shell::SHIL_LARGE,
        windows::Win32::UI::Shell::SHIL_EXTRALARGE,
        windows::Win32::UI::Shell::SHIL_JUMBO,
        windows::Win32::UI::Shell::SHIL_SMALL,
    ];
    let mut lists = Vec::new();
    for kind in KINDS {
        unsafe {
            let Ok(list) = SHGetImageList::<IImageList>(kind as i32) else {
                continue;
            };
            let mut cx = 0i32;
            let mut cy = 0i32;
            if list.GetIconSize(&mut cx, &mut cy).is_err() {
                continue;
            }
            let size = cx.max(cy).unsigned_abs();
            if size > 0 {
                lists.push((list, size));
            }
        }
    }
    let selected_size = select_image_size(lists.iter().map(|(_, size)| *size), target_px)?;
    let (list, _) = lists.into_iter().find(|(_, size)| *size == selected_size)?;
    unsafe { list.GetIcon(image_index, draw_flags).ok() }
}

/// 诊断工具(--icondump):把各条图标提取路径在 SM_CXICON 档位的像素导出为
/// BMP+BGRA 原始文件,用于和原生桌面截图逐像素对比(快捷方式箭头差异定位)。
pub fn icon_dump(path: &str, prefix: &str) {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    fn save_dump(stem: &str, bgra: &[u8], w: u32, h: u32) {
        use std::io::Write;
        let data = (w * h * 4) as u32;
        if let Ok(mut f) = std::fs::File::create(format!("{stem}.bmp")) {
            let _ = f.write_all(&[0x42u8, 0x4D]);
            let _ = f.write_all(&(14u32 + 40 + data).to_le_bytes());
            let _ = f.write_all(&0u32.to_le_bytes());
            let _ = f.write_all(&(14u32 + 40).to_le_bytes());
            let _ = f.write_all(&40u32.to_le_bytes());
            let _ = f.write_all(&(w as i32).to_le_bytes());
            let _ = f.write_all(&(-(h as i32)).to_le_bytes());
            let _ = f.write_all(&1u16.to_le_bytes());
            let _ = f.write_all(&32u16.to_le_bytes());
            let _ = f.write_all(&0u32.to_le_bytes());
            let _ = f.write_all(&data.to_le_bytes());
            let _ = f.write_all(&2835u32.to_le_bytes());
            let _ = f.write_all(&2835u32.to_le_bytes());
            let _ = f.write_all(&0u32.to_le_bytes());
            let _ = f.write_all(&0u32.to_le_bytes());
            let _ = f.write_all(bgra);
        }
        let _ = std::fs::write(format!("{stem}.bgra"), bgra);
    }

    fn hicon_size(h: HICON) -> (i32, i32) {
        unsafe {
            let mut ii: ICONINFO = std::mem::zeroed();
            if GetIconInfo(h, &mut ii).is_ok() {
                let mut bm: windows::Win32::Graphics::Gdi::BITMAP = std::mem::zeroed();
                let n = windows::Win32::Graphics::Gdi::GetObjectW(
                    windows::Win32::Graphics::Gdi::HGDIOBJ(ii.hbmMask.0),
                    std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAP>() as i32,
                    Some(&mut bm as *mut _ as *mut _),
                );
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                    windows::Win32::Graphics::Gdi::HGDIOBJ(ii.hbmColor.0),
                );
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                    windows::Win32::Graphics::Gdi::HGDIOBJ(ii.hbmMask.0),
                );
                if n != 0 {
                    return (bm.bmWidth, bm.bmHeight);
                }
            }
            (0, 0)
        }
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // 与 ui::init 相同的 DPI 感知,否则 SM_CXICON 被虚拟化成 96dpi 的 32
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        let px = GetSystemMetrics(SM_CXICON) as u32;
        println!("icondump: SM_CXICON={px} path={path}");
        let w = wide(path);

        // V1: 生产主路径(SHGFI 一步合成 overlay)
        let mut sfi: SHFILEINFOW = std::mem::zeroed();
        let ok = SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            size_of_val(&sfi) as u32,
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_ADDOVERLAYS,
        ) != 0;
        if ok && sfi.hIcon.0 != 0 {
            println!("v1_shgfi real_hicon_size={:?}", hicon_size(sfi.hIcon));
            if let Some(b) = crate::render::icon_pixels(sfi.hIcon, px) {
                save_dump(&format!("{prefix}_v1_shgfi"), &b, px, px);
            }
            let _ = DestroyIcon(sfi.hIcon);
        } else {
            println!("v1_shgfi FAILED");
        }

        // V2/V3: image-list 档位(Explorer 绘制机制)
        let mut sfi2: SHFILEINFOW = std::mem::zeroed();
        let ok2 = SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi2),
            size_of_val(&sfi2) as u32,
            SHGFI_SYSICONINDEX | SHGFI_ADDOVERLAYS | SHGFI_OVERLAYINDEX,
        ) != 0;
        if !ok2 || sfi2.iIcon < 0 {
            println!("sysiconindex FAILED");
            return;
        }
        let raw = sfi2.iIcon as u32;
        let image_index = (raw & 0x00FF_FFFF) as i32;
        let overlay = (raw >> 24) & 0xFF;
        println!("image_index={image_index} overlay_index={overlay}");
        const KINDS: [u32; 4] = [SHIL_LARGE, SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_SMALL];
        for kind in KINDS {
            let Ok(list) = SHGetImageList::<IImageList>(kind as i32) else {
                continue;
            };
            let mut cx = 0i32;
            let mut cy = 0i32;
            if list.GetIconSize(&mut cx, &mut cy).is_err() {
                continue;
            }
            println!("tier kind={kind} size={cx}x{cy}");
            if cx.max(cy) as u32 != px {
                continue; // 只 dump 与桌面图标同档位的结果
            }
            let flags_overlay = ILD_TRANSPARENT.0 as u32 | (overlay << 8);
            if let Ok(icon2) = list.GetIcon(image_index, flags_overlay) {
                if let Some(b) = crate::render::icon_pixels(icon2, px) {
                    save_dump(&format!("{prefix}_v2_imglist_ovl"), &b, px, px);
                }
                let _ = DestroyIcon(icon2);
            }
            if let Ok(icon3) = list.GetIcon(image_index, ILD_TRANSPARENT.0 as u32) {
                if let Some(b) = crate::render::icon_pixels(icon3, px) {
                    save_dump(&format!("{prefix}_v3_imglist_base"), &b, px, px);
                }
                let _ = DestroyIcon(icon3);
            }
        }
        println!("icondump done");
    }
}

/// 按目标物理像素请求 Windows Shell 原生图像。作为 image-list 获取失败时的 fallback。
pub fn get_icon_bitmap(
    path: &str,
    target_px: u32,
) -> Option<windows::Win32::Graphics::Gdi::HBITMAP> {
    if target_px == 0 {
        return None;
    }
    let w = wide(path);
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR::from_raw(w.as_ptr()), None).ok()?;
        factory
            .GetImage(
                SIZE {
                    cx: target_px as i32,
                    cy: target_px as i32,
                },
                SIIGBF_ICONONLY | SIIGBF_SCALEUP,
            )
            .ok()
    }
}

/// 旧版 Shell 接口降级路径。仅在按尺寸图像工厂失败时使用。
pub fn get_icon_hicon(path: &str) -> Option<HICON> {
    let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let flags = SHGFI_ICON | SHGFI_LARGEICON | SHGFI_ADDOVERLAYS;
    let w = wide(path);
    unsafe {
        SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            size_of_val(&sfi) as u32,
            flags,
        );
    }
    if sfi.hIcon.0 == 0 {
        None
    } else {
        Some(sfi.hIcon)
    }
}

/// 打开文件或目录（双击图标）
pub fn open_path(path: &str) {
    let w = wide(path);
    unsafe {
        // 与 Explorer 双击一致:不给动词、带 SEE_MASK_INVOKEIDLIST,
        // 由 shell 调用默认动词(部分条目默认动词不是 "open")
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        // SEE_MASK_INVOKEIDLIST(0x0C = DEFAULT|INVOKEIDLIST):用默认动词激活
        info.fMask = 0x000C;
        info.lpFile = PCWSTR::from_raw(w.as_ptr());
        info.nShow = SW_SHOWNORMAL.0 as i32;
        let _ = ShellExecuteExW(&mut info);
    }
}

/// 打开回收站资源管理器窗口(双击回收站图标)
pub fn open_recycle_bin() {
    let file = wide(crate::model::RECYCLE_BIN_PATH);
    let verb = wide("open");
    unsafe {
        let _ = ShellExecuteW(
            HWND(0),
            PCWSTR::from_raw(verb.as_ptr()),
            PCWSTR::from_raw(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 在资源管理器中定位文件
pub fn open_in_explorer(path: &str) {
    let w = wide("explorer.exe");
    let sel = format!("/select,\"{}\"", path);
    let params = wide(&sel);
    let op = wide("open");
    unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR::from_raw(op.as_ptr()),
            PCWSTR::from_raw(w.as_ptr()),
            PCWSTR::from_raw(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 追加一个菜单项
pub fn append_menu(menu: HMENU, id: u32, text: &str) {
    let w = wide(text);
    unsafe {
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            id as usize,
            PCWSTR::from_raw(w.as_ptr()),
        );
    }
}

/// 追加一个带勾选标记的菜单项
pub fn append_menu_checked(menu: HMENU, id: u32, text: &str) {
    let w = wide(text);
    unsafe {
        let _ = AppendMenuW(menu, MF_CHECKED, id as usize, PCWSTR::from_raw(w.as_ptr()));
    }
}

pub fn append_separator(menu: HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// 追加一个弹出式子菜单(如"排序 ▸")
pub fn append_submenu(menu: HMENU, text: &str, submenu: HMENU) {
    let w = wide(text);
    unsafe {
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            submenu.0 as usize,
            PCWSTR::from_raw(w.as_ptr()),
        );
    }
}

fn track_popup(menu: HMENU, hwnd: HWND, x: i32, y: i32) -> u32 {
    unsafe {
        // 默认左键选择 + 返回命令 id（不带 TPM_RIGHTBUTTON，避免左键点菜单项不触发）
        let r = TrackPopupMenu(menu, TPM_RETURNCMD, x, y, 0, hwnd, None);
        if r.0 != 0 {
            r.0 as u32
        } else {
            0
        }
    }
}

// ---------------- 系统右键菜单(与 Explorer 原生完全一致) ----------------

// Shell 菜单命令 id 区间:idCmdFirst 必须小于 idCmdLast(曾因 0x8000>0x7FFF 的
// 倒挂区间导致扩展与默认动词全部跳过注册,菜单只剩 8 项)。上限 0x5FFF 避开
// DeskFence 自有命令 id(0x6001+)。
const CMD_FIRST: u32 = 1;
const CMD_LAST: u32 = 0x5FFF;
/// 注入的"重命名"菜单项 id(在 shell 动词区间与 DL_CMD 之外)
const DL_ITEM_RENAME_ID: u32 = 0x6008;

/// "重命名"由 Explorer 桌面视图层(DefView)注入,纯 IContextMenu 菜单不含它;
/// 在"删除"与"属性"之间补上同款菜单项,保持与原生逐项一致。文案跟随系统
/// 安装语言(zh-CN:重命名(&M),其余:Rename)。
fn inject_rename_item(menu: HMENU, ctx: &IContextMenu) {
    unsafe {
        let count = GetMenuItemCount(menu);
        if count < 3 {
            return;
        }
        for i in 0..count {
            let id = GetMenuItemID(menu, i as i32);
            if id == DL_ITEM_RENAME_ID {
                return;
            }
            if (CMD_FIRST..=CMD_LAST).contains(&id) && verb_is_rename(ctx, id - CMD_FIRST) {
                return;
            }
        }
        let key = wide(r"SYSTEM\CurrentControlSet\Control\Nls\Language");
        let val = wide("InstallLanguage");
        let mut buf = [0u16; 32];
        let mut sz = (buf.len() * 2) as u32;
        let mut zh = false;
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_ok()
        {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let lang = String::from_utf16_lossy(&buf[..end]);
            zh = lang.starts_with("08") && lang.ends_with("04");
        }
        let text = if zh { "重命名(&M)" } else { "Rename(&M)" };
        let w = wide(text);
        let pos = (count - 2) as u32;
        let _ = InsertMenuW(
            menu,
            pos,
            MF_BYPOSITION,
            DL_ITEM_RENAME_ID as usize,
            PCWSTR::from_raw(w.as_ptr()),
        );
    }
}

thread_local! {
    static ACTIVE_CONTEXT_MENU: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

pub fn forward_menu_message(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    ACTIVE_CONTEXT_MENU.with(|slot| {
        let ctx = slot.borrow();
        let ctx = ctx.as_ref()?;
        unsafe {
            if let Ok(menu3) = ctx.cast::<IContextMenu3>() {
                let mut result = LRESULT(0);
                if menu3
                    .HandleMenuMsg2(msg, wparam, lparam, Some(&mut result))
                    .is_ok()
                {
                    return Some(result);
                }
            }
            if let Ok(menu2) = ctx.cast::<IContextMenu2>() {
                if menu2.HandleMenuMsg(msg, wparam, lparam).is_ok() {
                    return Some(LRESULT(1));
                }
            }
        }
        None
    })
}
// windows 0.52 未导出这个掩码,按 shlobj_core.h 补定义
/// Explorer 桌面图标菜单带 CMF_CANRENAME,shell 因此输出视图级"重命名"动词
const CMF_CANRENAME: u32 = 0x00100000;
const CMIC_MASK_UNICODE: u32 = 0x00004000;

/// DeskFence 子菜单命令 id(桌面背景右键菜单里注入,由 ui.rs 分派)
pub const DL_CMD_ADD_FENCE: u32 = 0x6001;
pub const DL_CMD_SHOW_ALL: u32 = 0x6002;
pub const DL_CMD_HIDE_ALL: u32 = 0x6003;
pub const DL_CMD_UNDO: u32 = 0x6004;
pub const DL_CMD_AUTO_ALIGN: u32 = 0x6005;
pub const DL_CMD_REFRESH: u32 = 0x6006;
pub const DL_CMD_QUIT: u32 = 0x6007;
/// 渲染模式切换(透明 ↔ 精确)
pub const DL_CMD_RENDER_MODE: u32 = 0x6009;
pub const DL_CMD_HELP: u32 = 0x600A;

/// 系统"图标标题"原始 LOGFONT(与 Explorer 桌面文字同源;精确模式 GDI 绘制用)
pub fn icon_title_logfont() -> Option<LOGFONTW> {
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETICONTITLELOGFONT,
            std::mem::size_of::<LOGFONTW>() as u32,
            Some(&mut lf as *mut LOGFONTW as *mut std::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
    };
    ok.then_some(lf)
}

/// PW_RENDERFULLCONTENT:PrintWindow 捕获 DWM 合成内容(Win8.1+;0x52 未导出该常量)
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(0x00000002);

/// 捕获一个窗口的合成像素(顶层 32bpp BGRA,自上而下),alpha 全部置 255。
/// 精确模式用它抓取桌面宿主(Progman/WorkerW)上实际显示的壁纸。
/// 捕获窗口像素。失败时返回原因字符串(PrintWindow 失败码/矩形异常等),
/// 供启动期诊断"快照迟迟不可用"的具体环节。
pub fn capture_window_pixels(hwnd: HWND) -> Result<(Vec<u8>, u32, u32), String> {
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return Err("GetWindowRect failed".into());
        }
        let w = (r.right - r.left).max(1) as u32;
        let h = (r.bottom - r.top).max(1) as u32;
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(format!("bad rect {w}x{h}"));
        }
        let hdc_screen = GetDC(HWND(0));
        if hdc_screen.0 == 0 {
            return Err("GetDC failed".into());
        }
        let dc = CreateCompatibleDC(hdc_screen);
        ReleaseDC(HWND(0), hdc_screen);
        if dc.0 == 0 {
            return Err("CreateCompatibleDC failed".into());
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
        let dib = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        {
            Ok(d) => d,
            Err(_) => {
                let _ = DeleteDC(dc);
                return Err("CreateDIBSection failed".into());
            }
        };
        if bits.is_null() {
            let _ = DeleteObject(dib);
            let _ = DeleteDC(dc);
            return Err("CreateDIBSection null bits".into());
        }
        let old = SelectObject(dc, dib);
        let ok = PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT).as_bool();
        let mut out = Vec::new();
        if ok {
            let src = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            out = src;
        }
        SelectObject(dc, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(dc);
        if !ok {
            let code = match GetLastError() {
                Ok(()) => 0,
                Err(e) => e.code().0 as u32,
            };
            return Err(format!("PrintWindow false err={code}"));
        }
        // GDI 捕获的 alpha 不可靠:精确模式整窗不透明,统一置 255
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok((out, w, h))
    }
}

/// 系统菜单里的"重命名"被拦截后改由栅栏内就地编辑完成(ui.rs 启动时注册)
static RENAME_REQUEST: std::sync::Mutex<Option<fn(&str)>> = std::sync::Mutex::new(None);

pub fn set_rename_request_hook(f: fn(&str)) {
    *RENAME_REQUEST.lock().unwrap() = Some(f);
}

fn request_rename(path: &str) -> bool {
    if let Some(f) = *RENAME_REQUEST.lock().unwrap() {
        f(path);
        true
    } else {
        false
    }
}

/// 完整路径 → 绝对 PIDL(SHParseDisplayName,能解析 ParseDisplayName 处理不了的完整路径)
fn pidl_from_path(path: &str) -> Option<*mut ITEMIDLIST> {
    let w = wide(path);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe {
        SHParseDisplayName(
            PCWSTR::from_raw(w.as_ptr()),
            None::<&IBindCtx>,
            &mut pidl,
            0,
            None,
        )
        .ok()?;
    }
    if pidl.is_null() {
        None
    } else {
        Some(pidl)
    }
}

/// 命令 id 对应的 verb 是否为 "rename"
fn verb_is_rename(ctx: &IContextMenu, verb_idx: u32) -> bool {
    let mut buf = [0u16; 64];
    unsafe {
        ctx.GetCommandString(
            verb_idx as usize,
            GCS_VERBW,
            None,
            PSTR(buf.as_mut_ptr() as *mut u8),
            64,
        )
        .map(|_| {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end]).eq_ignore_ascii_case("rename")
        })
        .unwrap_or(false)
    }
}

/// 用 Unicode 扩展结构执行菜单命令(兼容 Win10+ 现代 verb 与第三方扩展菜单项)。
/// 优先取字符串 verb(GCS_VERBW,如 "open"/"delete"),跨 shell 版本比数字偏移稳;
/// 取不到时回退 MAKEINTRESOURCEW(verb_idx)。HRESULT 落盘便于诊断。
fn invoke_command(hwnd: HWND, ctx: &IContextMenu, verb_idx: u32, x: i32, y: i32) {
    unsafe {
        // 取字符串 verb
        let mut wverb = [0u16; 64];
        let has_str = ctx
            .GetCommandString(
                verb_idx as usize,
                GCS_VERBW,
                None,
                PSTR(wverb.as_mut_ptr() as *mut u8),
                64,
            )
            .is_ok();
        let mut info: CMINVOKECOMMANDINFOEX = std::mem::zeroed();
        info.cbSize = size_of::<CMINVOKECOMMANDINFOEX>() as u32;
        info.fMask = CMIC_MASK_UNICODE;
        info.hwnd = hwnd;
        info.nShow = SW_SHOWNORMAL.0 as i32;
        if has_str {
            info.lpVerbW = PCWSTR::from_raw(wverb.as_ptr());
        } else {
            // MAKEINTRESOURCEW(verb_idx):数字 verb 的规范传递方式
            info.lpVerbW = PCWSTR::from_raw(verb_idx as usize as *const u16);
        }
        // 部分 shell 实现仍读 ANSI lpVerb 字段:同步填数字偏移,避免 E_INVALIDARG
        info.lpVerb = PCSTR(verb_idx as usize as *const u8);
        info.ptInvoke = POINT { x, y };
        // 命令产生的 UI(删除确认框/进度框)需要前台宿主,否则可能压在桌面底下看不见。
        // 用隐形菜单宿主:前台化栅栏窗口会提升其 z-band,自愈定时器拉回时
        // 分层窗口跨 band 移动引发重合成闪屏
        let _ = SetForegroundWindow(crate::ui::menu_host_or(hwnd));
        let hr = ctx.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const _);
        let verb_desc = if has_str {
            String::from_utf16_lossy(&wverb[..wverb.iter().position(|c| *c == 0).unwrap_or(64)])
        } else {
            format!("#{}", verb_idx)
        };
        log(&format!("invoke verb '{}' -> hr={:?}", verb_desc, hr));
    }
}

/// 构建与原生桌面一致的"项目"菜单源。优先走桌面 DefView 选中项路线(与
/// Explorer 右键桌面图标 100% 同源,含视图层"重命名");不可用时退回桌面文件
/// 夹 GetUIObjectOf(此时补注入"重命名"保持条目一致)。

fn build_item_menu(hwnd: HWND, paths: &[String]) -> Option<(IContextMenu, Vec<*mut ITEMIDLIST>)> {
    // 注:Explorer 的 WM_GETOBJECT 跨进程不回 IShellView,无法直接取 DefView
    // 选中项菜单;走桌面文件夹 GetUIObjectOf 路线 + 注入"重命名"对齐原生。
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for p in paths {
            match pidl_from_path(p) {
                Some(pidl) => pidls.push(pidl),
                None => {
                    for q in pidls {
                        CoTaskMemFree(Some(q as *const _));
                    }
                    log("item menu: pidl failed");
                    return None;
                }
            }
        }
        let mut children: Vec<*const ITEMIDLIST> = Vec::new();
        for abs in &pidls {
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            if SHBindToParent::<IShellFolder>(*abs, Some(&mut child)).is_err() {
                for q in &pidls {
                    CoTaskMemFree(Some(*q as *const _));
                }
                return None;
            }
            children.push(child as *const ITEMIDLIST);
        }
        let mut child0: *mut ITEMIDLIST = std::ptr::null_mut();
        let parent: IShellFolder = match SHBindToParent(pidls[0], Some(&mut child0)) {
            Ok(p) => p,
            Err(_) => {
                for q in pidls {
                    CoTaskMemFree(Some(q as *const _));
                }
                return None;
            }
        };
        let _ = child0; // child0 是 pidls[0] 内部别名,不能单独释放
        let ctx: IContextMenu = match parent.GetUIObjectOf::<_, IContextMenu>(hwnd, &children, None)
        {
            Ok(c) => c,
            Err(_) => {
                log("item menu: GetUIObjectOf failed");
                for q in pidls {
                    CoTaskMemFree(Some(q as *const _));
                }
                return None;
            }
        };
        Some((ctx, pidls))
    }
}

/// 释放菜单源持有的资源并恢复桌面选中状态
fn free_item_pidls(pidls: Vec<*mut ITEMIDLIST>) {
    for q in pidls {
        unsafe {
            CoTaskMemFree(Some(q as *const _));
        }
    }
}

/// 菜单弹出前必须把 owner 前台化(KB135788/MSDN Shell_NotifyIcon 标准做法):
/// 没有前台权时 TrackPopupMenu 会立即以 0 返回(托盘菜单"弹出即退"),
/// 且点击菜单外的桌面空白不会关闭菜单(僵尸菜单)。owner 用不可感知的
/// 1px 菜单宿主窗口(绝不能用栅栏窗口——前台化会提升其 z-band,自愈
/// 定时器拉回时分层窗口跨 band 移动引发重合成闪屏)。SetForegroundWindow
/// 在进程无前台权限时会静默失败(如合成回调、长时间无真实输入),此时用
/// AttachThreadInput 暂借前台线程的输入状态重试。
pub unsafe fn menu_foreground(hwnd: HWND) -> MenuForegroundGuard {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    if !SetForegroundWindow(hwnd).as_bool() {
        let fg = GetForegroundWindow();
        if fg.0 != 0 {
            let cur = GetCurrentThreadId();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            if fg_thread != 0 && fg_thread != cur
                && AttachThreadInput(cur, fg_thread, true).as_bool()
            {
                let _ = SetForegroundWindow(hwnd);
                let _ = AttachThreadInput(cur, fg_thread, false);
            }
        }
    }
    MenuForegroundGuard { hwnd }
}

pub struct MenuForegroundGuard {
    hwnd: HWND,
}

impl Drop for MenuForegroundGuard {
    fn drop(&mut self) {
        unsafe {
            // 菜单关闭也算交互:2.5s 内推迟壁纸捕获,避开宿主未稳定态的
            // 强制重绘(±4% 亮度闪)
            crate::ui::mark_interaction();
            let _ = PostMessageW(self.hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// 弹出并等待菜单选择,返回命令 id(0=取消)。期间保留 COM 菜单对象,
/// WndProc 可转发动态/自绘子菜单消息(WM_INITMENUPOPUP 等)。
unsafe fn run_item_menu(hwnd: HWND, ctx: &IContextMenu, menu: HMENU, x: i32, y: i32) -> u32 {
    ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
    // owner 用隐形菜单宿主,避免前台化栅栏窗口引发 z-band 往返的闪屏
    let host = crate::ui::menu_host_or(hwnd);
    let _guard = menu_foreground(host);
    let id = track_popup(menu, host, x, y);
    ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    id
}

pub fn show_shell_context_menu_paths(hwnd: HWND, paths: &[String], x: i32, y: i32) {
    if paths.len() <= 1 {
        if let Some(path) = paths.first() {
            show_shell_context_menu(hwnd, path, x, y);
        }
        return;
    }
    unsafe {
        let (ctx, pidls) = match build_item_menu(hwnd, paths) {
            Some(v) => v,
            None => {
                fallback_menu(hwnd, &paths[0], x, y);
                return;
            }
        };
        let Ok(menu) = CreatePopupMenu() else {
            free_item_pidls(pidls);
            return;
        };
        if ctx
            .QueryContextMenu(menu, 0, CMD_FIRST, CMD_LAST, CMF_NORMAL | CMF_CANRENAME)
            .is_err()
        {
            let _ = DestroyMenu(menu);
            free_item_pidls(pidls);
            fallback_menu(hwnd, &paths[0], x, y);
            return;
        }
        if !crate::model::is_recycle_bin(&paths[0]) {
            inject_rename_item(menu, &ctx);
        }
        let id = run_item_menu(hwnd, &ctx, menu, x, y);
        if id == DL_ITEM_RENAME_ID {
            request_rename(&paths[0]);
        } else if (CMD_FIRST..=CMD_LAST).contains(&id) {
            let verb_idx = id - CMD_FIRST;
            if verb_is_rename(&ctx, verb_idx) && !crate::model::is_recycle_bin(&paths[0]) {
                request_rename(&paths[0]);
            } else {
                invoke_command(hwnd, &ctx, verb_idx, x, y);
            }
        }
        free_item_pidls(pidls);
    }
}

/// 图标右键:与 Explorer 完全一致的原生上下文菜单(含"重命名");菜单里的
/// "重命名"被拦截改由栅栏内就地编辑完成。回收站等虚拟条目跳过"重命名"注入。
pub fn show_shell_context_menu(hwnd: HWND, path: &str, x: i32, y: i32) {
    unsafe {
        let (ctx, pidls) = match build_item_menu(hwnd, &[path.to_string()]) {
            Some(v) => v,
            None => {
                log("item menu: build failed -> fallback menu");
                fallback_menu(hwnd, path, x, y);
                return;
            }
        };
        let Ok(menu) = CreatePopupMenu() else {
            free_item_pidls(pidls);
            return;
        };
        if ctx
            .QueryContextMenu(menu, 0, CMD_FIRST, CMD_LAST, CMF_NORMAL | CMF_CANRENAME)
            .is_err()
        {
            let _ = DestroyMenu(menu);
            free_item_pidls(pidls);
            log("item menu: QueryContextMenu failed -> fallback menu");
            fallback_menu(hwnd, path, x, y);
            return;
        }
        let item_count = GetMenuItemCount(menu);
        log(&format!("item menu: {item_count} entries"));
        if !crate::model::is_recycle_bin(path) {
            inject_rename_item(menu, &ctx);
        }
        let id = run_item_menu(hwnd, &ctx, menu, x, y);
        log(&format!("item menu chose id=0x{:X}", id));
        if id == DL_ITEM_RENAME_ID {
            request_rename(path);
        } else if (CMD_FIRST..=CMD_LAST).contains(&id) {
            let verb_idx = id - CMD_FIRST;
            if verb_is_rename(&ctx, verb_idx) && !crate::model::is_recycle_bin(path) {
                request_rename(path);
            } else {
                log(&format!("invoking verb_idx={} for {:?}", verb_idx, path));
                invoke_command(hwnd, &ctx, verb_idx, x, y);
            }
        }
        free_item_pidls(pidls);
    }
}

fn defview_ishellview() -> Option<IShellView> {
    use windows::Win32::UI::Accessibility::ObjectFromLresult;
    unsafe {
        let dv = match find_defview_window() {
            Some(d) => d,
            None => {
                log("dv isv: no defview window");
                return None;
            }
        };
        const WM_GETOBJECT: u32 = 0x003D;
        let iid = <IShellView as ComInterface>::IID;
        let mut res = 0usize;
        let ok = SendMessageTimeoutW(
            dv,
            WM_GETOBJECT,
            WPARAM(0),
            LPARAM(&iid as *const _ as isize),
            SMTO_ABORTIFHUNG,
            500,
            Some(&mut res as *mut usize),
        );
        if ok.0 == 0 {
            log("dv isv: WM_GETOBJECT timeout");
            return None;
        }
        if res == 0 {
            log("dv isv: WM_GETOBJECT returned 0");
            return None;
        }
        let mut psv: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Err(e) = ObjectFromLresult(LRESULT(res as isize), &iid, None, &mut psv) {
            log(&format!("dv isv: ObjectFromLresult failed: {e}"));
            return None;
        }
        let psv: IShellView = std::mem::transmute(psv);
        Some(psv)
    }
}

fn defview_background_menu() -> Option<IContextMenu> {
    use windows::Win32::UI::Shell::SVGIO_BACKGROUND;
    unsafe {
        let psv = defview_ishellview()?;
        psv.GetItemObject::<IContextMenu>(SVGIO_BACKGROUND).ok()
    }
}

/// 查找桌面 SHELLDLL_DefView 窗口(Progman 直属,或 WorkerW 下)。
fn find_defview_window() -> Option<HWND> {
    unsafe {
        let progman = wide("Progman");
        let defview = wide("SHELLDLL_DefView");
        let workerw = wide("WorkerW");
        let cur = FindWindowExW(
            FindWindowW(PCWSTR::from_raw(progman.as_ptr()), None),
            None,
            PCWSTR::from_raw(defview.as_ptr()),
            None,
        );
        if cur.0 != 0 {
            return Some(cur);
        }
        let mut host = FindWindowW(PCWSTR::from_raw(workerw.as_ptr()), None);
        while host.0 != 0 {
            let dv = FindWindowExW(host, None, PCWSTR::from_raw(defview.as_ptr()), None);
            if dv.0 != 0 {
                return Some(dv);
            }
            host = FindWindowExW(None, host, PCWSTR::from_raw(workerw.as_ptr()), None);
        }
        let mut slot: Option<HWND> = None;
        let _ = EnumWindows(
            Some(enum_find_defview),
            LPARAM(&mut slot as *mut Option<HWND> as isize),
        );
        slot
    }
}

unsafe extern "system" fn enum_find_defview(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let slot: &mut Option<HWND> = &mut *(lparam.0 as *mut Option<HWND>);
    if slot.is_some() {
        return BOOL(0);
    }
    let defview = wide("SHELLDLL_DefView");
    let dv = FindWindowExW(hwnd, None, PCWSTR::from_raw(defview.as_ptr()), None);
    if dv.0 != 0 {
        *slot = Some(dv);
        return BOOL(0);
    }
    BOOL(1)
}

/// 桌面空白区右键:弹出 Explorer 桌面原生菜单(查看/排序方式/刷新/粘贴/新建/
/// 显示设置/个性化…),并把 DeskFence 命令注入成子菜单。返回 DL_CMD_* 时由调用方执行。
pub fn show_desktop_context_menu(
    hwnd: HWND,
    x: i32,
    y: i32,
    align_mode: &str,
    render_mode: &str,
) -> u32 {
    unsafe {
        let ctx: IContextMenu = match defview_background_menu() {
            Some(c) => c,
            None => {
                let psf = match SHGetDesktopFolder() {
                    Ok(f) => f,
                    Err(_) => return 0,
                };
                match psf.GetUIObjectOf::<_, IContextMenu>(hwnd, &[], None) {
                    Ok(c) => c,
                    Err(_) => return 0,
                }
            }
        };
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        if ctx
            .QueryContextMenu(menu, 0, CMD_FIRST, CMD_LAST, CMF_NORMAL)
            .is_err()
        {
            let _ = DestroyMenu(menu);
            return 0;
        }
        let Ok(sub) = CreatePopupMenu() else {
            let _ = DestroyMenu(menu);
            return 0;
        };
        append_menu(sub, DL_CMD_ADD_FENCE, "新建栅栏");
        append_menu(sub, DL_CMD_HELP, "使用说明");
        append_menu(sub, DL_CMD_SHOW_ALL, "显示全部栅栏");
        append_menu(sub, DL_CMD_HIDE_ALL, "隐藏全部栅栏");
        append_menu(sub, DL_CMD_UNDO, "撤销上次布局调整");
        append_menu(sub, DL_CMD_REFRESH, "刷新");
        // 三档对齐:点击在 自动→网格→自由 间循环(完整设置在托盘菜单)
        let mode_label = match align_mode {
            "grid" => "对齐方式: 网格(图标格倍数)",
            "free" => "对齐方式: 自由移动",
            _ => "对齐方式: 自动(固定间隔)",
        };
        append_menu(sub, DL_CMD_AUTO_ALIGN, mode_label);
        // 渲染模式:透明(默认,兼容动态壁纸) ↔ 精确(壁纸底+ClearType,与原生一致)
        let render_label = match render_mode {
            "precise" => "渲染模式: 精确(壁纸底,与原生一致)",
            _ => "渲染模式: 透明(动态壁纸兼容)",
        };
        append_menu(sub, DL_CMD_RENDER_MODE, render_label);
        append_separator(sub);
        append_menu(sub, DL_CMD_QUIT, "退出 DeskFence");
        let label = wide("DeskFence");
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            sub.0 as usize,
            PCWSTR::from_raw(label.as_ptr()),
        );
        ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
        let host = crate::ui::menu_host_or(hwnd);
        let _ = SetForegroundWindow(host);
        let id = track_popup(menu, host, x, y);
        ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        if (CMD_FIRST..=CMD_LAST).contains(&id) {
            invoke_command(hwnd, &ctx, id - CMD_FIRST, x, y);
            let _ = DestroyMenu(menu);
            return 0;
        }
        let _ = DestroyMenu(menu);
        id
    }
}

/// 内置降级菜单(系统菜单链路完全不可用时的兜底,覆盖桌面右键核心操作)
fn fallback_menu(hwnd: HWND, path: &str, x: i32, y: i32) {
    const F_OPEN: u32 = 1;
    const F_OPENWITH: u32 = 2;
    const F_LOCATE: u32 = 3;
    const F_RENAME: u32 = 4;
    const F_DELETE: u32 = 5;
    const F_PROPERTIES: u32 = 6;
    const F_COPY: u32 = 7;
    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    append_menu(menu, F_OPEN, "打开");
    append_menu(menu, F_OPENWITH, "打开方式…");
    append_separator(menu);
    append_menu(menu, F_COPY, "复制");
    append_separator(menu);
    append_menu(menu, F_LOCATE, "打开所在位置");
    append_menu(menu, F_RENAME, "重命名");
    append_menu(menu, F_DELETE, "删除");
    append_separator(menu);
    append_menu(menu, F_PROPERTIES, "属性");
    let host = crate::ui::menu_host_or(hwnd);
    let id = track_popup(menu, host, x, y);
    unsafe {
        let _ = DestroyMenu(menu);
    };
    match id {
        F_OPEN => open_path(path),
        F_OPENWITH => open_with(path),
        F_COPY => {
            let _ = crate::ole::clipboard_set_files(&[path.to_string()]);
        }
        F_LOCATE => open_in_explorer(path),
        F_RENAME => {
            request_rename(path);
        }
        F_DELETE => delete_to_recycle_bin(hwnd, path),
        F_PROPERTIES => show_properties(path),
        _ => {}
    }
}

/// "打开方式…"对话框
pub fn open_with(path: &str) {
    let w = wide(path);
    let op = wide("openas");
    unsafe {
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR::from_raw(op.as_ptr()),
            PCWSTR::from_raw(w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 删除到回收站(与 Explorer 的右键删除一致,可还原)
pub fn delete_to_recycle_bin(hwnd: HWND, path: &str) {
    delete_to_recycle_bin_many(hwnd, &[path.to_string()]);
}

fn multistring(paths: &[String]) -> Vec<u16> {
    let mut data = Vec::new();
    for path in paths {
        data.extend(wide(path).into_iter().take_while(|c| *c != 0));
        data.push(0);
    }
    data.push(0);
    data
}

pub fn copy_files_to_desktop(hwnd: HWND, paths: &[String]) -> bool {
    let Some(target) = desktop_dir() else {
        return false;
    };
    if paths.is_empty() {
        return false;
    }
    let from = multistring(paths);
    let mut to = wide(&target.to_string_lossy());
    to.push(0);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = FO_COPY;
    op.pFrom = PCWSTR::from_raw(from.as_ptr());
    op.pTo = PCWSTR::from_raw(to.as_ptr());
    op.fFlags = (FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION).0 as u16;
    unsafe { SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool() }
}

pub fn delete_to_recycle_bin_many(hwnd: HWND, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let from = multistring(paths);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = FO_DELETE;
    op.pFrom = PCWSTR::from_raw(from.as_ptr());
    op.fFlags = FOF_ALLOWUNDO.0 as u16;
    unsafe {
        let _ = SHFileOperationW(&mut op);
    }
}

/// 显示文件属性对话框
pub fn show_properties(path: &str) {
    let w = wide(path);
    let op = wide("properties");
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.lpVerb = PCWSTR::from_raw(op.as_ptr());
        sei.lpFile = PCWSTR::from_raw(w.as_ptr());
        sei.nShow = SW_SHOWNORMAL.0 as i32;
        let _ = ShellExecuteExW(&mut sei);
    }
}

/// 重命名文件(普通 rename;目标已存在或失败返回 false)
pub fn rename_path(old: &str, new_name: &str) -> bool {
    let Some(parent) = std::path::Path::new(old).parent() else {
        return false;
    };
    let newp = parent.join(new_name);
    if newp == std::path::Path::new(old) {
        return true;
    }
    let same_case_insensitive = newp.to_string_lossy().eq_ignore_ascii_case(old);
    if !same_case_insensitive && newp.exists() {
        return false;
    }
    std::fs::rename(old, &newp).is_ok()
}

// ---------------- 系统外观与设置 ----------------

/// 系统浅色/深色主题(AppsUseLightTheme),失败回退深色(false)
pub fn is_light_theme() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("AppsUseLightTheme");
    let mut v: u32 = 0;
    let mut sz = std::mem::size_of::<u32>() as u32;
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut ty),
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_ok()
    };
    ok && v == 1
}

/// Windows 系统强调色(HKCU DWM AccentColor, 0x00BBGGRR),失败回退默认蓝
pub fn system_accent() -> [f32; 3] {
    let key = wide(r"Software\Microsoft\Windows\DWM");
    let val = wide("AccentColor");
    let mut v: u32 = 0;
    let mut sz = std::mem::size_of::<u32>() as u32;
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_DWORD,
            Some(&mut ty),
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            Some(&mut sz),
        )
        .is_ok()
    };
    if ok && v != 0 {
        [
            (v & 0xFF) as f32 / 255.0,
            ((v >> 8) & 0xFF) as f32 / 255.0,
            ((v >> 16) & 0xFF) as f32 / 255.0,
        ]
    } else {
        [0.30, 0.62, 1.0]
    }
}

// 注意:不要使用 0x052C 消息生成 WorkerW 宿主 —— 每次调用都会让 Win11 桌面层
// 在 Progman/WorkerW 之间切换宿主,导致桌面反复重建(栅栏消失、桌面空白)。
// 栅栏只使用 Explorer 自然存在的宿主窗口。

// ---------------- 开机自启 ----------------

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "DeskFence";
/// 旧版本(DeskLens3)的自启项名,设置新自启时顺手清掉
const RUN_VALUE_LEGACY: &str = "DeskLens3";

pub fn get_autostart() -> bool {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut sz: u32 = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
            RRF_RT_REG_SZ,
            Some(&mut ty),
            None,
            Some(&mut sz),
        )
        .is_ok()
    }
}

pub fn set_autostart(on: bool) -> bool {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    if exe.is_empty() {
        return false;
    }
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let legacy = wide(RUN_VALUE_LEGACY);
    let value = format!("\"{}\"", exe);
    let vw = wide(&value);
    unsafe {
        // 无论开/关,都先清掉旧版 DeskLens3 的自启项,避免新旧并存重复启动
        let mut lhkey: HKEY = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut lhkey,
        )
        .is_ok()
            && lhkey.0 != 0
        {
            let _ = RegDeleteValueW(lhkey, PCWSTR::from_raw(legacy.as_ptr()));
            let _ = RegCloseKey(lhkey);
        }
        if on {
            let mut hkey: HKEY = HKEY::default();
            let ok = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(key.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
            .is_ok();
            if !ok {
                return false;
            }
            let set = RegSetValueExW(
                hkey,
                PCWSTR::from_raw(name.as_ptr()),
                0,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    vw.as_ptr() as *const u8,
                    vw.len() * 2,
                )),
            )
            .is_ok();
            let _ = RegCloseKey(hkey);
            set
        } else {
            let mut hkey: HKEY = HKEY::default();
            let ok = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(key.as_ptr()),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            )
            .is_ok();
            if !ok {
                return false;
            }
            let del = RegDeleteValueW(hkey, PCWSTR::from_raw(name.as_ptr())).is_ok();
            let _ = RegCloseKey(hkey);
            del
        }
    }
}

// ---------------- 桌面目录变更监听 ----------------

static DESKTOP_DIRTY: AtomicBool = AtomicBool::new(false);

/// 取走并清除"桌面有变更"标记(由监听线程置位,全局定时器消费)
pub fn take_desktop_dirty() -> bool {
    DESKTOP_DIRTY.swap(false, Ordering::Relaxed)
}

/// 后台线程阻塞式监听桌面目录变更,置位 DIRTY 标记。
/// 与 30 秒轮询并存:监听提供即时刷新,轮询兜底。
fn start_directory_watcher(dir: std::path::PathBuf) {
    let wdir = wide(&dir.to_string_lossy());
    std::thread::spawn(move || unsafe {
        let handle = CreateFileW(
            PCWSTR::from_raw(wdir.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
            HANDLE::default(),
        );
        let Ok(handle) = handle else { return };
        if handle.is_invalid() {
            return;
        }
        let mut buf: Vec<u8> = vec![0u8; 65536];
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_CREATION;
        loop {
            let mut ret: u32 = 0;
            let ok = ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() as u32,
                BOOL(0),
                filter,
                Some(&mut ret),
                None,
                None,
            );
            if ok.is_err() {
                break;
            }
            DESKTOP_DIRTY.store(true, Ordering::Relaxed);
            // 合并文件操作风暴,避免每次变更都全量重扫
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let _ = CloseHandle(handle);
    });
}

pub fn start_desktop_watcher() {
    let mut seen = HashSet::new();
    for dir in [desktop_dir(), public_desktop_dir()].into_iter().flatten() {
        let key = dir.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            start_directory_watcher(dir);
        }
    }
}

// ---------------- 壁纸变化监听(事件驱动) ----------------

/// 幻灯片轮换不会广播 WM_SETTINGCHANGE(这正是历史上 15s 轮询存在的原因),
/// 但 Explorer 每次轮换都会重写主题缓存:Themes\TranscodedWallpaper 与
/// CachedImageFiles\*。盯住该目录即可毫秒级感知轮换;手动换壁纸另有
/// WM_SETTINGCHANGE 广播(托盘窗口已处理)。两者就位后,例行轮询只剩
/// 长间隔兜底,栅栏背景在壁纸切换后 ~250-500ms 内跟上,视觉无感。
fn themes_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let p = std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Themes");
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// 监听主题缓存目录(递归,覆盖 CachedImageFiles)。有变化即置位脏标记并
/// 向托盘窗口投递 notify_msg,由 UI 侧防抖后"捕获-比对-变了才重绘"。
pub fn start_wallpaper_watcher(notify_hwnd: HWND, notify_msg: u32) {
    let Some(dir) = themes_dir() else {
        return;
    };
    let wdir = wide(&dir.to_string_lossy());
    std::thread::spawn(move || unsafe {
        let handle = CreateFileW(
            PCWSTR::from_raw(wdir.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
            HANDLE::default(),
        );
        let Ok(handle) = handle else { return };
        if handle.is_invalid() {
            return;
        }
        let mut buf: Vec<u8> = vec![0u8; 16384];
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_CREATION;
        loop {
            let mut ret: u32 = 0;
            let ok = ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() as u32,
                BOOL(1), // 递归:CachedImageFiles 也要覆盖
                filter,
                Some(&mut ret),
                None,
                None,
            );
            if ok.is_err() {
                break;
            }
            let _ = PostMessageW(notify_hwnd, notify_msg, WPARAM(0), LPARAM(0));
            // Explorer 写缓存是"临时文件+改名"多步操作,这里只粗合并;
            // 精确防抖由 UI 侧 250ms 定时器完成
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = CloseHandle(handle);
    });
}

/// 当前壁纸"签名":每显示器当前壁纸路径 + 背景色 + 填充模式。手动换壁纸
/// 和幻灯片轮换都会改变 GetWallpaper 的返回(注册表与 TranscodedWallpaper
/// 缓存则不一定更新,实测部分机器换壁纸根本不写该目录)。调用方每秒做一次
/// 纯字符串比较,即可秒级感知变化,替代高频像素级重捕获。
/// DesktopWallpaper 协同类的 CLSID(windows crate 未导出此常量,取 shlguid.h;
/// 注意是 C2CF**3**110,写错一位 CoCreateInstance 静默失败)
const CLSID_DESKTOP_WALLPAPER: windows::core::GUID =
    windows::core::GUID::from_u128(0xc2cf3110_0460_4fc1_b9d0_8a1c0c9cc4bd);

pub fn wallpaper_signature() -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::UI::Shell::IDesktopWallpaper;
    unsafe {
        // 注:部分定制环境(本机实测)该 coclass 未注册(REGDB_E_CLASSNOTREG),
        // 返回 None 由调用方一次性告警并依赖其它信号源,属预期降级
        let dp: IDesktopWallpaper =
            CoCreateInstance(&CLSID_DESKTOP_WALLPAPER, None, CLSCTX_ALL).ok()?;
        let mut parts: Vec<String> = Vec::new();
        let n = dp.GetMonitorDevicePathCount().ok()?;
        for i in 0..n {
            let dev = dp.GetMonitorDevicePathAt(i).ok()?;
            let dev_s = pwstr_to_string(dev);
            if let Ok(wp) = dp.GetWallpaper(PCWSTR(dev.as_ptr())) {
                parts.push(pwstr_to_string(wp).unwrap_or_default());
                CoTaskMemFree(Some(wp.0 as *const _));
            } else {
                parts.push(String::new());
            }
            parts.push(dev_s.unwrap_or_default());
            CoTaskMemFree(Some(dev.0 as *const _));
        }
        let bg = dp
            .GetBackgroundColor()
            .map(|c| c.0.to_string())
            .unwrap_or_default();
        let pos = dp.GetPosition().map(|p| p.0.to_string()).unwrap_or_default();
        Some(format!("{bg}|{pos}|{}", parts.join("\u{1}")))
    }
}

unsafe fn pwstr_to_string(p: windows::core::PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *p.0.add(len) != 0 {
        len += 1;
    }
    String::from_utf16(std::slice::from_raw_parts(p.0, len)).ok()
}

#[cfg(test)]
mod tests {
    use super::select_image_size;

    #[test]
    fn selects_smallest_size_at_least_target() {
        assert_eq!(select_image_size([16, 256, 48, 32, 64], 40), Some(48));
        assert_eq!(select_image_size([64, 48, 96], 48), Some(48));
    }

    #[test]
    fn selects_largest_size_when_all_are_below_target() {
        assert_eq!(select_image_size([16, 32, 48], 64), Some(48));
    }

    #[test]
    fn ignores_zero_sizes_and_handles_no_available_size() {
        assert_eq!(select_image_size([0, 32, 0], 24), Some(32));
        assert_eq!(select_image_size([0, 0], 24), None);
        assert_eq!(select_image_size([], 24), None);
    }
}
