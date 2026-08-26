#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod ole;
mod render;
mod shell;
mod ui;

fn main() {
    // windows 子系统下 panic 默认静默,任何 UI 线程 panic 都会导致窗口全部消失而进程残留。
    // 落盘 panic 信息便于事后诊断。
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}");
        crate::ui::log(&msg);
    }));
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--restore-desktop") {
        // 恢复模式:跳过渲染器/托盘/配置加载,只还原 Explorer 原生桌面图标列表
        // 并清掉本工具的标记。桌面图标异常时用 `deskfence.exe --restore-desktop` 自救。
        let ok = ui::restore_desktop_now();
        println!(
            "restore desktop: {}",
            if ok { "ok" } else { "desktop list unavailable" }
        );
        std::process::exit(if ok { 0 } else { 1 });
    }
    if let Some(pos) = args.iter().position(|a| a == "--icondump") {
        // 诊断:导出各图标提取路径的像素,供与原生桌面截图对比(--icondump <file> <out-prefix>)
        let path = args.get(pos + 1).cloned().unwrap_or_default();
        let prefix = args.get(pos + 2).cloned().unwrap_or_default();
        if path.is_empty() || prefix.is_empty() {
            println!("usage: deskfence.exe --icondump <file> <out-prefix>");
            std::process::exit(2);
        }
        shell::icon_dump(&path, &prefix);
        return;
    }
    // 单实例:二次双击只唤醒已有实例(显示全部栅栏),不启动第二个进程
    if !acquire_single_instance() {
        ui::notify_second_instance();
        return;
    }
    if !ui::init() {
        println!("failed to init renderer/COM");
        std::process::exit(1);
    }
    ui::startup();
    let _ = ui::run_message_loop();
}

/// 命名互斥体保证单实例;返回 true = 本进程是唯一实例
fn acquire_single_instance() -> bool {
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    static HANDLE_SLOT: OnceLock<HANDLE> = OnceLock::new();
    unsafe {
        let name: Vec<u16> = "Local\\DeskFence.SingleInstance"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        match CreateMutexW(None, false, PCWSTR::from_raw(name.as_ptr())) {
            Ok(h) => match GetLastError() {
                // 新式错误模型:GetLastError() 返回 Result<()>
                Ok(()) => {
                    let _ = HANDLE_SLOT.set(h);
                    true
                }
                Err(e) if e.code().0 as u32 == ERROR_ALREADY_EXISTS.0 => false,
                Err(_) => true,
            },
            Err(_) => true, // 拿不到互斥体也不阻塞启动
        }
    }
}
