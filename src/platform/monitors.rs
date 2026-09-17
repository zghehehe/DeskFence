//! 显示器工作区几何（2026-09-16 从 ui.rs 原样搬出，纯搬家不改行为）：
//! 主屏工作区 / 矩形所在屏工作区 / 全部显示器工作区枚举。
//! 叶子模块——只依赖 std / windows crate / crate::model，不依赖 crate::ui。

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromRect, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::Rect;

/// 屏幕工作区（不含任务栏）
pub(crate) fn work_area() -> (f32, f32, f32, f32) {
    // SAFETY: 全零 RECT 合法；SPI_GETWORKAREA 契约——pvParam 指向 RECT；
    // r 是栈变量，调用期间有效。
    let mut r: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: 同上。
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    (
        r.left as f32,
        r.top as f32,
        (r.right - r.left) as f32,
        (r.bottom - r.top) as f32,
    )
}

/// 矩形所在显示器的工作区(多显示器:磁吸/含屏按各自屏幕进行)
pub(crate) fn work_area_for_rect(r: &Rect) -> (f32, f32, f32, f32) {
    // SAFETY: rc/mi 为栈结构，MONITORINFO 按契约先填 cbSize；
    // MonitorFromRect/GetMonitorInfoW 均为同步查询。
    unsafe {
        let rc = RECT {
            left: r.x.round() as i32,
            top: r.y.round() as i32,
            right: (r.x + r.w).round() as i32,
            bottom: (r.y + r.h).round() as i32,
        };
        let mon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let w = mi.rcWork;
            return (
                w.left as f32,
                w.top as f32,
                (w.right - w.left) as f32,
                (w.bottom - w.top) as f32,
            );
        }
    }
    work_area()
}

/// # Safety
/// EnumDisplayMonitors 的回调契约：lparam 是调用方透传的原值（
/// all_work_areas 的栈 Vec 指针），枚举同步执行、回调在返回前完成；
/// 本回调只追加工作区数据（mi 为按 cbSize 契约填充的栈缓冲）。
unsafe extern "system" fn enum_monitor_cb(
    mon: HMONITOR,
    _dc: HDC,
    _rc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let areas = unsafe { &mut *(lparam.0 as *mut Vec<(f32, f32, f32, f32)>) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        let w = mi.rcWork;
        areas.push((
            w.left as f32,
            w.top as f32,
            (w.right - w.left) as f32,
            (w.bottom - w.top) as f32,
        ));
    }
    BOOL(1)
}

/// 所有显示器的工作区(settle/含屏用)
pub(crate) fn all_work_areas() -> Vec<(f32, f32, f32, f32)> {
    let mut areas: Vec<(f32, f32, f32, f32)> = Vec::new();
    // SAFETY: areas 是栈 Vec，其指针经 lparam 透传给回调（枚举同步执行，
    // 回调在返回前完成）；枚举失败/为空由 work_area 兜底。
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut areas as *mut Vec<(f32, f32, f32, f32)> as isize),
        );
    }
    if areas.is_empty() {
        areas.push(work_area());
    }
    areas
}

/// 当前鼠标屏幕坐标（拖动位移必须用屏幕坐标，
/// 因为窗口移动后 WM_MOUSEMOVE 的客户区坐标会随之变化，造成抖动/拖不动）。
/// 失败保持 (0,0)，调用方按无效坐标处理。
pub(crate) fn screen_cursor() -> (f32, f32) {
    // SAFETY: pt 是栈上输出指针，调用期间有效。
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        (pt.x as f32, pt.y as f32)
    }
}
