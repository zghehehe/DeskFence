#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cats_panel;
mod drag;
mod iconcache;
mod menu;
mod model;
mod ole;
mod render;
mod selfheal;
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
    // 清洁启动序列(用户约定的默认行为,2026-08-29):任何时候起新实例,
    // 都先停掉旧实例、确认环境就绪,没问题了才启动新服务——杜绝双实例
    // 互殴/僵尸窗口/脏桌面层把运行环境弄乱。
    ensure_clean_startup();
    if !ui::init() {
        println!("failed to init renderer/COM");
        std::process::exit(1);
    }
    ui::startup();
    let _ = ui::run_message_loop();
}

/// 清洁启动:①替换旧实例(停止所有 deskfence.exe 并等待退出);
/// ②桌面宿主就绪门槛(Explorer 桌面层未就绪时最多等 15s——登录早期/
/// Explorer 重启中,超时放行交由既有自愈在就绪后补挂,绝不永久阻塞)。
fn ensure_clean_startup() {
    // ① 新实例替换旧实例:双击旧版 exe 不会"唤醒旧进程拒绝新版",
    // 更新/重测直接启动即可;旧实例非正常退出由 boot 的崩溃恢复兜底。
    let stale = shell::pids_by_name("deskfence.exe");
    if !stale.is_empty() {
        let remain = shell::terminate_by_name("deskfence.exe", 5000);
        ui::log(&format!(
            "clean-start: replaced {} previous instance(s){}",
            stale.len(),
            if remain.is_empty() {
                String::new()
            } else {
                format!(", {} resisted: {:?}", remain.len(), remain)
            }
        ));
    }
    // ② 宿主就绪门槛
    if ui::desktop_host_ready() {
        return;
    }
    ui::log("clean-start: desktop host not ready, waiting (max 15s)");
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if ui::desktop_host_ready() {
            ui::log("clean-start: desktop host became ready");
            return;
        }
    }
    ui::log("clean-start: host still absent after 15s, booting with deferred attach");
}
