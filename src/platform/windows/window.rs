use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{InvalidateRect, RDW_ALLCHILDREN, RDW_FRAME, RDW_INVALIDATE, RedrawWindow},
    UI::WindowsAndMessaging::{
        BringWindowToTop, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow,
        ShowWindow,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct NativeWindowHandle(isize);

impl NativeWindowHandle {
    pub fn new(hwnd: isize) -> Option<Self> {
        (hwnd != 0).then_some(Self(hwnd))
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

pub fn restore_window(handle: NativeWindowHandle) {
    let hwnd = handle.hwnd();
    unsafe {
        let command = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, command);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn window_is_visible(handle: NativeWindowHandle) -> bool {
    unsafe { IsWindowVisible(handle.hwnd()).as_bool() }
}

pub fn force_window_redraw(handle: NativeWindowHandle) {
    let hwnd = handle.hwnd();
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME,
        );
    }
}
