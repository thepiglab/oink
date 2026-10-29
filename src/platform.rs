//! Platform glue: physical screen size (for a true µV/mm), system fonts, and files opened from
//! Finder on macOS.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Lets platform callbacks wake the UI.
pub fn set_context(ctx: &egui::Context) {
    let _ = CONTEXT.set(ctx.clone());
}

/// Files the OS asked us to open since the last call (Finder double-click, Dock drop).
pub fn take_opened_files() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}

#[allow(dead_code)]
fn push_opened(path: PathBuf) {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).push(path);
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// Logical points per physical millimetre on the main display, if the OS reports the display's
/// physical size. Used so that "7 µV/mm" means 7 µV per real millimetre.
pub fn points_per_mm(pixels_per_point: f32) -> Option<f32> {
    #[cfg(target_os = "macos")]
    let v = {
        let _ = pixels_per_point;
        macos::points_per_mm()
    };
    #[cfg(windows)]
    let v = windows::pixels_per_mm().map(|px| px / pixels_per_point.max(0.5));
    #[cfg(not(any(target_os = "macos", windows)))]
    let v: Option<f32> = {
        let _ = pixels_per_point;
        None
    };
    // 1.5 to 20 points/mm covers 38 to 500 dpi; anything else is a bogus report.
    v.filter(|v| (1.5..20.0).contains(v))
}

/// System UI and monospace fonts: SF on macOS, Segoe UI and Consolas on Windows.
/// Returns (name, data, is_monospace). Missing fonts are skipped; egui's built-in fonts remain
/// as fallbacks.
pub fn system_fonts() -> Vec<(&'static str, egui::FontData, bool)> {
    let candidates: &[(&str, &str, bool)] = if cfg!(target_os = "macos") {
        &[("system-ui", "/System/Library/Fonts/SFNS.ttf", false), ("system-mono", "/System/Library/Fonts/SFNSMono.ttf", true)]
    } else if cfg!(windows) {
        &[("system-ui", "C:\\Windows\\Fonts\\segoeui.ttf", false), ("system-mono", "C:\\Windows\\Fonts\\consola.ttf", true)]
    } else {
        &[]
    };
    candidates
        .iter()
        .filter_map(|&(name, path, mono)| {
            let data = load_font(path)?;
            let mut font = egui::FontData::from_static(data);
            if cfg!(target_os = "macos") && !mono {
                // SF sits a little high in egui's line box.
                font.tweak.y_offset_factor = 0.02;
            }
            Some((name, font, mono))
        })
        .collect()
}

/// Maps the font file instead of copying it, so a large system font (SF is ~8 MB) costs
/// shared, file-backed pages rather than private memory.
#[cfg(unix)]
fn load_font(path: &str) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    // SAFETY: a read-only private mapping of a file we just opened. It is never unmapped, so
    // the slice is valid for the rest of the program.
    unsafe {
        let ptr = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0);
        if ptr == libc::MAP_FAILED {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, len))
    }
}

#[cfg(not(unix))]
fn load_font(path: &str) -> Option<&'static [u8]> {
    std::fs::read(path).ok().map(|v| &*Box::leak(v.into_boxed_slice()))
}

#[cfg(target_os = "macos")]
mod macos {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayScreenSize(display: u32) -> CGSize;
        fn CGDisplayPixelsWide(display: u32) -> usize;
    }

    pub fn points_per_mm() -> Option<f32> {
        // SAFETY: plain CoreGraphics queries on the main display.
        unsafe {
            let display = CGMainDisplayID();
            let size = CGDisplayScreenSize(display);
            // Width of the current display mode in points ("looks like" resolution).
            let points = CGDisplayPixelsWide(display);
            (size.width > 10.0 && points > 0).then(|| (points as f64 / size.width) as f32)
        }
    }

    /// Lets files opened from Finder (double-click, "Open With", drop on the Dock icon) reach
    /// the app. AppKit delivers them to the application delegate, which winit owns and does
    /// not forward, so we add `application:openURLs:` to winit's delegate class. Must run
    /// after the event loop is created (which installs that delegate) and before it starts.
    pub fn install_open_handler() {
        use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
        use objc2::{ffi, sel};
        use objc2_foundation::{NSArray, NSURL};

        unsafe extern "C-unwind" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut NSArray<NSURL>) {
            // SAFETY: AppKit passes a valid NSArray of NSURL (or nil).
            if let Some(urls) = unsafe { urls.as_ref() } {
                for url in urls.iter() {
                    if let Some(path) = url.to_file_path() {
                        super::push_opened(path);
                    }
                }
            }
        }

        let Some(class) = AnyClass::get(c"WinitApplicationDelegate") else { return };
        let f: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>) = open_urls;
        // SAFETY: the function matches the method's type encoding "v@:@@" (void; self, _cmd,
        // NSApplication*, NSArray*). class_addMethod leaves an existing method untouched.
        unsafe {
            let imp: Imp = std::mem::transmute(f);
            ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, sel!(application:openURLs:), imp, c"v@:@@".as_ptr());
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::install_open_handler;

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_RAW_DPI};

    /// Physical pixels per millimetre of the primary monitor, from its EDID ("raw" DPI).
    pub fn pixels_per_mm() -> Option<f32> {
        // SAFETY: plain Win32 queries with valid out-pointers.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let (mut x, mut y) = (0u32, 0u32);
            let ok = GetDpiForMonitor(monitor, MDT_RAW_DPI, &mut x, &mut y) == 0;
            (ok && x > 0).then(|| x as f32 / 25.4)
        }
    }

    /// Lets `--info` print to the terminal it was started from, despite the GUI subsystem.
    pub fn attach_console() {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: no preconditions; failure (no parent console) is harmless.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(windows)]
pub use windows::attach_console;
