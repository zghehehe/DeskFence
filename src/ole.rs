//! OLE 拖拽支持：拖出（IDataObject+IDropSource）+ 拖入（IDropTarget）

use windows::core::{Interface, GUID, PCWSTR};
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

#[link(name = "ole32")]
extern "system" {
    fn ReleaseStgMedium(pmedium: *const STGMEDIUM);
}

pub const DRAGDROP_S_CANCEL: i32 = 0x00040101;
pub const DRAGDROP_S_DROP: i32 = 0x00040100;
pub const DRAGDROP_S_USEDEFAULTCURSORS: i32 = 0x00040102;
pub type HRESULT = i32;

// ---------------- IDropSource ----------------

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

#[repr(C)]
pub struct DropSource {
    pub vtable: &'static DropSourceVtbl,
}

static DROP_SOURCE_VTABLE: DropSourceVtbl = DropSourceVtbl {
    query_interface: ds_qi,
    add_ref: ds_addref,
    release: ds_release,
    query_continue_drag: ds_qcd,
    give_feedback: ds_gf,
};

unsafe extern "system" fn ds_qi(
    _: *mut std::ffi::c_void,
    _: *const GUID,
    _: *mut *mut std::ffi::c_void,
) -> HRESULT {
    windows::Win32::Foundation::E_NOINTERFACE.0
}
unsafe extern "system" fn ds_addref(_: *mut std::ffi::c_void) -> u32 {
    1
}
unsafe extern "system" fn ds_release(_: *mut std::ffi::c_void) -> u32 {
    1
}
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
    let esc = (GetKeyState(VK_ESCAPE.0 as i32) as i32) & 0x8000 != 0;
    let lb = (GetKeyState(VK_LBUTTON.0 as i32) as i32) & 0x8000 != 0;
    let rb = (GetKeyState(VK_RBUTTON.0 as i32) as i32) & 0x8000 != 0;
    if esc || rb {
        DRAGDROP_S_CANCEL
    } else if !lb {
        DRAGDROP_S_DROP
    } else {
        0
    }
}
unsafe extern "system" fn ds_gf(_: *mut std::ffi::c_void, _: u32) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

pub fn make_idropsource() -> (Box<DropSource>, IDropSource) {
    let boxed = Box::new(DropSource {
        vtable: &DROP_SOURCE_VTABLE,
    });
    let raw = Box::into_raw(boxed) as *const DropSource as *mut std::ffi::c_void;
    let ids: IDropSource = unsafe { std::mem::transmute(raw) };
    (unsafe { Box::from_raw(raw as *mut DropSource) }, ids)
}

// ---------------- IDropTarget ----------------

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

#[repr(C)]
pub struct DropTargetData {
    pub vtable: &'static DropTargetVtbl,
    pub fence_id: u32,
    pub on_drop: fn(u32, Vec<String>, i32, i32),
}

static DROP_TARGET_VTABLE: DropTargetVtbl = DropTargetVtbl {
    query_interface: dt_qi,
    add_ref: dt_addref,
    release: dt_release,
    drag_enter: dt_effect,
    drag_over: dt_over,
    drag_leave: dt_leave,
    drop: dt_drop,
};

unsafe extern "system" fn dt_qi(
    this: *mut std::ffi::c_void,
    _: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if !ppv.is_null() {
        *ppv = this;
    }
    0
}
unsafe extern "system" fn dt_addref(_: *mut std::ffi::c_void) -> u32 {
    1
}
unsafe extern "system" fn dt_release(_: *mut std::ffi::c_void) -> u32 {
    1
}
unsafe extern "system" fn dt_effect(
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    _: u32,
    _: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        *pdweffect |= 1 | 2; // COPY | MOVE
    }
    0
}
unsafe extern "system" fn dt_over(
    _: *mut std::ffi::c_void,
    _: u32,
    _: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        *pdweffect |= 1 | 2; // COPY | MOVE
    }
    0
}
unsafe extern "system" fn dt_leave(_: *mut std::ffi::c_void) -> HRESULT {
    0
}
unsafe extern "system" fn dt_drop(
    this: *mut std::ffi::c_void,
    pdataobj: *mut std::ffi::c_void,
    _: u32,
    pt: POINT,
    pdweffect: *mut u32,
) -> HRESULT {
    if !pdweffect.is_null() {
        *pdweffect |= 1 | 2; // COPY | MOVE
    }
    if this.is_null() || pdataobj.is_null() {
        return 0;
    }
    let data = &*(this as *const DropTargetData);
    let paths = read_hdrop(pdataobj);
    if !paths.is_empty() {
        // pt 为屏幕坐标,由 ui 层换算到栅栏客户区做命中(回收站图标 = 删除)
        (data.on_drop)(data.fence_id, paths, pt.x, pt.y);
    }
    0
}

/// 解析拖入的 CF_HDROP 文件列表
pub fn read_hdrop(pdataobj: *mut std::ffi::c_void) -> Vec<String> {
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
                let hdrop = HDROP(medium.u.hGlobal.0 as isize);
                let out = parse_hdrop(hdrop);
                ReleaseStgMedium(&medium);
                out
            }
            Err(_) => Vec::new(),
        }
    }
}

fn parse_hdrop(hdrop: HDROP) -> Vec<String> {
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
    let ok = unsafe {
        OleSetClipboard(&data)
            .and_then(|_| OleFlushClipboard())
            .is_ok()
    };
    for pidl in pidls {
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
        }
    }
    ok
}

pub fn clipboard_get_files() -> Vec<String> {
    unsafe {
        OleGetClipboard()
            .map(|obj| read_hdrop(obj.as_raw()))
            .unwrap_or_default()
    }
}

pub fn clipboard_clear() {
    unsafe {
        let _ = OleSetClipboard(None::<&IDataObject>);
    }
}

pub fn drag_out_files(paths: &[String], _on_dropped_into_fence: impl Fn(&[String]) + 'static) {
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
        let null: IDataObject = std::mem::transmute(std::ptr::null_mut::<std::ffi::c_void>());
        let dataobj: IDataObject =
            match SHCreateDataObject::<&IDataObject, IDataObject>(None, Some(&refs), &null) {
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
        drop(_keep);
    }
}

/// 为栅栏窗口注册拖入目标，返回泄漏的 DropTargetData（随进程存活）
pub fn register_drop_target(hwnd: HWND, fence_id: u32) -> bool {
    let raw = Box::into_raw(Box::new(DropTargetData {
        vtable: &DROP_TARGET_VTABLE,
        fence_id,
        on_drop: on_fence_drop,
    }));
    let target: IDropTarget =
        unsafe { std::mem::transmute(raw as *const DropTargetData as *mut std::ffi::c_void) };
    unsafe { RegisterDragDrop(hwnd, &target).is_ok() }
}

fn on_fence_drop(fence_id: u32, paths: Vec<String>, screen_x: i32, screen_y: i32) {
    crate::drag::on_fence_drop_cb(fence_id, paths, screen_x, screen_y);
}
