//! Windows IME control implementation using window messages.

use super::{ImeCapabilities, ImeController, ImeDetector, ImeInfo, ImeType};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::LazyLock;

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HWND,
    UI::Input::Ime::ImmGetDefaultIMEWnd,
    UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, IsWindow, SendMessageA,
        WM_IME_CONTROL,
    },
};

/// IME control message constants
#[cfg(windows)]
const IMC_SETOPENSTATUS: u32 = 0x0006;
#[cfg(windows)]
const IMC_GETOPENSTATUS: u32 = 0x0005;

/// Cached foreground window handle.
/// The handle is cached on first use and validated before each use.
/// If the cached handle is invalid, it's refreshed from GetForegroundWindow().
/// This reduces system calls while ensuring we always use a valid window handle.
static CACHED_WINDOW: LazyLock<AtomicPtr<()>> =
    LazyLock::new(|| AtomicPtr::new(std::ptr::null_mut()));

/// Windows IME controller using window messages (WM_IME_CONTROL).
/// This approach is more reliable than using ImmSetOpenStatus directly.
///
/// The controller caches the foreground window handle to reduce system calls,
/// but validates it before each use to ensure it's still valid.
pub struct WindowsImeController;

impl WindowsImeController {
    /// Get a valid foreground window handle, using cache if available and valid.
    /// Returns an error if no foreground window is available.
    ///
    /// This function caches the window handle to avoid repeated calls to GetForegroundWindow(),
    /// but validates the cached handle before use. The cache is refreshed when:
    /// 1. Cache is empty (first call)
    /// 2. Cached window handle is invalid (window closed)
    /// 3. Foreground window has changed (user switched to another app)
    unsafe fn get_valid_foreground_window() -> Result<HWND> {
        // Try to use cached window handle
        let cached_ptr = CACHED_WINDOW.load(Ordering::Acquire);
        let cached_hwnd = cached_ptr as HWND;

        // Validate cached handle if it exists
        if !cached_hwnd.is_null() && IsWindow(cached_hwnd) != 0 {
            // Check if it's still the foreground window
            // This is necessary because IME state is tied to the foreground window
            let current_fg = GetForegroundWindow();
            if current_fg == cached_hwnd {
                log::trace!(
                    "IME: Using cached foreground window handle: {:p}",
                    cached_hwnd
                );
                return Ok(cached_hwnd);
            }
            // Foreground window changed, will update cache below
            log::trace!(
                "IME: Foreground window changed from {:p} to {:p}, refreshing cache",
                cached_hwnd,
                current_fg
            );
        }

        // Cache miss, invalid cache, or foreground window changed - get fresh handle
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            log::debug!("IME: GetForegroundWindow returned null");
            return Err(anyhow::anyhow!("Failed to get foreground window handle"));
        }

        // Update cache
        CACHED_WINDOW.store(hwnd as *mut (), Ordering::Release);
        log::trace!("IME: Cached foreground window handle: {:p}", hwnd);
        Ok(hwnd)
    }

    /// Get the IME window handle for the given application window.
    /// Returns `None` if IME window is not available (IME likely not active).
    unsafe fn get_ime_window(hwnd: HWND) -> Option<HWND> {
        let ime_wnd = ImmGetDefaultIMEWnd(hwnd);
        if ime_wnd.is_null() {
            log::trace!("IME: ImmGetDefaultIMEWnd returned null, IME window not available");
            return None;
        }
        log::trace!("IME: Got IME window handle: {:p}", ime_wnd);
        Some(ime_wnd)
    }
}

impl ImeController for WindowsImeController {
    /// Query whether IME is currently enabled.
    ///
    /// Returns `Ok(true)` if IME is enabled, `Ok(false)` if disabled.
    /// If the IME window is not available, returns `Ok(false)` (IME is considered closed).
    fn is_ime_enabled() -> Result<bool> {
        unsafe {
            let hwnd = Self::get_valid_foreground_window()
                .context("Failed to get foreground window for IME status query")?;

            // If IME window is not available, IME is considered closed
            let ime_wnd = match Self::get_ime_window(hwnd) {
                Some(wnd) => wnd,
                None => return Ok(false),
            };

            // Send WM_IME_CONTROL message directly to the IME window to get IME status
            // According to Windows API documentation:
            // - wParam: IMC_GETOPENSTATUS (0x0005)
            // - lParam: 0 (not used)
            // Returns: non-zero if IME is open, 0 if closed
            // Use SendMessageA (ANSI) instead of SendMessageW for better IME compatibility
            let result = SendMessageA(ime_wnd, WM_IME_CONTROL, IMC_GETOPENSTATUS as usize, 0);

            let is_enabled = result != 0;
            log::trace!(
                "IME: is_ime_enabled result: {} (raw: {})",
                is_enabled,
                result
            );
            // If result is non-zero, IME is open
            Ok(is_enabled)
        }
    }

    /// Set IME enabled/disabled state.
    ///
    /// # Arguments
    /// * `enabled` - `true` to enable IME, `false` to disable
    ///
    /// If the IME window is not available, this function returns `Ok(())` silently,
    /// allowing the function to work even when IME is not active.
    fn set_ime_enabled(enabled: bool) -> Result<()> {
        unsafe {
            log::trace!("IME: set_ime_enabled called with enabled={}", enabled);

            let hwnd = Self::get_valid_foreground_window()
                .context("Failed to get foreground window for IME control")?;

            // If IME window is not available, silently return success
            // This allows the function to work even when IME is not active
            let ime_wnd = match Self::get_ime_window(hwnd) {
                Some(wnd) => wnd,
                None => {
                    log::debug!("IME: IME window not available, skipping control");
                    return Ok(());
                }
            };

            // Send WM_IME_CONTROL message directly to the IME window to set IME status
            // According to Windows API documentation:
            // - wParam: IMC_SETOPENSTATUS (0x0006)
            // - lParam: 1 to enable, 0 to disable
            // Use SendMessageA (ANSI) instead of SendMessageW for better IME compatibility
            let lparam = if enabled { 1 } else { 0 };
            log::trace!(
                "IME: Sending WM_IME_CONTROL to IME window with wParam={}, lParam={}",
                IMC_SETOPENSTATUS,
                lparam
            );

            let result = SendMessageA(ime_wnd, WM_IME_CONTROL, IMC_SETOPENSTATUS as usize, lparam);

            log::trace!("IME: SendMessageA result: {}", result);

            // Also try sending to the application window as a fallback
            // Some IME implementations require sending to the app window first
            let app_result = SendMessageA(hwnd, WM_IME_CONTROL, IMC_SETOPENSTATUS as usize, lparam);
            log::trace!("IME: SendMessageA to app window result: {}", app_result);

            Ok(())
        }
    }

    /// Get information about the current IME
    fn get_ime_info() -> Result<ImeInfo> {
        unsafe {
            let hwnd = Self::get_valid_foreground_window()
                .context("Failed to get foreground window for IME info")?;

            if let Some(ime_wnd) = Self::get_ime_window(hwnd) {
                // Try to get IME window title to identify the IME
                let len = GetWindowTextLengthW(ime_wnd);
                let mut buffer = vec![0u16; (len + 1) as usize];
                let actual_len = GetWindowTextW(ime_wnd, buffer.as_mut_ptr(), buffer.len() as i32);

                let name = if actual_len > 0 {
                    String::from_utf16_lossy(&buffer[..actual_len as usize])
                } else {
                    "Unknown IME".to_string()
                };

                // Detect IME type and capabilities
                let ime_type = ImeDetector::detect_ime_type(&name);
                let capabilities = match ime_type {
                    ImeType::Sogou | ImeType::GooglePinyin | ImeType::Baidu => {
                        ImeCapabilities::FullControl
                    }
                    ImeType::Microsoft => ImeCapabilities::WithState,
                    _ => ImeCapabilities::Basic,
                };

                Ok(ImeInfo {
                    name,
                    version: None, // Could be obtained from registry or file properties
                    capabilities,
                })
            } else {
                Ok(ImeInfo {
                    name: "No IME".to_string(),
                    version: None,
                    capabilities: ImeCapabilities::Basic,
                })
            }
        }
    }

    /// Check if any IME is available
    fn is_ime_available() -> bool {
        unsafe {
            match Self::get_valid_foreground_window() {
                Ok(hwnd) => Self::get_ime_window(hwnd).is_some(),
                Err(_) => false,
            }
        }
    }

    /// Initialize Windows IME support
    fn initialize() -> Result<()> {
        // On Windows, ensure we have proper permissions for IME control
        log::info!("Initializing Windows IME support");

        // Test IME availability
        if !Self::is_ime_available() {
            log::warn!("No IME detected on system");
        }

        Ok(())
    }
}
