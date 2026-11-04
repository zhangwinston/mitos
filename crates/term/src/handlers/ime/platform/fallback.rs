//! Fallback IME controller for unsupported platforms.

use super::{ImeCapabilities, ImeController, ImeInfo};

/// Fallback IME controller for platforms without specific implementation.
///
/// This implementation does nothing but allows the code to compile on
/// unsupported platforms. All operations are no-ops.
pub struct FallbackImeController;

impl ImeController for FallbackImeController {
    fn is_ime_enabled() -> super::Result<bool> {
        // Always return false on unsupported platforms
        log::warn!("IME control not supported on this platform");
        Ok(false)
    }

    fn set_ime_enabled(_enabled: bool) -> super::Result<()> {
        // No-op on unsupported platforms
        log::warn!("IME control not supported on this platform");
        Ok(())
    }

    fn get_ime_info() -> super::Result<ImeInfo> {
        Ok(ImeInfo {
            name: "Unsupported Platform".to_string(),
            version: None,
            capabilities: ImeCapabilities::Basic,
        })
    }

    fn is_ime_available() -> bool {
        false
    }

    fn initialize() -> super::Result<()> {
        log::warn!("IME support not available on this platform");
        Ok(())
    }
}
