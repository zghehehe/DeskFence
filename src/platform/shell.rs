//! Shell 集成：桌面扫描、文件图标、打开文件、系统右键菜单

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{Interface, BOOL, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, DIB_RGB_COLORS, HGDIOBJ, LOGFONTW,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileAttributesW, ReadDirectoryChangesW, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_SYSTEM, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, IBindCtx, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
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
    GetWindowThreadProcessId, InsertMenuW, PostMessageW, SendMessageTimeoutW, SetForegroundWindow,
    SystemParametersInfoW, TrackPopupMenu, HICON, HMENU, MENU_ITEM_FLAGS, MF_BYPOSITION,
    MF_CHECKED, MF_POPUP, MF_SEPARATOR, SMTO_ABORTIFHUNG, SM_CXICON, SPI_GETICONTITLELOGFONT,
    SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_RETURNCMD, WM_NULL,
};

use crate::model::FileItem;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// shell 模块日志(转 logging::log 落盘)
fn log(line: &str) {
    crate::logging::log(line);
}

/// 读取系统桌面原生图标尺寸（像素）：HKCU\...\Bags\1\Desktop\IconSize。
/// 缺失/无效回退 48:Windows 默认视图就是"中等图标"=48,从未 Ctrl+滚轮
/// 过的新配置档常常没有该值——回退 32 会让栅栏图标比原生小一号,而实测
/// 格距的留白补偿会把格子尺寸修正到与原生一致,形成"格子对、图标小"
/// 的掩蔽组合(2026-09-16 用户实拍,勿回退 32)。
/// IconSize 注册表值归一(纯核):8..=256 生效,缺失/越界回退 48
/// (48=Windows"中等图标"默认;勿回退 32,见 desktop_icon_size 注释)
pub fn icon_size_or_default(v: Option<u32>) -> f32 {
    match v {
        Some(v) if (8..=256).contains(&v) => v as f32,
        _ => 48.0,
    }
}

pub fn desktop_icon_size() -> f32 {
    let mut v: u32 = 0;
    let mut sz = std::mem::size_of::<u32>() as u32;
    let key = wide(r"Software\Microsoft\Windows\Shell\Bags\1\Desktop");
    let val = wide("IconSize");
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    // SAFETY: key/val 是 NUL 结尾宽串（wide()），调用期间存活；v/sz/ty 是
    // 栈上输出槽位；RRF_RT_REG_DWORD 限定类型不匹配即失败，不写 v。
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
    icon_size_or_default(if ok { Some(v) } else { None })
}

/// 读取 WindowMetrics\IconSpacing / IconVerticalSpacing（REG_SZ，如 "-1130"）。
/// 两个值都转换为相对于 32px 图标的逻辑留白；缺失或无效时使用
/// 紧凑的保守回退，避免把失效的系统值放大成栅栏内的大块空白。
/// IconSpacing/IconVerticalSpacing 字符串解析(纯核):twips(如 "-1130")
/// → |v|/15 像素 → 扣 32px 基准图标得留白 → 钳 16..96;不可解析=None
pub fn spacing_pad_from_twips(s: &str) -> Option<f32> {
    let v = s.trim().parse::<i32>().ok()?;
    let px = (v.unsigned_abs() as f32) / 15.0; // twips → 像素
    let pad = px - 32.0; // 扣除 32px 基准图标，得到留白
    Some(pad.clamp(16.0, 96.0))
}

pub fn desktop_cell_pads() -> (f32, f32) {
    let read = |name: &str| -> Option<f32> {
        let key = wide(r"Control Panel\Desktop\WindowMetrics");
        let val = wide(name);
        let mut buf = [0u16; 32];
        let mut sz = (buf.len() * 2) as u32;
        let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
        // SAFETY: 字符串参数同 desktop_icon_size；buf 是 64 字节栈缓冲、
        // sz 先传字节数——值长于缓冲时 API 返回失败（不越界写），
        // from_utf16 前按 NUL 截断。
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
        spacing_pad_from_twips(&s)
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
    // SAFETY: 同 desktop_icon_size：NUL 宽串 + 栈输出槽位，类型限定失败即不写。
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
/// LOGFONT.lfHeight → 图标名字号像素(纯核):负值本身即像素字符高度
/// (已随 DPI 缩放,不能再按磅值换算乘 DPI,否则字号双重放大);
/// 0=回退 12;结果钳 8..48
pub fn font_px_from_lfheight(lf_height: i32) -> f32 {
    let px = if lf_height < 0 {
        -(lf_height as f32)
    } else if lf_height > 0 {
        lf_height as f32
    } else {
        12.0
    };
    px.clamp(8.0, 48.0)
}

/// Read Explorer's configured desktop icon caption font.
pub fn desktop_icon_font() -> (String, f32, i32) {
    // SAFETY: 全零 LOGFONTW 是合法初始值（纯数据结构，无引用/指针字段）；
    // SPI_GETICONTITLELOGFONT 契约：pvParam 指向 LOGFONTW、uiParam 传其
    // 字节数；lf 是栈变量，调用期间有效；lfFaceName 定长数组按 NUL 截断。
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    // SAFETY: 同上。
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
    (
        if family.is_empty() {
            "Segoe UI".into()
        } else {
            family
        },
        font_px_from_lfheight(lf.lfHeight),
        lf.lfWeight,
    )
}

/// 桌面目录（已知文件夹优先，回退 USERPROFILE\Desktop）
pub fn desktop_dir() -> Option<std::path::PathBuf> {
    // SAFETY(整块): SHGetKnownFolderPath 成功时返回 CoTaskMemAlloc 分配的
    // PWSTR，所有权归调用方——本函数读完后必须且只用 CoTaskMemFree 释放
    // 一次（p 不再被使用）；to_string 沿 NUL 扫描只读。
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
    // SAFETY: 承接上方所有权论证：释放 COM 分配的路径串（仅此一次）。
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
    // SAFETY: 所有权约定同 desktop_dir：成功路径的 PWSTR 由本函数
    // CoTaskMemFree 配对释放一次。
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_PublicDesktop, KF_FLAG_DEFAULT, None).ok()? };
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: 承接上方：COM 分配的串只释放这一次。
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
    // SAFETY: 全零 SHFILEINFOW 合法（纯数据结构）；SHGFI_DISPLAYNAME 契约：
    // psfi 指向调用方缓冲、cbFileInfo 传其大小；w 是 NUL 宽串；
    // szDisplayName 定长数组按 NUL 截断。
    let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let w = wide(&path.to_string_lossy());
    // SAFETY: 同上。
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
///
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
        // 元数据可能被创建方进程短暂锁住(刚新建的文件):目录项侧失败时
        // 用路径侧重试一次,仍失败才跳过——跳过=该轮 rescan 认为文件不存在,
        // 会把刚新建的文件当"消失"处理(2026-09-03 用户实测位置漂移)
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => match std::fs::metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            },
        };
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
    let per = n.div_ceil(threads);
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
                // SAFETY: COM 初始化/反初始化必须按线程配对：本块在本工作线程
                // 开头 init、结尾 CoUninitialize，中间的 SHGFI 调用落在已初始化
                // 的 STA 线程上；"已初始化"的返回被忽略（配对计数仍平衡）。
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
                // SAFETY: 与线程开头的 CoInitializeEx 配对（见上）。
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
pub fn prewarm_icon_cache(paths: &[String], px: f32) -> std::collections::HashMap<String, Vec<u8>> {
    const THREADS: usize = 4;
    let n = paths.len();
    let mut merged = std::collections::HashMap::new();
    if n == 0 {
        return merged;
    }
    let threads = THREADS.min(n);
    let per = n.div_ceil(threads);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for t in 0..threads {
            let slice = &paths[t * per..((t + 1) * per).min(n)];
            handles.push(scope.spawn(move || {
                // SAFETY: COM init/uninit 按线程配对（论证同
                // resolve_display_names 的工作线程）。
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                let mut local: std::collections::HashMap<String, Vec<u8>> =
                    std::collections::HashMap::new();
                for p in slice {
                    crate::render::get_icon_buffer(&mut local, p, px);
                }
                // SAFETY: 与线程开头的 CoInitializeEx 配对。
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
    // SAFETY(整块): 全零 SHFILEINFOW 合法；SHGetFileInfoW 的 psfi/cbFileInfo
    // 按契约传栈缓冲与大小，w 为 NUL 宽串；成功时 hIcon 所有权移交本函数→
    // 直接返回给调用方（由调用方 DestroyIcon）；SHGetImageList 返回的
    // IImageList 是 COM 包装，离开作用域自动 Release；list.GetIcon 产出的
    // HICON 同样移交调用方。
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
        if ok && !sfi.hIcon.0.is_null() {
            return Some(sfi.hIcon);
        }
    }
    // 兜底:按"实测尺寸"挑最接近目标 px 的 shell 图像列表档位。
    // 不能按逻辑档位名挑(150% DPI 下 SHIL_LARGE 实际是 48px、EXTRALARGE 是 72px,
    // 按名字挑会拿 72px 图标再缩到 48 → 模糊)。
    // SAFETY: 参数契约同上（栈缓冲+NUL 宽串）。
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
    let draw_flags = ILD_TRANSPARENT.0 | (overlay_index << 8); // INDEXTOOVERLAYMASK
    const KINDS: [u32; 4] = [
        windows::Win32::UI::Shell::SHIL_LARGE,
        windows::Win32::UI::Shell::SHIL_EXTRALARGE,
        windows::Win32::UI::Shell::SHIL_JUMBO,
        windows::Win32::UI::Shell::SHIL_SMALL,
    ];
    let mut lists = Vec::new();
    for kind in KINDS {
        // SAFETY: SHGetImageList 返回系统图像列表的 COM 包装（drop 自动
        // Release）；GetIconSize 的 cx/cy 是栈输出槽位；GetIcon 的
        // INDEXTOOVERLAYMASK 位拼装见上方 draw_flags。
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
    // SAFETY: list 是有效的系统图像列表 COM 包装；产出的 HICON 所有权
    // 移交调用方（由调用方 DestroyIcon）。
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
        let data = w * h * 4;
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
        // SAFETY: ii/bm 为全零栈结构；GetIconInfo 契约——hIcon 有效、
        // piconinfo 指向调用方 ICONINFO；成功时其中 hbmColor/hbmMask 两个
        // GDI 位图所有权移交调用方（随后 DeleteObject 配对释放，失败路径
        // 也释放）；GetObjectW 只按 BITMAP 大小读位图头。
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

    // SAFETY(整块): 诊断路径，与生产同一套契约：COM init 无配对 uninit
    // （进程即将退出，诊断命令路径无影响）；zeroed 结构合法；SHGFI 参数
    // 契约同 get_system_icon_hicon；GetIcon 产出的 HICON 用完当场
    // DestroyIcon（不泄漏）。
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
        if ok && !sfi.hIcon.0.is_null() {
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
            let flags_overlay = ILD_TRANSPARENT.0 | (overlay << 8);
            if let Ok(icon2) = list.GetIcon(image_index, flags_overlay) {
                if let Some(b) = crate::render::icon_pixels(icon2, px) {
                    save_dump(&format!("{prefix}_v2_imglist_ovl"), &b, px, px);
                }
                let _ = DestroyIcon(icon2);
            }
            if let Ok(icon3) = list.GetIcon(image_index, ILD_TRANSPARENT.0) {
                if let Some(b) = crate::render::icon_pixels(icon3, px) {
                    save_dump(&format!("{prefix}_v3_imglist_base"), &b, px, px);
                }
                let _ = DestroyIcon(icon3);
            }
        }
        println!("icondump done");
        // SAFETY: 与函数开头的 CoInitializeEx 配对(2026-09-17 补,
        // 让"init/uninit 按线程配对"在诊断路径同样成立)。
        windows::Win32::System::Com::CoUninitialize();
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
    // SAFETY: w 是 NUL 宽串（SHCreateItemFromParsingName 同步读）；
    // GetImage 产出的 HBITMAP 所有权移交调用方（由调用方 DeleteObject）。
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
    // SAFETY: 全零 SHFILEINFOW 合法；SHGFI 参数契约同 get_system_icon_hicon；
    // 成功时 hIcon 所有权移交调用方。
    let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let flags = SHGFI_ICON | SHGFI_LARGEICON | SHGFI_ADDOVERLAYS;
    let w = wide(path);
    // SAFETY: 同上。
    unsafe {
        SHGetFileInfoW(
            PCWSTR::from_raw(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            size_of_val(&sfi) as u32,
            flags,
        );
    }
    if sfi.hIcon.0.is_null() {
        None
    } else {
        Some(sfi.hIcon)
    }
}

/// 打开文件或目录（双击图标）
pub fn open_path(path: &str) {
    let w = wide(path);
    // SAFETY: info 为全零+cbSize 按契约填充；lpFile 指向 NUL 宽串 w
    // （同步调用，期间存活）；SHELLEXECUTEINFOW 其余字段为 0/null 合法。
    unsafe {
        // 与 Explorer 双击一致:不给动词、带 SEE_MASK_INVOKEIDLIST,
        // 由 shell 调用默认动词(部分条目默认动词不是 "open")
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        // SEE_MASK_INVOKEIDLIST(0x0C = DEFAULT|INVOKEIDLIST):用默认动词激活
        info.fMask = 0x000C;
        info.lpFile = PCWSTR::from_raw(w.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        let _ = ShellExecuteExW(&mut info);
    }
}

/// 打开回收站资源管理器窗口(双击回收站图标)
pub fn open_recycle_bin() {
    let file = wide(crate::model::RECYCLE_BIN_PATH);
    let verb = wide("open");
    // SAFETY: 三个宽串均 NUL 结尾且在同步调用期间存活；其余参数为 null 合法。
    unsafe {
        let _ = ShellExecuteW(
            None,
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
    // SAFETY: 同 open_recycle_bin：NUL 宽串在同步调用期间存活。
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR::from_raw(op.as_ptr()),
            PCWSTR::from_raw(w.as_ptr()),
            PCWSTR::from_raw(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 用系统默认浏览器打开 URL("检查更新"用):应用进程自身不发起任何
/// 网络请求,零联网承诺不受影响
pub fn open_url(url: &str) {
    let verb = wide("open");
    let target = wide(url);
    // SAFETY: 同 open_recycle_bin：NUL 宽串在同步调用期间存活。
    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR::from_raw(verb.as_ptr()),
            PCWSTR::from_raw(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 追加一个菜单项
pub fn append_menu(menu: HMENU, id: u32, text: &str) {
    let w = wide(text);
    // SAFETY: menu 是调用方持有的有效菜单；AppendMenuW 同步复制 w 字符串。
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
    // SAFETY: 同 append_menu：有效菜单 + 同步复制的 NUL 宽串。
    unsafe {
        let _ = AppendMenuW(menu, MF_CHECKED, id as usize, PCWSTR::from_raw(w.as_ptr()));
    }
}

pub fn append_separator(menu: HMENU) {
    // SAFETY: 分隔项无字符串指针；menu 是调用方持有的有效菜单。
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// 追加一个带勾选标记的弹出式子菜单父项(对钩显示状态+右侧箭头,
/// 如"✓ 自动分类 ▸"):父项点击只能展开子菜单,不承载命令(Win32 语义)
pub fn append_submenu_checked(menu: HMENU, text: &str, submenu: HMENU) {
    let w = wide(text);
    // SAFETY: MF_POPUP 下 usize 参数承载子菜单句柄（Win32 语义）；
    // 其余同 append_menu。
    unsafe {
        let _ = AppendMenuW(
            menu,
            MF_POPUP | MF_CHECKED,
            submenu.0 as usize,
            PCWSTR::from_raw(w.as_ptr()),
        );
    }
}

/// 追加一个弹出式子菜单(如"排序 ▸")
pub fn append_submenu(menu: HMENU, text: &str, submenu: HMENU) {
    let w = wide(text);
    // SAFETY: 同 append_submenu_checked。
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
    // SAFETY: menu 是调用方构建的有效菜单、hwnd 是前台化过的 owner
    //（见 menu_foreground 前置条件）；TPM_RETURNCMD 模式同步返回命令 id。
    unsafe {
        // 默认左键选择 + 返回命令 id（不带 TPM_RIGHTBUTTON，避免左键点菜单项不触发）
        let r = TrackPopupMenu(menu, TPM_RETURNCMD, x, y, None, hwnd, None);
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
// DeskFence 自有命令 id(0x6001+)。(2026-09-16 提 pub 供 tests/ 断言区间)
pub const CMD_FIRST: u32 = 1;
pub const CMD_LAST: u32 = 0x5FFF;
/// 注入的"重命名"菜单项 id(在 shell 动词区间与 DL_CMD 之外)
pub const DL_ITEM_RENAME_ID: u32 = 0x6008;

/// "重命名"由 Explorer 桌面视图层(DefView)注入,纯 IContextMenu 菜单不含它;
/// 在"删除"与"属性"之间补上同款菜单项,保持与原生逐项一致。文案跟随系统
/// 安装语言(zh-CN:重命名(&M),其余:Rename)。
fn inject_rename_item(menu: HMENU, ctx: &IContextMenu) {
    // SAFETY: menu 是有效菜单；GetMenuItemID/InsertMenuW 按位置操作菜单
    // （同步）；w 是 NUL 宽串；ctx 只被 verb_is_rename 只读查询。
    unsafe {
        let count = GetMenuItemCount(Some(menu));
        if count < 3 {
            return;
        }
        for i in 0..count {
            let id = GetMenuItemID(menu, i);
            if id == DL_ITEM_RENAME_ID {
                return;
            }
            if (CMD_FIRST..=CMD_LAST).contains(&id) && verb_is_rename(ctx, id - CMD_FIRST) {
                return;
            }
        }
        // 菜单文案跟随应用语言设置(2026-09-11 前=注册表安装语言判定)
        let text = crate::lang::rename_item();
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
        // SAFETY: ctx 是 ACTIVE_CONTEXT_MENU 里的有效 COM 对象（本线程
        // 主持菜单模态循环期间存活）；HandleMenuMsg2 的 result 是栈上
        // [out] 槽位；消息参数由 wndproc 原样转入（系统所有）。
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
// windows crate 未导出这个掩码(0.62 仍缺),按 shlobj_core.h 补定义
/// Explorer 桌面图标菜单带 CMF_CANRENAME,shell 因此输出视图级"重命名"动词
const CMF_CANRENAME: u32 = 0x00100000;
const CMIC_MASK_UNICODE: u32 = 0x00004000;

/// DeskFence 子菜单命令 id(桌面背景右键菜单里注入,由 menu.rs 的
/// dispatch_desktop_command 分派)
pub const DL_CMD_ADD_FENCE: u32 = 0x6001;
pub const DL_CMD_SHOW_ALL: u32 = 0x6002;
pub const DL_CMD_HIDE_ALL: u32 = 0x6003;
pub const DL_CMD_UNDO: u32 = 0x6004;
pub const DL_CMD_AUTO_ALIGN: u32 = 0x6005;
pub const DL_CMD_REFRESH: u32 = 0x6006;
pub const DL_CMD_QUIT: u32 = 0x6007;
/// 渲染模式切换(透明 ↔ 精确)
pub const DL_CMD_RENDER_MODE: u32 = 0x6009;

/// 系统"图标标题"原始 LOGFONT(与 Explorer 桌面文字同源;精确模式 GDI 绘制用)
pub fn icon_title_logfont() -> Option<LOGFONTW> {
    // SAFETY: 契约同 desktop_icon_font：全零 LOGFONTW + SPI 按字节数写入
    // 栈变量。
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    // SAFETY: 同上。
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

/// PW_RENDERFULLCONTENT:PrintWindow 捕获 DWM 合成内容(Win8.1+;windows crate 0.62 仍未导出该常量)
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(0x00000002);

/// 捕获一个窗口的合成像素(顶层 32bpp BGRA,自上而下),alpha 全部置 255。
/// 精确模式用它抓取桌面宿主(Progman/WorkerW)上实际显示的壁纸。
/// 捕获窗口像素。失败时返回原因字符串(PrintWindow 失败码/矩形异常等),
/// 供启动期诊断"快照迟迟不可用"的具体环节。
pub fn capture_window_pixels(hwnd: HWND) -> Result<(Vec<u8>, u32, u32), String> {
    // SAFETY(整块): GDI 资源全程成对：GetDC/ReleaseDC、CreateCompatibleDC/
    // DeleteDC、CreateDIBSection 的 DIB/DeleteObject、SelectObject 之后必
    // 还原旧对象再删除；CreateDIBSection 的 bits 指针在 DIB 存活期间有效，
    // from_raw_parts 的长度=biWidth*biHeight*4 与 32bpp DIB 布局一致（biHeight
    // 取负=自上而下行序）；PrintWindow 同步完成后立即拷贝像素再释放。
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
        let hdc_screen = GetDC(None);
        if hdc_screen.0.is_null() {
            return Err("GetDC failed".into());
        }
        let dc = CreateCompatibleDC(Some(hdc_screen));
        ReleaseDC(None, hdc_screen);
        if dc.0.is_null() {
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
        let dib = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(d) => d,
            Err(_) => {
                let _ = DeleteDC(dc);
                return Err("CreateDIBSection failed".into());
            }
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(dc);
            return Err("CreateDIBSection null bits".into());
        }
        let old = SelectObject(dc, HGDIOBJ(dib.0));
        let ok = PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT).as_bool();
        let mut out = Vec::new();
        if ok {
            let src = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            out = src;
        }
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(dc);
        if !ok {
            let code = GetLastError().0;
            return Err(format!("PrintWindow false err={code}"));
        }
        // GDI 捕获的 alpha 不可靠:精确模式整窗不透明,统一置 255
        for px in out.as_chunks_mut::<4>().0 {
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
    // SAFETY: w 是 NUL 宽串；pidl 是栈上 [out] 槽位；成功时 SHParseDisplayName
    // 分配的 PIDL 所有权移交调用方（由 build_item_menu/free_item_pidls 以
    // CoTaskMemFree 配对释放）。
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
    // SAFETY: ctx 是有效 COM 对象；GetCommandString 的 pszName 按 GCS_VERBW
    // 契约写入宽字符（PSTR 视图 underlying 是 128 字节栈缓冲，cchMax=64
    // 以宽字符计不越界）；按 NUL 截断后解析。
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
/// 返回实际下发的 verb 字符串(取不到时 "#idx"),供调用方识别删除类动词。
fn invoke_command(hwnd: HWND, ctx: &IContextMenu, verb_idx: u32, x: i32, y: i32) -> String {
    // SAFETY(整块): ctx 是有效 COM 对象；info 全零+cbSize 按契约填充；
    // wverb 是 128 字节栈缓冲（GCS_VERBW 以宽字符写，cchMax=64 不越界）；
    // MAKEINTRESOURCEW 路径把 verb_idx 打包进指针值——shell 侧按整数解读、
    // 不解引用（官方文档的规范传递方式）；hwnd 是本进程窗口；info 生命周期
    // 覆盖同步的 InvokeCommand 调用。
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
        info.nShow = SW_SHOWNORMAL.0;
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
        let _ = SetForegroundWindow(crate::winids::menu_host_or(hwnd));
        let hr = ctx.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const _);
        let verb_desc = if has_str {
            String::from_utf16_lossy(&wverb[..wverb.iter().position(|c| *c == 0).unwrap_or(64)])
        } else {
            format!("#{}", verb_idx)
        };
        log(&format!("invoke verb '{}' -> hr={:?}", verb_desc, hr));
        verb_desc
    }
}

/// 构建与原生桌面一致的"项目"菜单源。优先走桌面 DefView 选中项路线(与
/// Explorer 右键桌面图标 100% 同源,含视图层"重命名");不可用时退回桌面文件
/// 夹 GetUIObjectOf(此时补注入"重命名"保持条目一致)。
fn build_item_menu(hwnd: HWND, paths: &[String]) -> Option<(IContextMenu, Vec<*mut ITEMIDLIST>)> {
    // 注:Explorer 的 WM_GETOBJECT 跨进程不回 IShellView,无法直接取 DefView
    // 选中项菜单;走桌面文件夹 GetUIObjectOf 路线 + 注入"重命名"对齐原生。
    // SAFETY(整块): PIDL 所有权链——pidl_from_path 分配的全部 PIDL 在每条
    // 失败出口逐一 CoTaskMemFree，成功路径移交返回值（调用方 free_item_pidls
    // 释放）；SHBindToParent 的 child 出参指向 PIDL 内部别名（不单独释放，
    // child0 同理）；GetUIObjectOf 返回的 IContextMenu 是 COM 包装，drop
    // 自动 Release；children 数组只被同步调用借用。
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
        let ctx: IContextMenu = match parent.GetUIObjectOf::<IContextMenu>(hwnd, &children, None) {
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
        // SAFETY: q 来自 pidl_from_path（SHParseDisplayName 的 COM 分配器
        // 分配），逐条各释放一次、移出所有权（消费 Vec）。
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
///
/// # Safety
/// 仅调用 Win32 窗口/线程 API；`hwnd` 无效时只是前台化失败（守卫照旧返回，
/// 由调用方决定后续），无内存安全前提。
pub unsafe fn menu_foreground(hwnd: HWND) -> MenuForegroundGuard {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    if !unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        let fg = unsafe { GetForegroundWindow() };
        if !fg.0.is_null() {
            let cur = unsafe { GetCurrentThreadId() };
            let fg_thread = unsafe { GetWindowThreadProcessId(fg, None) };
            if fg_thread != 0
                && fg_thread != cur
                && unsafe { AttachThreadInput(cur, fg_thread, true) }.as_bool()
            {
                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = AttachThreadInput(cur, fg_thread, false);
                }
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
        // SAFETY: self.hwnd 是构造守卫时记录的有效窗口；PostMessage 无指针
        // 参数（KB135788 菜单收尾标准做法）。
        unsafe {
            // 菜单关闭也算交互:2.5s 内推迟壁纸捕获,避开宿主未稳定态的
            // 强制重绘(±4% 亮度闪)
            crate::state::mark_interaction();
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// 弹出并等待菜单选择,返回命令 id(0=取消)。期间保留 COM 菜单对象,
/// WndProc 可转发动态/自绘子菜单消息(WM_INITMENUPOPUP 等)。
/// # Safety
/// 必须在 UI 线程调用（ACTIVE_CONTEXT_MENU 是 thread_local，TrackPopupMenu
/// 模态循环期间 wndproc 的 forward_menu_message 在同线程取它）；hwnd 是
/// 本进程窗口、menu 是调用方构建且尚未销毁的菜单、ctx 在整个调用期间存活；
/// 函数负责销毁 menu 并清空 thread_local 槽位。
unsafe fn run_item_menu(hwnd: HWND, ctx: &IContextMenu, menu: HMENU, x: i32, y: i32) -> u32 {
    ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
    // owner 用隐形菜单宿主,避免前台化栅栏窗口引发 z-band 往返的闪屏
    let host = crate::winids::menu_host_or(hwnd);
    let _guard = unsafe { menu_foreground(host) };
    let id = track_popup(menu, host, x, y);
    ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = None);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
    id
}

pub fn show_shell_context_menu_paths(hwnd: HWND, paths: &[String], x: i32, y: i32) {
    if paths.len() <= 1 {
        if let Some(path) = paths.first() {
            show_shell_context_menu(hwnd, path, x, y);
        }
        return;
    }
    // SAFETY(整块): 所有权链同 build_item_menu/run_item_menu/invoke_command
    // 各自的论证：PIDL 逐出口释放；菜单对象 DestroyMenu；COM 包装 drop 自动
    // Release；全部同步调用。
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
    // SAFETY(整块): 同 show_shell_context_menu_paths：PIDL/菜单/COM 所有权
    // 在各子函数内配对，本块只做同步编排。
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
        let item_count = GetMenuItemCount(Some(menu));
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
                let verb = invoke_command(hwnd, &ctx, verb_idx, x, y);
                // shell 动词在应用背后改动了桌面(典型 delete:文件已被 shell
                // 移入回收站,2026-09-09 用户实测 hr=Ok 但栅栏图标滞留不散):
                // 主动重扫让栅栏跟上;删除类再走"主动删除"标记,扫描宽恕当轮
                // 放行。其余动词经 rescan 的无变化早退,不会引发无谓重绘。
                if verb.eq_ignore_ascii_case("delete") {
                    crate::state::mark_scan_removed(&[path.to_string()]);
                }
                // 经托盘窗异步请求重扫(WM_DL3_RESCAN 由 UI 线程消息泵处理):
                // shell 不反向依赖 ui,消息一跳的延迟对"重扫跟上桌面"无感
                if let Some(tray) = crate::winids::TRAY_HWND.get().copied() {
                    let _ = PostMessageW(
                        Some(tray),
                        crate::winids::WM_DL3_RESCAN,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
        free_item_pidls(pidls);
    }
}

fn defview_ishellview() -> Option<IShellView> {
    use windows::Win32::UI::Accessibility::ObjectFromLresult;
    // SAFETY(整块): dv 是 Explorer 桌面 DefView 窗口（现查现用）；WM_GETOBJECT
    // 跨进程请求接口——iid 是栈上 GUID（调用期间存活）、res 是栈 [out]；
    // ObjectFromLresult 把返回的 LRESULT 转成已 AddRef 的接口指针 psv，
    // transmute 成 COM 包装即接管该引用（drop 自动 Release）。
    unsafe {
        let dv = match find_defview_window() {
            Some(d) => d,
            None => {
                log("dv isv: no defview window");
                return None;
            }
        };
        const WM_GETOBJECT: u32 = 0x003D;
        let iid = <IShellView as Interface>::IID;
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
        if let Err(e) = ObjectFromLresult(LRESULT(res as isize), &iid, WPARAM(0), &mut psv) {
            log(&format!("dv isv: ObjectFromLresult failed: {e}"));
            return None;
        }
        let psv: IShellView = std::mem::transmute(psv);
        Some(psv)
    }
}

fn defview_background_menu() -> Option<IContextMenu> {
    use windows::Win32::UI::Shell::SVGIO_BACKGROUND;
    // SAFETY: psv 是 defview_ishellview 返回的存活 COM 包装；
    // GetItemObject 返回的 IContextMenu 同为 COM 包装（drop 自动 Release）。
    unsafe {
        let psv = defview_ishellview()?;
        psv.GetItemObject::<IContextMenu>(SVGIO_BACKGROUND).ok()
    }
}

/// 查找桌面 SHELLDLL_DefView 窗口(Progman 直属,或 WorkerW 下)。
fn find_defview_window() -> Option<HWND> {
    // SAFETY: 三个类名均为 NUL 宽串（同步查找期间存活）；EnumWindows 的
    // lparam 承载栈槽位 slot 的指针——EnumWindows 同步枚举，回调在调用
    // 返回前全部执行完毕，slot 生命周期覆盖。
    unsafe {
        let progman = wide("Progman");
        let defview = wide("SHELLDLL_DefView");
        let workerw = wide("WorkerW");
        let cur = FindWindowExW(
            FindWindowW(PCWSTR::from_raw(progman.as_ptr()), None).ok(),
            None,
            PCWSTR::from_raw(defview.as_ptr()),
            None,
        )
        .unwrap_or_default();
        if !cur.0.is_null() {
            return Some(cur);
        }
        let mut host = FindWindowW(PCWSTR::from_raw(workerw.as_ptr()), None).unwrap_or_default();
        while !host.0.is_null() {
            let dv = FindWindowExW(Some(host), None, PCWSTR::from_raw(defview.as_ptr()), None)
                .unwrap_or_default();
            if !dv.0.is_null() {
                return Some(dv);
            }
            host = FindWindowExW(None, Some(host), PCWSTR::from_raw(workerw.as_ptr()), None)
                .unwrap_or_default();
        }
        let mut slot: Option<HWND> = None;
        let _ = EnumWindows(
            Some(enum_find_defview),
            LPARAM(&mut slot as *mut Option<HWND> as isize),
        );
        slot
    }
}

/// # Safety
/// EnumWindows 的回调契约：lparam 是 EnumWindows 调用方透传的原值——
/// 即 find_defview_window 栈槽位 Option<HWND> 的指针，EnumWindows 同步
/// 执行期间有效；本回调只读写该槽位，返回 FALSE 终止枚举。
unsafe extern "system" fn enum_find_defview(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let slot: &mut Option<HWND> = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
    if slot.is_some() {
        return BOOL(0);
    }
    // 类名宽串只编码一次(静态),枚举每个顶层窗的回调不再重复分配
    // (2026-09-17 优化;字符串仅作 FindWindowExW 的只读比较入参)
    static DEFVIEW: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    let defview = DEFVIEW.get_or_init(|| wide("SHELLDLL_DefView"));
    let dv = unsafe {
        FindWindowExW(Some(hwnd), None, PCWSTR::from_raw(defview.as_ptr()), None)
            .unwrap_or_default()
    };
    if !dv.0.is_null() {
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
    // SAFETY(整块): 菜单构建/弹出/命令执行的契约同前述各函数
    // （append_menu/track_popup/invoke_command/run_item_menu）；
    // thread_local 槽位在 track_popup 前后成对置位/清空。
    unsafe {
        let ctx: IContextMenu = match defview_background_menu() {
            Some(c) => c,
            None => {
                let psf = match SHGetDesktopFolder() {
                    Ok(f) => f,
                    Err(_) => return 0,
                };
                match psf.GetUIObjectOf::<IContextMenu>(hwnd, &[], None) {
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
        append_menu(sub, DL_CMD_ADD_FENCE, crate::lang::new_fence());
        append_menu(sub, DL_CMD_SHOW_ALL, crate::lang::tray_show_all());
        append_menu(sub, DL_CMD_HIDE_ALL, crate::lang::tray_hide_all());
        append_menu(sub, DL_CMD_UNDO, crate::lang::undo_layout());
        append_menu(sub, DL_CMD_REFRESH, crate::lang::refresh());
        // 三档对齐:点击在 自动→网格→自由 间循环(完整设置在托盘菜单)
        let mode_label = match align_mode {
            "grid" => crate::lang::desktop_align_grid(),
            "free" => crate::lang::desktop_align_free(),
            _ => crate::lang::desktop_align_auto(),
        };
        append_menu(sub, DL_CMD_AUTO_ALIGN, mode_label);
        // 渲染模式:精确(默认,壁纸底+ClearType,与原生一致) ↔ 透明(兜底,动态壁纸不兼容时)
        let render_label = match render_mode {
            "precise" => crate::lang::desktop_render_precise(),
            _ => crate::lang::desktop_render_transparent(),
        };
        append_menu(sub, DL_CMD_RENDER_MODE, render_label);
        append_separator(sub);
        append_menu(sub, DL_CMD_QUIT, crate::lang::quit_deskfence());
        let label = wide("DeskFence");
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            sub.0 as usize,
            PCWSTR::from_raw(label.as_ptr()),
        );
        ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
        let host = crate::winids::menu_host_or(hwnd);
        let _ = SetForegroundWindow(host);
        let id = track_popup(menu, host, x, y);
        ACTIVE_CONTEXT_MENU.with(|slot| *slot.borrow_mut() = None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
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
    // SAFETY: 无参创建菜单，失败得 null 句柄由 unwrap_or_default 吸收
    //（后续菜单调用对空句柄失败无害）。
    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    append_menu(menu, F_OPEN, crate::lang::open());
    append_menu(menu, F_OPENWITH, crate::lang::open_with());
    append_separator(menu);
    append_menu(menu, F_COPY, crate::lang::copy());
    append_separator(menu);
    append_menu(menu, F_LOCATE, crate::lang::open_location());
    append_menu(menu, F_RENAME, crate::lang::rename());
    append_menu(menu, F_DELETE, crate::lang::delete());
    append_separator(menu);
    append_menu(menu, F_PROPERTIES, crate::lang::properties());
    let host = crate::winids::menu_host_or(hwnd);
    let id = track_popup(menu, host, x, y);
    // SAFETY: menu 是本函数创建、track_popup 已返回（模态结束）的菜单，
    // 此时销毁安全。
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

/// 路径在磁盘上已确认不存在(删除/移走):GetFileAttributesW 返回 INVALID。
/// 与"read_dir 瞬态漏读"互补——文件仍在盘上时属性查询依然成功,那才是
/// 扫描宽恕要保护的情形。
pub fn path_gone_from_disk(path: &str) -> bool {
    // windows crate 未导出 FILE_ATTRIBUTE_INVALID(0.62 仍缺),失败值即 u32::MAX
    const FILE_ATTR_INVALID: u32 = u32::MAX;
    let w = wide(path);
    // SAFETY: w 是 NUL 宽串；GetFileAttributesW 只读返回属性值，无输出指针。
    unsafe { GetFileAttributesW(PCWSTR::from_raw(w.as_ptr())) == FILE_ATTR_INVALID }
}

/// "打开方式…"对话框
pub fn open_with(path: &str) {
    let w = wide(path);
    let op = wide("openas");
    // SAFETY: 同 open_recycle_bin：NUL 宽串在同步调用期间存活。
    unsafe {
        let _ = ShellExecuteW(
            None,
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

/// SHFileOperation 的双 NUL 结尾宽字符路径串(2026-09-16 提 pub 供 tests/)
pub fn multistring(paths: &[String]) -> Vec<u16> {
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
    // SAFETY: op 全零+关键字段填充；pFrom/pTo 均为双 NUL 结尾缓冲
    //（multistring 契约/手动补 NUL），SHFileOperationW 同步调用期间存活。
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = FO_COPY;
    op.pFrom = PCWSTR::from_raw(from.as_ptr());
    op.pTo = PCWSTR::from_raw(to.as_ptr());
    op.fFlags = (FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION).0 as u16;
    // SAFETY: 同上。
    unsafe { SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool() }
}

pub fn delete_to_recycle_bin_many(hwnd: HWND, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let from = multistring(paths);
    // SAFETY: op 全零+关键字段填充；pFrom 双 NUL 结尾（multistring 契约），
    // 同步调用期间存活；返回码忽略（错误走 rescan 收敛）。
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = FO_DELETE;
    op.pFrom = PCWSTR::from_raw(from.as_ptr());
    op.fFlags = FOF_ALLOWUNDO.0 as u16;
    // SAFETY: 同上。
    unsafe {
        let _ = SHFileOperationW(&mut op);
    }
}

/// 显示文件属性对话框
pub fn show_properties(path: &str) {
    let w = wide(path);
    let op = wide("properties");
    // SAFETY: 同 open_path：全零+cbSize 的结构、NUL 宽串、同步调用。
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.lpVerb = PCWSTR::from_raw(op.as_ptr());
        sei.lpFile = PCWSTR::from_raw(w.as_ptr());
        sei.nShow = SW_SHOWNORMAL.0;
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
    // SAFETY: 同 desktop_icon_size 的 RegGetValueW 契约（NUL 宽串+栈输出）。
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
    // SAFETY: 同 desktop_icon_size 的 RegGetValueW 契约（NUL 宽串+栈输出）。
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
/// 计划任务名(2026-09-16 起自启首选计划任务):任务计划程序服务的登录
/// 触发不经 Explorer 的 Run 键排队,冷开机可比 Run 键早数十秒拉起进程
/// (2026-09-16 实测本机 Run 键路径 Explorer 出桌面后 86s 才轮到)。
/// 创建失败(如组策略禁用 schtasks)自动回退 HKCU Run 键,旧路径原样保留。
const TASK_NAME: &str = "DeskFence";

/// 跑一条 schtasks 子命令:隐藏窗口、10s 超时,返回退出码是否为 0
fn run_schtasks(args: &[&str]) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_string());
    let line = format!("\"{}\\System32\\schtasks.exe\" {}", windir, args.join(" "));
    let mut wide_line = wide(&line);
    // SAFETY(整块): lpCommandLine 必须指向**可写**缓冲（CreateProcessW 契约，
    // wide_line.as_mut_ptr() 满足）；si/pi 为栈结构、pi 是 [out] 槽位；
    // 成功后 hProcess/hThread 各 CloseHandle 一次（含失败退出码路径）。
    unsafe {
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        if CreateProcessW(
            None,
            Some(windows::core::PWSTR::from_raw(wide_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &si,
            &mut pi,
        )
        .is_err()
        {
            return false;
        }
        let _ = WaitForSingleObject(pi.hProcess, 10_000);
        let mut code: u32 = 1;
        let _ = GetExitCodeProcess(pi.hProcess, &mut code);
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
        code == 0
    }
}

/// 计划任务是否已注册(/Query 退出码 0;其他失败一律按"无任务"回退)
fn scheduled_task_exists() -> bool {
    run_schtasks(&["/Query", "/TN", TASK_NAME])
}

/// schtasks 注册失败的机器标记(组策略禁用任务计划并不罕见,2026-09-16 本机
/// 实测 schtasks/Register-ScheduledTask 均"拒绝访问"):迁移只尝试一次,
/// 失败即写此标记,避免每次开机白跑 schtasks 子进程拖慢启动;用户显式
/// 关/开自启或任务创建成功时清掉。
fn task_fail_marker() -> std::path::PathBuf {
    crate::model::config_dir().join("autostart_task_unavailable")
}

/// 删除 HKCU Run 键里的自启值(任务路径成功后清掉,避免双启动)
fn delete_run_autostart() {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let legacy = wide(RUN_VALUE_LEGACY);
    // SAFETY: 注册表键句柄配对：RegOpenKeyExW 成功后 RegCloseKey 一次；
    // NUL 宽串参数；删除不存在的值返回错误被忽略。
    unsafe {
        let mut lhkey: HKEY = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut lhkey,
        )
        .0 == 0
            && !lhkey.0.is_null()
        {
            let _ = RegDeleteValueW(lhkey, PCWSTR::from_raw(legacy.as_ptr()));
            let _ = RegDeleteValueW(lhkey, PCWSTR::from_raw(name.as_ptr()));
            let _ = RegCloseKey(lhkey);
        }
    }
}

/// get_autostart 结果缓存(0=未知,1=开,2=关):schtasks /Query 每次要拉起
/// 子进程(冷盘+杀软下可到几百 ms),托盘菜单每次现建都查会卡顿。进程内
/// 缓存一份,set_autostart 成功后同步更新;外部手动改任务/注册表,重启
/// 后可见。
static AUTOSTART_CACHE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// HKCU Run 键里是否还有自启值(旧路径,迁移判断用)
fn run_key_autostart_exists() -> bool {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut sz: u32 = 0;
    // SAFETY: 查存在性：pvData 传 None（只探测不取值），sz 出参可为栈值；
    // NUL 宽串同前。
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

pub fn get_autostart() -> bool {
    match AUTOSTART_CACHE.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    // 先查注册表(零开销),没有再查计划任务(拉子进程)
    let on = run_key_autostart_exists() || scheduled_task_exists();
    AUTOSTART_CACHE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
    on
}

/// 一次性迁移(2026-09-16):升级前用 Run 键自启的老用户,检测到 Run 键值
/// 存在而计划任务未注册时,走一次 set_autostart(true)(建任务+清 Run 键)。
/// schtasks 不可用的机器上 set_autostart 会回退回写 Run 键,无副作用;
/// 迁移完成后启动路径只剩一次注册表读。
pub fn migrate_autostart_to_task() {
    if task_fail_marker().exists() || !run_key_autostart_exists() {
        return;
    }
    if scheduled_task_exists() {
        // 任务已存在(如外部建过):清掉 Run 键值避免双启动即可
        delete_run_autostart();
        return;
    }
    log("autostart: migrating run key to scheduled task");
    set_autostart(true);
}

pub fn set_autostart(on: bool) -> bool {
    let ok = set_autostart_impl(on);
    if ok {
        AUTOSTART_CACHE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
    }
    ok
}

fn set_autostart_impl(on: bool) -> bool {
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
    // SAFETY(整块): 注册表句柄全部开/关配对；RegSetValueExW 的数据指针是
    // vw 的字节视图（from_raw_parts 长度=元素数*2，NUL 宽串含终止符一起写）；
    // schtasks 走子进程（见 run_schtasks 论证）。
    unsafe {
        // 无论开/关,都先清掉旧版 DeskLens3 的自启项,避免新旧并存重复启动
        let mut lhkey: HKEY = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut lhkey,
        )
        .0 == 0
            && !lhkey.0.is_null()
        {
            let _ = RegDeleteValueW(lhkey, PCWSTR::from_raw(legacy.as_ptr()));
            let _ = RegCloseKey(lhkey);
        }
        if on {
            // 首选计划任务:登录即触发,注册后 /Query 复核;成功则清掉 Run 键值
            // 避免双启动
            let tr = format!("\"{}\"", exe);
            let created = run_schtasks(&[
                "/Create", "/F", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/RL", "LIMITED",
            ]);
            if created && scheduled_task_exists() {
                delete_run_autostart();
                let _ = std::fs::remove_file(task_fail_marker());
                log("autostart: scheduled task registered, run key removed");
                return true;
            }
            let _ = std::fs::write(task_fail_marker(), b"1");
            log("autostart: scheduled task unavailable, fallback to run key");
            let mut hkey: HKEY = HKEY::default();
            let ok = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(key.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
            .0 == 0;
            if !ok {
                return false;
            }
            let set = RegSetValueExW(
                hkey,
                PCWSTR::from_raw(name.as_ptr()),
                None,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    vw.as_ptr() as *const u8,
                    vw.len() * 2,
                )),
            )
            .0 == 0;
            let _ = RegCloseKey(hkey);
            set
        } else {
            // 关闭:计划任务与 Run 键值都清,不存在则忽略;失败标记一并清,
            // 下次开启时重新优先尝试任务路径
            let _ = run_schtasks(&["/Delete", "/F", "/TN", TASK_NAME]);
            delete_run_autostart();
            let _ = std::fs::remove_file(task_fail_marker());
            true
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
    // SAFETY(整块): 专属监听线程内：CreateFileW 的句柄由 CloseHandle 收尾
    //（含 break 路径）；ReadDirectoryChangesW 同步（无 OVERLAPPED）使用
    // buf——buf 归线程闭包所有、在循环期间不被移动；ret 是栈 [out]；
    // 目录句柄打开失败直接 return（无泄漏）。
    std::thread::spawn(move || unsafe {
        let handle = CreateFileW(
            PCWSTR::from_raw(wdir.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
            None,
        );
        // 打不开目录=桌面变更即时刷新整段静默失效(只剩 30s 轮询兜底),
        // 用户感知"新增文件半天不出现"却无迹可查——留一行现场。
        let handle = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                log(&format!(
                    "desktop dir watcher open failed: {}",
                    dir.display()
                ));
                return;
            }
        };
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
                false,
                filter,
                Some(&mut ret),
                None,
                None,
            );
            if ok.is_err() {
                // 监视线程异常终止:此后桌面变更静默退回轮询兜底,记一行
                // 才能解释"刷新怎么突然变慢了"。
                log("desktop dir watcher stopped (read error)");
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
    // HWND 在 windows 0.62 起不再实现 Send;按指针位捕获,线程内重建,语义不变
    let notify_hwnd_bits = notify_hwnd.0 as usize;
    let wdir = wide(&dir.to_string_lossy());
    // SAFETY(整块): 同 start_directory_watcher 的句柄/缓冲论证；HWND 按
    // 位在线程内重建（跨线程只传数值，PostMessage 是窗口跨线程的唯一
    // 合法触碰方式，托盘窗口由本进程 UI 线程持有）。
    std::thread::spawn(move || unsafe {
        let notify_hwnd = HWND(notify_hwnd_bits as *mut std::ffi::c_void);
        let handle = CreateFileW(
            PCWSTR::from_raw(wdir.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
            None,
        );
        // 打不开主题目录=壁纸轮换的毫秒级信号源静默失效(只剩
        // WM_SETTINGCHANGE+10min 兜底轮询),幻灯片壁纸切换明显滞后却
        // 无迹可查——留一行现场。
        let handle = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                log(&format!(
                    "wallpaper dir watcher open failed: {}",
                    dir.display()
                ));
                return;
            }
        };
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
                true, // 递归:CachedImageFiles 也要覆盖
                filter,
                Some(&mut ret),
                None,
                None,
            );
            if ok.is_err() {
                break;
            }
            let _ = PostMessageW(Some(notify_hwnd), notify_msg, WPARAM(0), LPARAM(0));
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
/// (2026-09-16 提 pub:tests/ 有 GUID 字符串比对回归测试)
pub const CLSID_DESKTOP_WALLPAPER: windows::core::GUID =
    windows::core::GUID::from_u128(0xc2cf3110_0460_4fc1_b9d0_8a1c0c9cc4bd);

pub fn wallpaper_signature() -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::UI::Shell::IDesktopWallpaper;
    // SAFETY(整块): CoCreateInstance 产出的 COM 包装 drop 自动 Release；
    // GetMonitorDevicePathAt/GetWallpaper 返回的 PWSTR 由 CoTaskMemAlloc
    // 分配、所有权移交调用方——每条各 CoTaskMemFree 一次（dev/wp 分开配对）；
    // pwstr_to_string 只读不解引用越界（见其 Safety 段）。
    unsafe {
        // 注:部分受管控/定制系统(实测存在)该 coclass 未注册(REGDB_E_CLASSNOTREG),
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
        let pos = dp
            .GetPosition()
            .map(|p| p.0.to_string())
            .unwrap_or_default();
        Some(format!("{bg}|{pos}|{}", parts.join("\u{1}")))
    }
}

/// # Safety
/// `p` 必须指向以 NUL 结尾、可读的宽字符串（shell API 的 CoTaskMem 分配
/// 约定）；本函数只读取（含 NUL 扫描），不释放、不拥有——释放由调用方
/// 的 CoTaskMemFree 完成。
unsafe fn pwstr_to_string(p: windows::core::PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while unsafe { *p.0.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(p.0, len) }).ok()
}

// ---------------- 进程辅助(环境体检/自愈) ----------------

/// 按进程名枚举 pid(不含自身)。用于环境体检(发现多余 DeskFence 实例)
/// 与"修复桌面环境"(重启 Explorer 重建桌面层)。
pub fn pids_by_name(name: &str) -> Vec<u32> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    let me = std::process::id();
    // SAFETY: 快照句柄 CloseHandle 收尾；PROCESSENTRY32W 契约：dwSize 必须
    // 先填结构大小再迭代；szExeFile 定长数组按 NUL 截断。
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snap, &mut e).is_ok() {
                loop {
                    let end = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                    let pname = String::from_utf16_lossy(&e.szExeFile[..end]);
                    if pname.eq_ignore_ascii_case(name) && e.th32ProcessID != me {
                        out.push(e.th32ProcessID);
                    }
                    if Process32NextW(snap, &mut e).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(HANDLE(snap.0));
        }
    }
    out
}

/// 按名终止进程并等待退出(最多 wait_ms)。返回未退出的残余 pid。
pub fn terminate_by_name(name: &str, wait_ms: u64) -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let start = std::time::Instant::now();
    loop {
        let alive = pids_by_name(name);
        if alive.is_empty() {
            return Vec::new();
        }
        if start.elapsed().as_millis() as u64 > wait_ms {
            return alive;
        }
        for pid in &alive {
            // SAFETY: OpenProcess 成功返回的句柄各 CloseHandle 一次；
            // 失败（进程已退出/权限不足）跳过即可，循环重试兜底。
            unsafe {
                if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, *pid) {
                    let _ = TerminateProcess(h, 1);
                    let _ = CloseHandle(h);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// 启动 explorer.exe(修复桌面环境用;GUI 子进程,无控制台闪窗)。
pub fn start_explorer() {
    let _ = std::process::Command::new("explorer.exe").spawn();
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
