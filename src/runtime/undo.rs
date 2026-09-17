//! 布局撤销栈（2026-09-16 从 ui.rs 原样搬出）：
//! 单级布局快照的压入（push_undo/push_undo_snapshot）与恢复（undo_pop_restore）。
//! 叶子模块（2026-09-17 断 undo→ui 上行边）——恢复只做数据回填+落盘,
//! 返回是否已恢复;整面重建呈现由调用方（menu 命令分发）编排。

use std::sync::{Mutex, OnceLock};

use crate::logging::log;
use crate::model::{self, Fence, Rect};
use crate::state::*;

static UNDO_STACK: OnceLock<Mutex<Vec<Vec<Fence>>>> = OnceLock::new();

fn undo_stack() -> &'static Mutex<Vec<Vec<Fence>>> {
    UNDO_STACK.get_or_init(|| Mutex::new(Vec::new()))
}

/// 压入当前布局快照(布局类操作前调用,支持撤销)
pub(crate) fn push_undo() {
    let snap = state().lock().unwrap().fences.clone();
    let mut u = undo_stack().lock().unwrap();
    if u.last().map(|l| *l == snap).unwrap_or(false) {
        return;
    }
    u.push(snap);
    if u.len() > 20 {
        u.remove(0);
    }
}

/// 拖动/缩放前压入"拖动前"快照(拖动过程中矩形已被实时更新)。
/// 调用方已持有 state 锁时传入其克隆的快照,避免同线程重复加锁死锁。
pub(crate) fn push_undo_snapshot(mut snap: Vec<Fence>, fence_id: u32, rect: Rect) {
    if let Some(f) = snap.iter_mut().find(|f| f.id == fence_id) {
        f.rect = rect;
    }
    let mut u = undo_stack().lock().unwrap();
    if u.last().map(|l| *l == snap).unwrap_or(false) {
        return;
    }
    u.push(snap);
    if u.len() > 20 {
        u.remove(0);
    }
}

/// 弹出并恢复上一份快照:回填 fences+落盘,返回是否实际恢复。
/// 恢复后的整面重建(show_all_fences)由调用方负责——undo 不依赖 ui。
pub(crate) fn undo_pop_restore() -> bool {
    let snap = undo_stack().lock().unwrap().pop();
    let Some(fences) = snap else {
        return false;
    };
    if fences.is_empty() {
        log("ignored empty undo snapshot to prevent blank desktop");
        return false;
    }
    {
        let mut s = state().lock().unwrap();
        s.fences = fences;
    }
    // Snapshots are already valid layouts; do not "settle" them again or
    // the restored positions cease to be the exact previous operation.
    {
        let s = state().lock().unwrap();
        let _ = model::save_config(&s.fences);
    }
    log("layout undo applied");
    true
}
