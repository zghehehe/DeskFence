//! OLE 拖拽支持：拖出（IDataObject+IDropSource）+ 拖入（IDropTarget）
//!
//! 手写 COM 对象的统一约定（各回调 Safety 段的共同前提）：
//! - 对象是 `#[repr(C)]` 结构体，首字段为 `&'static Vtbl`——对象地址本身就是
//!   合法的 COM 接口指针，字段顺序与官方 vtable 方法序一致（IUnknown 三件套在前）；
//! - vtable 存于编译期 static，终身有效，函数指针 ABI(extern "system"）与 COM
//!   调用约定一致；
//! - 所有回调只由 OLE 引擎（DoDragDrop / 拖放管理器）经 vtable 触达，Rust 代码
//!   从不直接调用它们。

use windows::core::{IUnknown, Interface, GUID, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropTarget, OleFlushClipboard, OleGetClipboard, OleSetClipboard,
    RegisterDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DragQueryFileW, IShellItem, SHCreateDataObject, SHCreateItemFromParsingName,
    SHGetIDListFromObject, HDROP,
};

use crate::shell::wide;

// ReleaseStgMedium 的手写 ole32 导入:用于释放 IDataObject::GetData 移交的
// STGMEDIUM(GetData 成功即把 medium 的所有权交给调用方,必须配对释放)。
#[link(name = "ole32")]
extern "system" {
    fn ReleaseStgMedium(pmedium: *const STGMEDIUM);
}

pub const DRAGDROP_S_CANCEL: i32 = 0x00040101;
pub const DRAGDROP_S_DROP: i32 = 0x00040100;
pub const DRAGDROP_S_USEDEFAULTCURSORS: i32 = 0x00040102;
// 手写 IDropSource vtable 的返回码别名:沿用 Win32 的 HRESULT 拼写,
// 与官方文档/签名一致(不允许大写缩写改名,反而伤可读性)
#[allow(clippy::upper_case_acronyms)]
pub type HRESULT = i32;

// ---------------- IDropSource ----------------

/// 手写 IDropSource vtable：字段顺序即 COM 方法序（IUnknown 三件套 +
/// QueryContinueDrag + GiveFeedback），函数指针 ABI 为 extern "system"，
/// 与 OLE 引擎的调用约定一致。
#[repr(C)]
pub struct DropSourceVtbl {
    pub query_interface: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const GUID,
        *mut *mut std::ffi::c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    pub query_continue_drag: unsafe extern "system" fn(*mut std::ffi::c_void, i32, u32) -> HRESULT,
    pub give_feedback: unsafe extern "system" fn(*mut std::ffi::c_void, u32) -> HRESULT,
}

/// 手写 IDropSource 对象：repr(C) 保证首字段 vtable 位于对象首地址，
/// 对象指针本身就是合法的 COM 接口指针（make_idropsource 里 transmute 的
/// 成立前提）。
#[repr(C)]
pub struct DropSource {
    pub vtable: &'static DropSourceVtbl,
}

// vtable 存于 static：终身有效，全部 DropSource 实例共享同一份。
static DROP_SOURCE_VTABLE: DropSourceVtbl = DropSourceVtbl {
    query_interface: ds_qi,
    add_ref: ds_addref,
    release: ds_release,
    query_continue_drag: ds_qcd,
    give_feedback: ds_gf,
};

/// # Safety
/// 仅由 OLE 引擎经 vtable 回调（共同前提见模块头），Rust 侧不直接调用。
/// 2026-09-17 补全 COM 契约（与 dt_qi 同批）：对 IUnknown/IDropSource 返回
/// this,未知 IID 返回 E_NOINTERFACE 并置空,ppv 空返回 E_POINTER——
/// 此前恒 E_NOINTERFACE 在"接口指针经 DoDragDrop 直传"的路径成立,
/// 但经不起接口探测。
unsafe extern "system" fn ds_qi(
    this: *mut std::ffi::c_void,
    iid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    use windows::Win32::System::Ole::IDropSource;
    if ppv.is_null() {
        return windows::Win32::Foundation::E_POINTER.0;
    }
    let iid = unsafe { &*iid };
    if iid == &<IDropSource as Interface>::IID || iid == &<IUnknown as Interface>::IID {
        unsafe {
            *ppv = this;
        }
        0 // S_OK
    } else {
        unsafe {
            *ppv = std::ptr::null_mut();
        }
        windows::Win32::Foundation::E_NOINTERFACE.0
    }
}
/// # Safety
/// 仅由 OLE 引擎经 vtable 回调，不解引用实参。计数恒 1 的 no-op 成立前提：
/// 对象内存由 make_idropsource 返回的 Box 持有并活过整个 DoDragDrop 会话，
/// 不存在真实的释放时机，AddRef/Release 无需真正计数。
unsafe extern "system" fn ds_addref(_: *mut std::ffi::c_void) -> u32 {
    1
}
/// # Safety
/// 同 ds_addref：no-op 释放，对象存活由调用方持有的 Box 保证。
unsafe extern "system" fn ds_release(_: *mut std::ffi::c_void) -> u32 {
    1
}
/// IDropSource::QueryContinueDrag 的判定纯核:Esc 或右键=取消;
/// 左键已松开=落下;其余=继续拖动
pub fn drop_verdict(esc: bool, lb_down: bool, rb_down: bool) -> HRESULT {
    if esc || rb_down {
        DRAGDROP_S_CANCEL
    } else if !lb_down {
        DRAGDROP_S_DROP
    } else {
        0
    }
}

/// # Safety
/// 仅由 OLE 引擎在拖拽线程的模态循环里同步回调；不解引用 this。
/// 不采用传入的 fEscape/grfKeyState 快照，直读 GetKeyState 实时键态——
/// GetKeyState 读取的是调用线程的键状态，其有效性由"OLE 在拖拽线程
/// 调用"这一调用约定保证。
unsafe extern "system" fn ds_qcd(
    _: *mut std::ffi::c_void,
    _fescape: i32,
    _grfkeystate: u32,
) -> HRESULT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
    };
    let _ = _fescape;
    let _ = _grfkeystate;
    unsafe {
        let esc = (GetKeyState(VK_ESCAPE.0 as i32) as i32) & 0x8000 != 0;
        let lb = (GetKeyState(VK_LBUTTON.0 as i32) as i32) & 0x8000 != 0;
        let rb = (GetKeyState(VK_RBUTTON.0 as i32) as i32) & 0x8000 != 0;
        drop_verdict(esc, lb, rb)
    }
}
/// # Safety
/// 仅由 OLE 引擎经 vtable 回调；不解引用实参，返回码要求 OLE 使用默认
/// 拖拽光标（DRAGDROP_S_USEDEFAULTCURSORS）。
unsafe extern "system" fn ds_gf(_: *mut std::ffi::c_void, _: u32) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

/// 构造手写 IDropSource。Box 持有对象内存，IDropSource 是同一地址的借用视图：
/// 调用方须让 Box 活过接口的全部使用（DoDragDrop 返回前 OLE 会经接口回调），
/// 且先析构接口包装再释放 Box——析构会经 vtable 调 no-op 的 ds_release，
/// 顺序反了是 UAF。接口包装的析构不会释放对象内存。
pub fn make_idropsource() -> (Box<DropSource>, IDropSource) {
    let boxed = Box::new(DropSource {
        vtable: &DROP_SOURCE_VTABLE,
    });
    let raw = Box::into_raw(boxed) as *const DropSource as *mut std::ffi::c_void;
    // SAFETY: DropSource 是 repr(C) 且首字段为 vtable 指针,对象地址即接口
    // 指针;transmute 出的 IDropSource 与 raw 同址,vtable 解析到 'static 的
    // DROP_SOURCE_VTABLE。
    let ids: IDropSource = unsafe { std::mem::transmute(raw) };
    // SAFETY: 与上面 into_raw 是同一指针,仅把所有权重建回 Box 交还调用方;
    // from_raw 只执行一次,无双重释放。
    (unsafe { Box::from_raw(raw as *mut DropSource) }, ids)
}

// ---------------- IDropTarget ----------------

/// 手写 IDropTarget vtable：字段顺序即 COM 方法序（IUnknown 三件套 +
/// DragEnter/DragOver/DragLeave/Drop）。
#[repr(C)]
pub struct DropTargetVtbl {
    pub query_interface: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const GUID,
        *mut *mut std::ffi::c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    pub drag_enter: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        u32,
        POINT,
        *mut u32,
    ) -> HRESULT,
    pub drag_over:
        unsafe extern "system" fn(*mut std::ffi::c_void, u32, POINT, *mut u32) -> HRESULT,
    pub drag_leave: unsafe extern "system" fn(*mut std::ffi::c_void) -> HRESULT,
    pub drop: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        u32,
        POINT,
        *mut u32,
    ) -> HRESULT,
}

/// 手写 IDropTarget 对象：首字段 vtable 使对象地址即接口指针；fence_id 与
/// on_drop 是回调要用的业务数据。对象由 register_drop_target 有意泄漏
/// （随进程存活、堆地址终身稳定）——这是 dt_* 各回调 Safety 论证的根基：
/// OLE 注册后可在任意后续时刻回调，对象必须永不失效。
#[repr(C)]
pub struct DropTargetData {
    pub vtable: &'static DropTargetVtbl,
    pub fence_id: u32,
    pub on_drop: fn(u32, Vec<String>, i32, i32),
}

// vtable 存于 static：终身有效，全部 DropTarget 实例共享同一份。
static DROP_TARGET_VTABLE: DropTargetVtbl = DropTargetVtbl {
    query_interface: dt_qi,
    add_ref: dt_addref,
    release: dt_release,
    drag_enter: dt_effect,
    drag_over: dt_over,
    drag_leave: dt_leave,
    drop: dt_drop,
};

/// # Safety
/// 仅由 OLE 拖放管理器经 vtable 回调。ppv 非空时必须指向可写的接口指针
/// 槽位（COM [out] 参数契约，由 OLE 调用侧保证）；写出的 this 能被调用方
/// 长期持有——成立前提是对象来自 register_drop_target 的有意泄漏 Box
/// （随进程存活）。2026-09-17 补全 COM 契约：只对 IUnknown/IDropTarget
/// 返回 this,未知 IID 返回 E_NOINTERFACE 并置空 ppv;ppv 为空返回
/// E_POINTER——此前"恒 S_OK 写 this"虽在纯拖放注册路径无实际后果,
/// 但经不起任何组件的接口探测。
unsafe extern "system" fn dt_qi(
    this: *mut std::ffi::c_void,
    iid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    use windows::Win32::System::Ole::IDropTarget;
    if ppv.is_null() {
        return windows::Win32::Foundation::E_POINTER.0;
    }
    let iid = unsafe { &*iid };
    if iid == &<IDropTarget as Interface>::IID || iid == &<IUnknown as Interface>::IID {
        unsafe {
            *ppv = this;
        }
        0 // S_OK
    } else {
        unsafe {
            *ppv = std::ptr::null_mut();
        }
        windows::Win32::Foundation::E_NOINTERFACE.0
    }
}
/// # Safety
/// 仅由 OLE 拖放管理器经 vtable 回调，不解引用实参。计数恒 1 的 no-op
/// 成立前提：对象有意泄漏随进程存活，不存在真实的释放时机。
unsafe extern "system" fn dt_addref(_: *mut std::ffi::c_void) -> u32 {
    1
}
/// # Safety
/// 同 dt_addref：no-op 释放，对象终身有效（有意泄漏）。
unsafe extern "system" fn dt_release(_: *mut std::ffi::c_void) -> u32 {
    1
}
/// # Safety
/// 仅由 OLE 拖放管理器经 vtable 回调。pdweffect 是 [in,out] 参数：非空时
/// 必须可读可写（由 OLE 调用侧保证）；实现只 |= 追加 COPY|MOVE、保留
/// 调用方预设位，不触碰其余实参（IDataObject 指针本路径不使用）。
unsafe extern "system" fn dt_effect(
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    _: u32,
    _: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        unsafe {
            *pdweffect |= 1 | 2; // COPY | MOVE
        }
    }
    0
}
/// # Safety
/// 同 dt_effect：pdweffect 非空时必须可读可写（COM [in,out] 契约）。
unsafe extern "system" fn dt_over(
    _: *mut std::ffi::c_void,
    _: u32,
    _: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        unsafe {
            *pdweffect |= 1 | 2; // COPY | MOVE
        }
    }
    0
}
/// # Safety
/// 仅由 OLE 拖放管理器经 vtable 回调；不解引用实参。
unsafe extern "system" fn dt_leave(_: *mut std::ffi::c_void) -> HRESULT {
    0
}
/// # Safety
/// 仅由 OLE 拖放管理器经 vtable 回调，前提：
/// - this 指向 register_drop_target 泄漏的 DropTargetData（repr(C) 首字段即
///   vtable，强转回结构体合法；Box::into_raw 后堆地址终身稳定，故解引用
///   得到的引用在回调期间有效）；
/// - pdataobj 非空时指向有效 IDataObject（转交 read_hdrop，其前提见该函数
///   的 Safety 段）；
/// - pdweffect 同 dt_effect 的 [in,out] 约定。
///
/// 运行期另对 this/pdataobj 做空指针防御（防御是兜底，不改变上述契约）。
unsafe extern "system" fn dt_drop(
    this: *mut std::ffi::c_void,
    pdataobj: *mut std::ffi::c_void,
    _: u32,
    pt: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        unsafe {
            *pdweffect |= 1 | 2; // COPY | MOVE
        }
    }
    if this.is_null() || pdataobj.is_null() {
        return 0;
    }
    let data = unsafe { &*(this as *const DropTargetData) };
    let paths = unsafe { read_hdrop(pdataobj) };
    if !paths.is_empty() {
        // pt 为屏幕坐标,由 ui 层换算到栅栏客户区做命中(回收站图标 = 删除)
        (data.on_drop)(data.fence_id, paths, pt.x, pt.y);
    }
    0
}

/// 解析拖入的 CF_HDROP 文件列表
///
/// # Safety
/// `pdataobj` 必须是指向有效 COM `IDataObject` 对象的原始指针
/// （内部直接 transmute 并调用其 GetData）。包装出的 IDataObject 离开作用域时
/// 会 Release 一次（消费一个引用），调用方须确保对象另有引用托底——拖放会话内
/// 由 OLE 引擎持有，剪贴板对象由 OLE 持有。
pub unsafe fn read_hdrop(pdataobj: *mut std::ffi::c_void) -> Vec<String> {
    unsafe {
        let dataobj: IDataObject = std::mem::transmute(pdataobj);
        let fmt = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        match dataobj.GetData(&fmt) {
            Ok(medium) => {
                let hdrop = HDROP(medium.u.hGlobal.0);
                let out = parse_hdrop(hdrop);
                ReleaseStgMedium(&medium);
                out
            }
            Err(_) => Vec::new(),
        }
    }
}

fn parse_hdrop(hdrop: HDROP) -> Vec<String> {
    // SAFETY: hdrop 来自 GetData 移交的 STGMEDIUM,调用方(read_hdrop)在
    // ReleaseStgMedium 之前调用本函数,句柄全程有效;0xFFFFFFFF 是
    // DragQueryFileW 文档约定的"取文件数"哨兵;4096 宽字符缓冲,超长路径
    // 由 API 截断,不越界。
    unsafe {
        let cnt = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
        let mut out = Vec::new();
        for i in 0..cnt {
            let mut buf = [0u16; 4096];
            let n = DragQueryFileW(hdrop, i, Some(&mut buf));
            if n > 0 {
                out.push(String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        out
    }
}

/// 拖出文件到桌面/其他栅栏（OLE）
fn data_object_for_files(paths: &[String]) -> Option<(IDataObject, Vec<*mut ITEMIDLIST>)> {
    // SAFETY: wide() 产出 NUL 结尾 UTF-16 缓冲,PCWSTR::from_raw 指向其存活期
    // 内的内存(SHCreateItemFromParsingName 同步完成,wp 不越本次迭代作用域);
    // SHGetIDListFromObject 返回的 pidl 由 COM 分配器分配,失败路径逐一
    // CoTaskMemFree 配对释放,成功路径所有权随返回值移交调用方;
    // SHCreateDataObject 生成的数据对象自持项目副本,不借用传入的 pidl 数组。
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for p in paths {
            let wp = wide(p);
            if let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR::from_raw(wp.as_ptr()), None)
            {
                if let Ok(pidl) = SHGetIDListFromObject(&item) {
                    pidls.push(pidl);
                }
            }
        }
        if pidls.is_empty() {
            return None;
        }
        let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        match SHCreateDataObject(None, Some(&ptrs), None) {
            Ok(obj) => Some((obj, pidls)),
            Err(_) => {
                for pidl in pidls {
                    windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
                }
                None
            }
        }
    }
}

pub fn clipboard_set_files(paths: &[String]) -> bool {
    let Some((data, pidls)) = data_object_for_files(paths) else {
        return false;
    };
    // SAFETY: data 是有效 COM 对象;OleSetClipboard 成功后 OLE 自持一份引用
    // (本函数持有的 data 随作用域正常析构,各减各的);OleFlushClipboard 把数据
    // 渲染落盘。pidl 数组所有权仍归本函数(见 data_object_for_files 的 SAFETY),
    // 逐一 CoTaskMemFree 配对。
    let ok = unsafe {
        OleSetClipboard(&data)
            .and_then(|_| OleFlushClipboard())
            .is_ok()
    };
    for pidl in pidls {
        unsafe {
            // SAFETY: 承接上方配对释放论证。
            windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
        }
    }
    ok
}

pub fn clipboard_get_files() -> Vec<String> {
    // SAFETY: OleGetClipboard 返回持引用的对象;as_raw() 只借用不转移所有权,
    // obj 活过 read_hdrop 整个调用,满足其 Safety 前提。
    unsafe {
        OleGetClipboard()
            .map(|obj| read_hdrop(obj.as_raw()))
            .unwrap_or_default()
    }
}

pub fn clipboard_clear() {
    // SAFETY: 传 None 清空剪贴板,无指针有效性前提。
    unsafe {
        let _ = OleSetClipboard(None::<&IDataObject>);
    }
}

/// OLE 拖出文件（到桌面/其他栅栏/外部应用）。阻塞式：DoDragDrop 模态循环
/// 期间经 vtable 同步回调 ds_*（生命周期约定见 make_idropsource）。
pub fn drag_out_files(paths: &[String], _on_dropped_into_fence: impl Fn(&[String]) + 'static) {
    // SAFETY(整块):wide() 缓冲与 pidl 配对释放的论证同 data_object_for_files;
    // DoDragDrop 阻塞期间 OLE 经 vtable 回调拖源——_keep 持有 DropSource 的
    // Box 直到 DoDragDrop 返回后才显式释放,回调全程对象存活;effect 是有效的
    // 栈上 [out] 槽位。
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for p in paths {
            let w = wide(p);
            if let Ok(item) = SHCreateItemFromParsingName::<PCWSTR, _, IShellItem>(
                PCWSTR::from_raw(w.as_ptr()),
                None,
            ) {
                if let Ok(pidl) = SHGetIDListFromObject(&item) {
                    if !pidl.is_null() {
                        pidls.push(pidl);
                    }
                }
            }
        }
        if pidls.is_empty() {
            return;
        }
        let refs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        // pdtInner(聚合进既有数据对象)此处不用,传 None(IntoParam 对 Option<&T>
        // 有实现,FFI 侧收到空指针)。2026-09-16 前是空指针 transmute 出 IDataObject
        // 空壳——windows-core 的接口包装是 NonNull,空壳本身就是无效值,且
        // 隐式析构会经空 vtable 调 Release(真 UB,此前未发作只是代码生成恰好
        // 消除了那次析构,属脆弱巧合)。
        let dataobj: IDataObject = match SHCreateDataObject::<Option<&IDataObject>, IDataObject>(
            None,
            Some(&refs),
            None,
        ) {
            Ok(d) => d,
            Err(_) => {
                for pidl in &pidls {
                    windows::Win32::System::Com::CoTaskMemFree(Some(*pidl as *const _));
                }
                return;
            }
        };
        let (_keep, source) = make_idropsource();
        let mut effect = DROPEFFECT(0);
        let _ = DoDragDrop(
            &dataobj,
            &source,
            DROPEFFECT_COPY | DROPEFFECT_MOVE,
            &mut effect,
        );
        for pidl in &pidls {
            windows::Win32::System::Com::CoTaskMemFree(Some(*pidl as *const _));
        }
        // 先析构接口包装再释放 Box:source 的析构要经 vtable 调 ds_release,
        // 若先 drop(_keep) 就是读已释放内存的 UAF(ds_release 是 no-op 故
        // 实践无害,但顺序必须正确,2026-09-16 修)。
        drop(source);
        drop(_keep);
    }
}

/// 为栅栏窗口注册拖入目标，返回注册是否成功。DropTargetData 有意泄漏随进程
/// 存活：OLE 注册后可在任意后续时刻回调，没有安全的释放时机（窗口注销
/// RevokeDragDrop 之后，泄漏内存随进程退出回收）。
pub fn register_drop_target(hwnd: HWND, fence_id: u32) -> bool {
    let raw = Box::into_raw(Box::new(DropTargetData {
        vtable: &DROP_TARGET_VTABLE,
        fence_id,
        on_drop: on_fence_drop,
    }));
    // SAFETY: 布局论证同 make_idropsource——repr(C) 首字段 vtable,对象地址
    // 即接口指针;Box 有意泄漏,接口指针终身有效。
    let target: IDropTarget =
        unsafe { std::mem::transmute(raw as *const DropTargetData as *mut std::ffi::c_void) };
    // SAFETY: hwnd 是调用方持有的有效窗口;target 如上论证有效。OLE 侧的
    // AddRef/Release 落到 no-op 的 dt_addref/dt_release,对象因泄漏始终有效。
    unsafe { RegisterDragDrop(hwnd, &target).is_ok() }
}

/// 拖入回调注入(2026-09-17 断 ole→drag 上行边):drag 在启动时注册落位
/// 处理器,本模块保持纯 COM 胶水、零上层依赖。fn 指针天然 Send/Sync。
type FenceDropCb = fn(u32, Vec<String>, i32, i32);
static FENCE_DROP_CB: std::sync::OnceLock<FenceDropCb> = std::sync::OnceLock::new();

pub fn set_fence_drop_cb(cb: FenceDropCb) {
    let _ = FENCE_DROP_CB.set(cb);
}

fn on_fence_drop(fence_id: u32, paths: Vec<String>, screen_x: i32, screen_y: i32) {
    if let Some(cb) = FENCE_DROP_CB.get() {
        cb(fence_id, paths, screen_x, screen_y);
    }
}
