//! Platform-specific IME (Input Method Editor) control implementation.
//!
//! This module provides a trait-based abstraction for controlling IME across
//! different platforms (Windows, Linux, macOS).

use anyhow::Result;
use std::collections::HashMap;

/// Information about the current IME
#[derive(Debug, Clone)]
pub struct ImeInfo {
    pub name: String,
    #[allow(dead_code)]
    pub version: Option<String>,
    pub capabilities: ImeCapabilities,
}

/// IME capability levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ImeCapabilities {
    /// Only basic on/off control
    Basic,
    /// Can query current state
    WithState,
    /// Full control capabilities (custom settings, etc.)
    FullControl,
}

/// Trait for platform-specific IME control operations.
pub trait ImeController {
    /// Query whether IME is currently enabled.
    ///
    /// Returns `Ok(true)` if IME is enabled, `Ok(false)` if disabled.
    /// Errors may occur due to platform-specific issues (permissions, API unavailable, etc.).
    fn is_ime_enabled() -> Result<bool>;

    /// Set IME enabled/disabled state.
    ///
    /// # Arguments
    /// * `enabled` - `true` to enable IME, `false` to disable
    ///
    /// Returns `Ok(())` on success.
    /// Errors may occur due to platform-specific issues (permissions, API unavailable, etc.).
    fn set_ime_enabled(enabled: bool) -> Result<()>;

    /// Get information about the current IME.
    ///
    /// Returns details about the active IME engine including name, version,
    /// and supported capabilities.
    fn get_ime_info() -> Result<ImeInfo>;

    /// Check if an IME is available and functional.
    ///
    /// Some systems may not have any IME installed or configured.
    #[allow(dead_code)]
    fn is_ime_available() -> bool;

    /// Perform platform-specific initialization.
    ///
    /// Called once during application startup to initialize any required
    /// platform-specific resources.
    fn initialize() -> Result<()> {
        // Default implementation is no-op
        Ok(())
    }
}

/// IME detection and optimization utilities
pub struct ImeDetector;

impl ImeDetector {
    /// Detect common IME engines by name patterns
    pub fn detect_ime_type(ime_name: &str) -> ImeType {
        let name_lower = ime_name.to_lowercase();

        if name_lower.contains("sogou") {
            ImeType::Sogou
        } else if name_lower.contains("microsoft") || name_lower.contains("ms") {
            ImeType::Microsoft
        } else if name_lower.contains("tencent") || name_lower.contains("qq") {
            // Must be checked before the generic "pinyin" pattern below so
            // e.g. "QQ Pinyin" is not misdetected as GooglePinyin.
            ImeType::Tencent
        } else if name_lower.contains("google") || name_lower.contains("pinyin") {
            ImeType::GooglePinyin
        } else if name_lower.contains("fcitx") {
            ImeType::Fcitx
        } else if name_lower.contains("ibus") {
            ImeType::IBus
        } else if name_lower.contains("baidu") {
            ImeType::Baidu
        } else if name_lower.contains("scim") {
            ImeType::SCIM
        } else {
            ImeType::Unknown
        }
    }

    /// Get optimal settings for a specific IME type
    pub fn get_optimal_settings(ime_type: ImeType) -> ImeSettings {
        match ime_type {
            ImeType::Sogou => ImeSettings {
                retry_count: 2,
                retry_delay_ms: 20,
                custom_settings: HashMap::from([
                    ("disable_animation".to_string(), "true".to_string()),
                    ("fast_switch".to_string(), "true".to_string()),
                ]),
            },
            ImeType::Microsoft => ImeSettings {
                retry_count: 3,
                retry_delay_ms: 10,
                custom_settings: HashMap::new(),
            },
            ImeType::GooglePinyin => ImeSettings {
                retry_count: 2,
                retry_delay_ms: 15,
                custom_settings: HashMap::from([(
                    "enhanced_compatibility".to_string(),
                    "true".to_string(),
                )]),
            },
            ImeType::Unknown => ImeSettings::default(),
            _ => ImeSettings::default(),
        }
    }
}

/// Known IME types
#[allow(clippy::upper_case_acronyms)] // SCIM mirrors the upstream input-method name
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeType {
    Sogou,
    Microsoft,
    GooglePinyin,
    Fcitx,
    IBus,
    Baidu,
    Tencent,
    SCIM,
    Unknown,
}

/// Platform-specific IME settings
#[derive(Debug, Clone)]
pub struct ImeSettings {
    pub retry_count: u32,
    /// Reserved for future tuning. IME read retries use [`std::thread::yield_now`] instead of
    /// wall-clock sleep so blocking job threads are not parked for milliseconds.
    #[allow(dead_code)]
    pub retry_delay_ms: u64,
    #[allow(dead_code)]
    pub custom_settings: HashMap<String, String>,
}

impl Default for ImeSettings {
    fn default() -> Self {
        Self {
            retry_count: 3,
            retry_delay_ms: 10,
            custom_settings: HashMap::new(),
        }
    }
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsImeController as PlatformImeController;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxImeController as PlatformImeController;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacosImeController as PlatformImeController;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod fallback;
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub use fallback::FallbackImeController as PlatformImeController;

/// Convenience function to query IME enabled state using platform-specific implementation.
pub fn is_ime_enabled() -> Result<bool> {
    #[cfg(any(test, feature = "integration"))]
    if let Some(fake) = testing::active_platform() {
        return fake.is_ime_enabled();
    }
    PlatformImeController::is_ime_enabled()
}

/// Convenience function to set IME enabled state using platform-specific implementation.
pub fn set_ime_enabled(enabled: bool) -> Result<()> {
    #[cfg(any(test, feature = "integration"))]
    if let Some(fake) = testing::active_platform() {
        return fake.set_ime_enabled(enabled);
    }
    PlatformImeController::set_ime_enabled(enabled)
}

/// Get information about the current IME
pub fn get_ime_info() -> Result<ImeInfo> {
    #[cfg(any(test, feature = "integration"))]
    if let Some(fake) = testing::active_platform() {
        return fake.get_ime_info();
    }
    PlatformImeController::get_ime_info()
}

/// Initialize platform-specific IME support
pub fn initialize() -> Result<()> {
    #[cfg(any(test, feature = "integration"))]
    if let Some(fake) = testing::active_platform() {
        return fake.initialize();
    }
    PlatformImeController::initialize()
}

/// Check whether an IME is available and functional.
#[allow(dead_code)]
pub fn is_ime_available() -> bool {
    #[cfg(any(test, feature = "integration"))]
    if let Some(fake) = testing::active_platform() {
        return fake.is_ime_available();
    }
    PlatformImeController::is_ime_available()
}

/// Test double for the OS IME controller.
///
/// The engine talks to the OS exclusively through the free functions above, so
/// installing a fake here makes every IME test hermetic: no test toggles the
/// developer's real input method, and success/failure/unavailable platform
/// behavior becomes assertable (which is how the fcitx5-remote breakage went
/// unnoticed — real control failures were indistinguishable from successes
/// under the old tests).
#[cfg(any(test, feature = "integration"))]
pub mod testing {
    use super::{ImeCapabilities, ImeInfo};
    use anyhow::Result;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, LazyLock, RwLock,
    };

    /// Scriptable fake platform. Share one instance with the test to observe
    /// and drive it while the engine holds its own handle.
    #[derive(Debug, Default)]
    pub struct FakeImePlatform {
        /// Whether `is_ime_available` reports an IME at all.
        pub available: AtomicBool,
        /// Current enabled state, flipped by `set_ime_enabled`.
        pub enabled: AtomicBool,
        /// Make every `is_ime_enabled` call return an error (FR-019 path).
        pub query_fails: AtomicBool,
        /// Make every `set_ime_enabled` call return an error (FR-019 path).
        pub control_fails: AtomicBool,
        pub query_calls: AtomicU64,
        pub set_calls: AtomicU64,
    }

    impl FakeImePlatform {
        pub fn available_and(enabled: bool) -> Arc<Self> {
            let fake = Arc::new(Self::default());
            fake.available.store(true, Ordering::Release);
            fake.enabled.store(enabled, Ordering::Release);
            fake
        }

        pub(crate) fn is_ime_enabled(&self) -> Result<bool> {
            self.query_calls.fetch_add(1, Ordering::Relaxed);
            if self.query_fails.load(Ordering::Acquire) {
                return Err(anyhow::anyhow!("fake: query failed"));
            }
            Ok(self.enabled.load(Ordering::Acquire))
        }

        pub(crate) fn set_ime_enabled(&self, enabled: bool) -> Result<()> {
            self.set_calls.fetch_add(1, Ordering::Relaxed);
            if self.control_fails.load(Ordering::Acquire) {
                return Err(anyhow::anyhow!("fake: control failed"));
            }
            self.enabled.store(enabled, Ordering::Release);
            Ok(())
        }

        pub(crate) fn get_ime_info(&self) -> Result<ImeInfo> {
            Ok(ImeInfo {
                name: "Fake IME".to_string(),
                version: None,
                capabilities: ImeCapabilities::WithState,
            })
        }

        pub(crate) fn initialize(&self) -> Result<()> {
            Ok(())
        }

        pub(crate) fn is_ime_available(&self) -> bool {
            self.available.load(Ordering::Acquire)
        }
    }

    static OVERRIDE: LazyLock<RwLock<Option<Arc<FakeImePlatform>>>> =
        LazyLock::new(|| RwLock::new(None));

    /// Install `fake` as the platform backend; `None` restores the real OS
    /// controller. Tests install while holding the IME test lock and restore
    /// on drop, so the override never leaks across tests.
    pub fn install_platform(fake: Option<Arc<FakeImePlatform>>) {
        *OVERRIDE.write().unwrap() = fake;
    }

    pub(crate) fn active_platform() -> Option<Arc<FakeImePlatform>> {
        OVERRIDE.read().unwrap().clone()
    }
}
