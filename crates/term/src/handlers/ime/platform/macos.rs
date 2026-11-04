//! macOS IME control implementation using TIS (Text Input Source) API.

use super::{ImeCapabilities, ImeController, ImeDetector, ImeInfo, ImeType};
use anyhow::{Context, Result};
use std::process::Command;

/// macOS IME controller using TIS (Text Input Source) APIs.
pub struct MacosImeController;

impl MacosImeController {
    /// Get current input source using imctl command
    fn get_current_input_source() -> Result<String> {
        let output = Command::new("/usr/bin/imctl")
            .output()
            .context("Failed to run imctl")?;

        if output.status.success() {
            let source = String::from_utf8_lossy(&output.stdout);
            Ok(source.trim().to_string())
        } else {
            // Fallback: try defaults read
            let output = Command::new("defaults")
                .args(&[
                    "read",
                    "-g",
                    "com.apple.HIToolbox",
                    "AppleCurrentInputMethod",
                ])
                .output()
                .context("Failed to read input method defaults")?;

            if output.status.success() {
                let source = String::from_utf8_lossy(&output.stdout);
                // Extract just the input method name
                if let Some(start) = source.find('"') {
                    if let Some(end) = source.rfind('"') {
                        Ok(source[start + 1..end].to_string())
                    } else {
                        Ok("Unknown".to_string())
                    }
                } else {
                    Ok(source.trim().to_string())
                }
            } else {
                Err(anyhow::anyhow!("No way to determine current input source"))
            }
        }
    }

    /// Check if an input source is an IME
    fn is_ime_input_source(source: &str) -> bool {
        source.to_lowercase().contains("pinyin")
            || source.to_lowercase().contains("sogou")
            || source.to_lowercase().contains("google")
            || source.to_lowercase().contains("baidu")
            || source.to_lowercase().contains("chinese")
            || source.to_lowercase().contains("kotoeri")
            || source.to_lowercase().contains("mazer")
            || source.to_lowercase().contains("tenkey")
    }

    /// Switch input using imselect or defaults
    fn switch_input_source(source: &str) -> Result<()> {
        // Try imselect first
        if Command::new("/usr/bin/imselect")
            .arg(source)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(());
        }

        // Fallback to defaults
        let output = Command::new("defaults")
            .args(&[
                "write",
                "-g",
                "com.apple.HIToolbox",
                "AppleCurrentInputMethod",
                "-string",
                source,
            ])
            .output()
            .context("Failed to switch input method")?;

        if output.status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to switch to input source: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

impl ImeController for MacosImeController {
    fn is_ime_enabled() -> Result<bool> {
        match Self::get_current_input_source() {
            Ok(source) => Ok(Self::is_ime_input_source(&source)),
            Err(_) => Ok(false),
        }
    }

    fn set_ime_enabled(enabled: bool) -> Result<()> {
        let current = Self::get_current_input_source()?;

        if enabled {
            // If we're already using an IME, do nothing
            if Self::is_ime_input_source(&current) {
                return Ok(());
            }

            // Try to switch to a common Chinese IME
            // Note: These are common values but may vary per installation
            let ime_sources = vec![
                "com.apple.inputmethod.SCIM.ITABC",
                "com.apple.inputmethod.SCIM.Shuangpin",
                "com.apple.keylayout.US", // Fallback to US if no IME found
            ];

            for source in ime_sources {
                if Self::switch_input_source(source).is_ok() {
                    log::info!("Switched to input source: {}", source);
                    return Ok(());
                }
            }
        } else {
            // If not an IME already, do nothing
            if !Self::is_ime_input_source(&current) {
                return Ok(());
            }

            // Switch to US keyboard
            Self::switch_input_source("com.apple.keylayout.US")?;
        }

        Ok(())
    }

    fn get_ime_info() -> Result<ImeInfo> {
        match Self::get_current_input_source() {
            Ok(source) => {
                let ime_type = ImeDetector::detect_ime_type(&source);
                let capabilities = if Self::is_ime_input_source(&source) {
                    ImeCapabilities::WithState
                } else {
                    ImeCapabilities::Basic
                };
                let _ = ime_type;

                Ok(ImeInfo {
                    name: source,
                    version: None,
                    capabilities,
                })
            }
            Err(e) => {
                log::error!("Failed to get macOS input source: {}", e);
                Ok(ImeInfo {
                    name: "Unknown".to_string(),
                    version: None,
                    capabilities: ImeCapabilities::Basic,
                })
            }
        }
    }

    fn is_ime_available() -> bool {
        // On macOS, IME is always available (built-in)
        true
    }

    fn initialize() -> Result<()> {
        log::info!("Initializing macOS IME support");

        // Verify TIS is accessible
        match Self::get_current_input_source() {
            Ok(source) => {
                log::debug!("Current input source: {}", source);
            }
            Err(e) => {
                log::error!("Failed to query initial input source: {}", e);
            }
        }

        Ok(())
    }
}
