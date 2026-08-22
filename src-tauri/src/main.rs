// Hide console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use siku_lib::run;

fn main() {
    // Show a message box on panic so the user knows something went wrong
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Siku 启动失败:\n{info}");
        eprintln!("{msg}");
        #[cfg(target_os = "windows")]
        {
            use std::ffi::CString;
            unsafe {
                extern "system" {
                    fn MessageBoxA(hwnd: isize, text: *const i8, caption: *const i8, utype: u32) -> i32;
                }
                let title = CString::new("Siku Error").unwrap();
                let text = CString::new(msg).unwrap();
                MessageBoxA(0, text.as_ptr(), title.as_ptr(), 0x10); // MB_ICONERROR
            }
        }
    }));

    run();
}
