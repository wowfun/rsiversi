//! Notify existing shells after preserving and updating the raw user PATH.
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_SETTINGCHANGE,
};

pub(crate) fn notify() -> std::io::Result<()> {
    let environment: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
    // SAFETY: HWND_BROADCAST is a documented pseudo-handle. WM_SETTINGCHANGE is
    // system-marshalled; its LPARAM points to a terminated UTF-16 string alive
    // for this synchronous call. WPARAM is zero and no result pointer is needed.
    // Abort hung recipients and cap each recipient at one second (the Win32 API
    // applies its timeout per top-level window, not to the whole broadcast).
    let result = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            1000,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::other(
            "environment notification timed out or failed",
        ))
    } else {
        Ok(())
    }
}
