//! 窗口身份标识（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
//! 窗口类名（栅栏/托盘/参考线/菜单宿主）、模块句柄、应用图标、
//! 托盘与菜单宿主的 HWND 单例、TaskbarCreated 注册消息。
//! 叶子模块——只依赖 std / windows crate / crate::shell，不依赖 crate::ui。

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, RegisterWindowMessageW, HICON};

use crate::shell;

pub(crate) fn class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceFence\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

pub(crate) fn tray_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceTray\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

pub(crate) fn guide_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceGuide\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

/// 菜单前台宿主专用类:历史上复用栅栏类,外部探针与自家 drag_elevate_anchor
/// 的兄弟栅栏扫描都会把它误当真栅栏(2026-08-28 wdprobe 实测数出 6 个"栅栏")。
pub(crate) fn menu_host_class_name() -> PCWSTR {
    static W: OnceLock<Vec<u16>> = OnceLock::new();
    let v = W.get_or_init(|| "DeskFenceMenuHost\0".encode_utf16().collect());
    PCWSTR::from_raw(v.as_ptr())
}

pub(crate) fn hinstance() -> HINSTANCE {
    // SAFETY: GetModuleHandleW(None) 取本进程模块句柄（不增加引用计数、
    // 无需释放），失败得 null（后续窗口创建失败可观察）。
    unsafe {
        HINSTANCE(
            windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap_or_default()
                .0,
        )
    }
}

pub(crate) fn deskfence_icon() -> HICON {
    // PCWSTR(1) = MakeIntResourceW(1),即 DeskFence.rc 里 ID=1 的图标资源。
    // 不能按 clippy 建议换成 ptr::dangling()(地址=对齐值 2,会查错资源)
    // SAFETY: 指针值 1 是资源 id 的规范打包方式（LoadIconW 按整数解读、
    // 不解引用）；共享图标句柄由系统持有，无需销毁。
    #[allow(clippy::manual_dangling_ptr)]
    unsafe {
        LoadIconW(Some(hinstance()), PCWSTR(1usize as *const u16)).unwrap_or_default()
    }
}

/// windows 0.62 起句柄类型(HWND/HHOOK 等)不再实现 Send/Sync(0.52 曾为全部
/// 句柄无条件提供)。本应用的窗口/钩子句柄只创建与使用于 UI 线程,静态量仅供
/// 后台线程读取判空或投递消息——指针本身的跨线程可见性与 0.52 时代语义相同,
/// 在此对少数持有句柄的静态容器恢复之(Deref 透传,调用点零改动)。
pub(crate) struct SyncHandle<H>(pub H);
// SAFETY(Send/Sync): H 为句柄类型（纯指针位，无线程亲和的 Rust 语义）。
// 约定见上：句柄的创建与使用只在 UI 线程；跨线程只发生两件事——读取
// OnceLock 里的句柄值做判空、或以 PostMessageW 异步投递（窗口系统侧串行
// 处理），两者都只把句柄当数值用，不构成数据竞争。
unsafe impl<H> Send for SyncHandle<H> {}
// SAFETY(Sync): 同上；&SyncHandle<H> 跨线程共享也只是共享句柄数值。
unsafe impl<H> Sync for SyncHandle<H> {}
impl<H> core::ops::Deref for SyncHandle<H> {
    type Target = H;
    fn deref(&self) -> &H {
        &self.0
    }
}

pub(crate) static TRAY_HWND: SyncHandle<OnceLock<HWND>> = SyncHandle(OnceLock::new());
/// 菜单前台宿主窗口(1x1 隐形):菜单前台化的目标,避免提升栅栏窗口 z 序
pub(crate) static MENU_HOST_HWND: SyncHandle<OnceLock<HWND>> = SyncHandle(OnceLock::new());

/// 菜单 owner 用的前台宿主;尚未创建时回退到调用方窗口
pub fn menu_host_or(fallback: HWND) -> HWND {
    MENU_HOST_HWND.get().copied().unwrap_or(fallback)
}

static TASKBAR_CREATED_MSG: OnceLock<u32> = OnceLock::new();

/// shell 动词改桌面后请求重扫(投给托盘窗,由 UI 线程消息泵处理;
/// 定义在叶子层让 shell 零上行依赖 ui)
pub(crate) const WM_DL3_RESCAN: u32 = 0x8000 + 9; // WM_APP+9
/// 合并后的高速 z 自检请求(WinEvent 回调投递;selfheal 产生、ui 托盘消费,
/// 定义在叶子层让 selfheal 不经 ui 取用)
pub(crate) const WM_DL3_ZCHECK: u32 = 0x8000 + 7; // WM_APP+7
/// EDIT 控件消息(windows crate 0.62 未导出,手写;就地改名/分类面板两处
/// 编辑框共用,收在叶子层避免各模块重复定义)
pub(crate) const EM_SETSEL: u32 = 0x00B1;

pub(crate) fn taskbar_created_msg() -> u32 {
    // SAFETY: name 是 NUL 宽串（同步调用期间存活）；RegisterWindowMessageW
    // 同名重复注册返回同一 id，OnceLock 保证只调一次。
    *TASKBAR_CREATED_MSG.get_or_init(|| unsafe {
        let name = shell::wide("TaskbarCreated");
        RegisterWindowMessageW(PCWSTR::from_raw(name.as_ptr()))
    })
}
