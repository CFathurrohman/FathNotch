//! Dropping files or text onto the notch: it opens on the Shelf while you drag, and a drop keeps them there.

use windows::core::{implement, Result};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

const CF_UNICODETEXT: u16 = 13;
const CF_HDROP: u16 = 15;

/// What the shell does with drag events (runs on the window thread).
pub struct Hooks {
    pub enter: fn(),
    pub leave: fn(),
    pub drop: fn(Vec<String>, Option<String>),
}

#[implement(IDropTarget)]
struct Target {
    hooks: Hooks,
}

fn format(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn has(data: Option<&IDataObject>, cf: u16) -> bool {
    data.is_some_and(|d| unsafe { d.QueryGetData(&format(cf)).is_ok() })
}

fn files(data: &IDataObject) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&format(CF_HDROP)) else { return out };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(String::from_utf16_lossy(&buf[..len]));
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

fn text(data: &IDataObject) -> Option<String> {
    unsafe {
        let mut medium = data.GetData(&format(CF_UNICODETEXT)).ok()?;
        let g = medium.u.hGlobal;
        let p = GlobalLock(g) as *const u16;
        let mut s = None;
        if !p.is_null() {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            s = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
            let _ = GlobalUnlock(g);
        }
        ReleaseStgMedium(&mut medium);
        s.filter(|t| !t.trim().is_empty())
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let ok = has(data, CF_HDROP) || has(data, CF_UNICODETEXT);
        unsafe { *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        if ok {
            (self.hooks.enter)();
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        (self.hooks.leave)();
        Ok(())
    }

    fn Drop(&self, data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = DROPEFFECT_COPY };
        if let Some(d) = data {
            let f = files(d);
            let t = if f.is_empty() { text(d) } else { None };
            (self.hooks.drop)(f, t);
        }
        Ok(())
    }
}

/// Makes the window accept drops (needs OleInitialize on this thread).
pub fn register(hwnd: HWND, hooks: Hooks) {
    let target: IDropTarget = Target { hooks }.into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}
