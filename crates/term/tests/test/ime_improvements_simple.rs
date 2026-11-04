//! Simple test to verify IME improvements work correctly

use super::helpers::test_syntax_loader;
use super::ime::{ime_test_config, ime_test_env};
use std::time::Duration;
use term::{
    application::TestApplication,
    args::Args,
    handlers::ime::{self, registry},
};
use view::{document::Mode, editor::Action};
use workspace::trust::WorkspaceTrust;

#[tokio::test(flavor = "multi_thread")]
async fn test_improvements_simple() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;

    // Get initial view
    let view1_id = app.editor.tree.focus();

    // Create additional views to test cleanup
    let doc1_id = app.editor.tree.get(view1_id).document_id();
    app.editor.switch(doc1_id, Action::VerticalSplit);
    let view2_id = app.editor.tree.focus();
    let doc2_id = app.editor.tree.get(view2_id).document_id();
    app.editor.switch(doc2_id, Action::HorizontalSplit);
    let view3_id = app.editor.tree.focus();

    // Enable IME tracking for views
    app.editor.set_mode(Mode::Insert);
    ime::handle_cursor_move(&mut app.editor, view1_id)?;
    ime::handle_cursor_move(&mut app.editor, view2_id)?;
    ime::handle_cursor_move(&mut app.editor, view3_id)?;

    // Check metrics
    let before_metrics = registry::get_registry_metrics();
    assert!(
        before_metrics.current_contexts() >= 3,
        "Should have created contexts for all views"
    );

    // Close one view
    app.editor.close(view3_id);

    // Run cleanup
    registry::prune_orphans(&app.editor);

    // Verify cleanup worked
    let after_metrics = registry::get_registry_metrics();
    assert!(
        after_metrics.current_contexts() < before_metrics.current_contexts(),
        "Cleanup should have removed context for closed view"
    );

    // Test manual cleanup
    registry::cleanup_old_contexts(Duration::from_millis(1));

    // Final metrics check
    let final_metrics = registry::get_registry_metrics();
    assert!(
        final_metrics.total_contexts_created() >= 3,
        "Should track total contexts created"
    );

    println!("✓ All IME improvements verified successfully");
    println!("  Final metrics:");
    println!("    Total created: {}", final_metrics.total_contexts_created());
    println!("    Total removed: {}", final_metrics.total_contexts_removed());
    println!("    Current contexts: {}", final_metrics.current_contexts());
    println!("    Max concurrent: {}", final_metrics.max_concurrent_contexts());
    println!("    Cleanup count: {}", final_metrics.cleanup_count());

    Ok(())
}
