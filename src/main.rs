#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cats_panel;
mod drag;
mod firstrun;
mod iconcache;
mod lang;
mod menu;
mod model;
mod ole;
mod rename;
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
        // 必须先清场:活着的旧实例每秒 reconcile 会把恢复的原生图标重新藏回,
        // 不杀旧实例的自救等于白做(2026-09-11)。
        clear_previous_instances();
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

/// 清洁启动:①替换旧实例(先礼后兵清场,见 clear_previous_instances);
/// ②桌面宿主就绪门槛(Explorer 桌面层未就绪时最多等 15s——登录早期/
/// Explorer 重启中,超时放行交由既有自愈在就绪后补挂,绝不永久阻塞)。
fn ensure_clean_startup() {
    // ① 新实例替换旧实例:双击旧版 exe 不会"唤醒旧进程拒绝新版",
    // 更新/重测直接启动即可;旧实例非正常退出由 boot 的崩溃恢复兜底。
    clear_previous_instances();
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

/// 清场:让旧实例退出,先礼后兵——先给托盘窗发 WM_CLOSE 走 quit_app 的
/// 完整清理(恢复原生图标/存壁纸快照/卸钩子/删托盘),健康实例毫秒级自退;
/// 收不到消息的卡死实例(UI 线程死循环)等 2s 后由强杀兜底。不判健康与否,
/// 只有先后顺序:任何一次启动都收敛到"唯一且健康"的新实例——双击 exe
/// 即一键自救(2026-09-11 与用户约定)。
fn clear_previous_instances() {
    let stale = shell::pids_by_name("deskfence.exe");
    if stale.is_empty() {
        return;
    }
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
    // 本函数在任何窗口创建之前执行,FindWindowW 命中的必是旧实例托盘窗
    unsafe {
        let tray = FindWindowW(ui::tray_class_name(), PCWSTR::null());
        if tray.0 != 0 {
            let _ = PostMessageW(tray, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if shell::pids_by_name("deskfence.exe").is_empty() {
            ui::log(&format!(
                "clean-start: {} previous instance(s) exited gracefully",
                stale.len()
            ));
            return;
        }
    }
    let remain = shell::terminate_by_name("deskfence.exe", 5000);
    ui::log(&format!(
        "clean-start: replaced {} previous instance(s) (graceful close timed out){}",
        stale.len(),
        if remain.is_empty() {
            ", terminated".to_string()
        } else {
            format!(", {} resisted: {:?}", remain.len(), remain)
        }
    ));
}
