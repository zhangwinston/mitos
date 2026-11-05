//! Platform-specific IME testing
//!
//! Adapted from helix: the OS-specific subtests there reached into private
//! controller internals (`LinuxImeController::is_ibus_running`, the private
//! `platform::{linux,windows,macos}` modules). Those internals are private in
//! mitos too, so every OS-specific probe goes through the public platform
//! facade (`platform::is_ime_available`/`get_ime_info`/`initialize`) instead.
//! The state-operation and editor-integration tests drive the scriptable
//! `FakeImePlatform` so they stay hermetic instead of toggling the developer's
//! real input method (same treatment as the `ime.rs` tests).

use super::helpers::test_syntax_loader;
use super::ime::{ime_test_config, ime_test_env, ime_test_env_with, overwrite_document_text};
use editor_core::Selection;
use std::sync::atomic::Ordering;
use term::{
    application::TestApplication,
    args::Args,
    handlers::ime::{self, platform, platform::testing::FakeImePlatform},
};
use view::document::Mode;
use workspace::trust::WorkspaceTrust;

#[tokio::test(flavor = "multi_thread")]
async fn test_ime_platform_detection() -> anyhow::Result<()> {
    // Hermetic: probe the platform facade through a fake IME instead of the
    // real OS controller.
    let fake = FakeImePlatform::available_and(false);
    let _ime_env = ime_test_env_with(fake.clone());

    // Test platform detection
    println!("Testing platform IME support...");

    // Check if IME is available
    let is_available = platform::is_ime_available();
    println!("  IME Available: {}", is_available);
    assert!(is_available, "fake platform must report itself available");

    if is_available {
        // Get IME info
        match platform::get_ime_info() {
            Ok(ime_info) => {
                println!("  IME Name: {}", ime_info.name);
                println!("  Capabilities: {:?}", ime_info.capabilities);
                if let Some(version) = ime_info.version {
                    println!("  Version: {}", version);
                }

                // Test IME state queries
                match platform::is_ime_enabled() {
                    Ok(enabled) => println!(
                        "  Current State: {}",
                        if enabled { "Enabled" } else { "Disabled" }
                    ),
                    Err(e) => println!("  Failed to query state: {}", e),
                }
            }
            Err(e) => println!("  Failed to get IME info: {}", e),
        }
    }

    // Test initialization
    match platform::initialize() {
        Ok(()) => println!("  Platform initialized successfully"),
        Err(e) => println!("  Initialization failed: {}", e),
    }

    assert_eq!(fake.query_calls.load(Ordering::Relaxed), 1);
    assert!(platform::initialize().is_ok());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ime_type_detection() {
    use term::handlers::ime::platform::{ImeDetector, ImeType};

    // Test IME type detection with various names. The tencent/qq arm is
    // checked before the generic "pinyin" pattern, so "QQ Pinyin" is
    // detected as Tencent rather than GooglePinyin.
    let test_cases = vec![
        ("Sogou Pinyin", ImeType::Sogou),
        ("Microsoft IME", ImeType::Microsoft),
        ("Google Pinyin", ImeType::GooglePinyin),
        ("fcitx", ImeType::Fcitx),
        ("IBus", ImeType::IBus),
        ("Baidu IME", ImeType::Baidu),
        ("QQ Pinyin", ImeType::Tencent),
        ("Tencent QQ", ImeType::Tencent),
        ("Unknown IME", ImeType::Unknown),
    ];

    for (name, expected_type) in test_cases {
        let detected = ImeDetector::detect_ime_type(name);
        assert_eq!(
            detected, expected_type,
            "Failed to detect IME type for: {}",
            name
        );

        // Get settings for this type
        let settings = ImeDetector::get_optimal_settings(detected);
        println!(
            "  {}: retry_count={}, retry_delay_ms={}",
            name, settings.retry_count, settings.retry_delay_ms
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ime_state_operations() -> anyhow::Result<()> {
    // Skip if IME is not available
    if !platform::is_ime_available() {
        println!("Skipping IME state test - no IME available");
        return Ok(());
    }

    // Hermetic fake platform: the state operations below round-trip through
    // the injected controller, never the real OS IME.
    let fake = FakeImePlatform::available_and(false);
    let _ime_env = ime_test_env_with(fake.clone());

    let app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;

    // Set editor to insert mode
    let mut app = app;
    app.editor.set_mode(Mode::Insert);

    // Try to get current state
    let initial_state = platform::is_ime_enabled();
    match initial_state {
        Ok(state) => println!("Initial IME state: {}", state),
        Err(ref e) => println!("Failed to get initial state: {}", e),
    }

    // Try to toggle state
    if let Ok(initial) = initial_state {
        let target = !initial;
        match platform::set_ime_enabled(target) {
            Ok(()) => {
                println!("Successfully set IME state to: {}", target);

                // Verify state changed
                std::thread::sleep(std::time::Duration::from_millis(100));

                match platform::is_ime_enabled() {
                    Ok(actual) => {
                        if actual == target {
                            println!("State change verified");
                        } else {
                            println!("State change failed (expected {}, got {})", target, actual);
                        }
                    }
                    Err(e) => println!("Failed to verify state: {}", e),
                }

                // Restore original state
                let _ = platform::set_ime_enabled(initial);
            }
            Err(e) => println!("Failed to set IME state: {}", e),
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ime_integration_with_editor() -> anyhow::Result<()> {
    let fake = FakeImePlatform::available_and(true);
    let _ime_env = ime_test_env_with(fake.clone());
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;
    let view_id = app.editor.tree.focus();

    // Create test content with different regions
    let doc_id = overwrite_document_text(
        &mut app,
        view_id,
        r#"fn test_function() {
    // This is a comment
    let string = "IME should work here";
    let value = 42; // IME should be off here
}
"#,
    );

    // Set language for proper syntax highlighting
    app.editor.set_document_language(doc_id, Some("rust"))?;

    // Test different positions
    app.editor.set_mode(Mode::Insert);

    // Move to string
    {
        let doc = view::doc_mut!(app.editor, &doc_id);
        let text_str = doc.text().to_string();
        let pos = text_str.find("IME should work here").unwrap();
        let pos_char = text_str[..pos].chars().count();
        doc.set_selection(view_id, Selection::single(pos_char, pos_char));
    }

    ime::handle_cursor_move(&mut app.editor, view_id)?;
    println!("Cursor moved to string position");

    // Move to code
    {
        let doc = view::doc_mut!(app.editor, &doc_id);
        let text_str = doc.text().to_string();
        let pos = text_str.find("42").unwrap();
        let pos_char = text_str[..pos].chars().count();
        doc.set_selection(view_id, Selection::single(pos_char, pos_char));
    }

    ime::handle_cursor_move(&mut app.editor, view_id)?;
    println!("Cursor moved to code position");

    // Verify IME state consistency
    match ime::verify_ime_state_consistency(&app.editor, view_id) {
        Ok(consistent) => println!("IME state consistency: {}", consistent),
        Err(e) => println!("Failed to verify consistency: {}", e),
    }

    Ok(())
}

/// Windows-specific IME probing.
///
/// helix reached into `platform::windows::WindowsImeController` (private in
/// mitos); the same information is exposed through the public facade.
#[cfg(target_os = "windows")]
#[tokio::test(flavor = "multi_thread")]
async fn test_windows_ime_specific() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();

    println!("Testing Windows-specific IME features...");

    // Test IME window detection
    let is_available = platform::is_ime_available();
    println!("  IME Window Available: {}", is_available);

    if is_available {
        // Get IME info
        match platform::get_ime_info() {
            Ok(info) => {
                println!("  IME Name: {}", info.name);
                println!("  Capabilities: {:?}", info.capabilities);
            }
            Err(e) => println!("  Failed to get Windows IME info: {}", e),
        }
    }

    Ok(())
}

/// Linux-specific IME probing.
///
/// helix reached into `platform::linux::LinuxImeController` and its private
/// daemon checks (`is_ibus_running`, `is_fcitx_running`, `get_ibus_engine`,
/// `get_fcitx_engine`); those are private in mitos, so probe through the
/// public facade (`is_ime_available` + `get_ime_info`, which reports the
/// detected daemon and its state) instead.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn test_linux_ime_specific() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();

    println!("Testing Linux-specific IME features...");

    // Check for IME daemons
    let is_available = platform::is_ime_available();
    println!("  IME daemon available: {}", is_available);

    if is_available {
        match platform::get_ime_info() {
            Ok(info) => {
                println!("  IME Name: {}", info.name);
                println!("  Capabilities: {:?}", info.capabilities);
            }
            Err(e) => println!("  Failed to get IME info: {}", e),
        }

        match platform::is_ime_enabled() {
            Ok(enabled) => println!("  Current state: {}", enabled),
            Err(e) => println!("  Failed to query state: {}", e),
        }
    } else {
        println!("  No IME daemon (IBus/FCITX) detected");
    }

    Ok(())
}

/// macOS-specific IME probing.
///
/// helix reached into `platform::macos::MacosImeController` (private in
/// mitos); the same information is exposed through the public facade.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn test_macos_ime_specific() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();

    println!("Testing macOS-specific IME features...");

    // Get current input source
    match platform::get_ime_info() {
        Ok(info) => {
            println!("  IME Name: {}", info.name);
            println!("  Capabilities: {:?}", info.capabilities);
        }
        Err(e) => println!("  Failed to get input source: {}", e),
    }

    Ok(())
}
