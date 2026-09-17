#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 本体在 lib.rs（集成测试经库目标访问全部模块）；这里只保留子系统属性与入口。
fn main() {
    deskfence::app_main();
}
