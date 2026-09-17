//! 日志子系统(2026-09-16 从 ui.rs 原样搬出,纯搬家不改行为):
//! run.log 追加式诊断日志,4MB 轮转归档 run.log.old,带本地日期时间戳,全程容错。
//! 叶子模块——只依赖 std / crate::model / windows crate,不依赖 crate::ui
//! (ui.rs 上帝文件按依赖方向拆分的第一刀,凡写诊断日志的模块应直接依赖本模块)。

use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::model;

pub fn log(line: &str) {
    let dir = model::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("run.log");
    // 轮转:超 4MB 归档为 run.log.old(覆盖旧档),防长期运行无限增长。
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.len() > 4 * 1024 * 1024 {
            let old = dir.join("run.log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&p, &old);
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        // 本地日期+时间:run.log 跨多次启动追加,只有时分秒无法区分天,
        // 排查偶发问题时对不上用户操作的时刻(2026-08-28 排查实证)。
        let st = unsafe { GetLocalTime() };
        let _ = writeln!(
            f,
            "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}] {}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, line
        );
    }
}
