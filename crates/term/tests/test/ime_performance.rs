//! Performance tests for IME auto-control functionality

use super::helpers::test_syntax_loader;
use super::ime::{char_offset, ime_test_config, ime_test_env, overwrite_document_text};
use editor_core::Selection;
use indoc::indoc;
use std::time::Duration;
use term::{
    application::TestApplication,
    args::Args,
    handlers::ime::{self, metrics, registry},
};
use view::{document::Mode, editor::Action};
use workspace::trust::WorkspaceTrust;

#[tokio::test(flavor = "multi_thread")]
async fn test_performance_cursor_moves() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;
    let view_id = app.editor.tree.focus();

    // Create a large document with various content
    let doc_id = overwrite_document_text(
        &mut app,
        view_id,
        indoc! {r##"
            fn performance_test() {
                for i in 0..1000 {
                    let msg = "测试消息";
                    println!("处理第 {} 条记录: {}", i, msg);
                    // More code here
                    let value = i * 2;
                    if value > 100 {
                        break;
                    }
                    // Comment section for testing
                    /* Multi-line comment
                       with Chinese content: 你好世界
                       More text here
                    */
                }

                // Function with string
                let greeting = "Hello, 世界!";
                let pattern = r#"
                    Raw string literal
                    with content: 内容测试
                "#;

                let array = [
                    "元素一",
                    "元素二",
                    "元素三",
                ];

                return Ok(());
            }
        "##},
    );

    // Set language for proper syntax highlighting
    app.editor.set_document_language(doc_id, Some("rust"))?;

    app.editor.set_mode(Mode::Insert);
    metrics::reset();

    // Perform rapid cursor movements to stress test performance
    let positions = vec![
        100,  // Start
        500,  // String
        1500, // Code
        2500, // Comment
        3500, // String
        4500, // Code
        5500, // Process loop
        6500, // Inside loop
        7500, // Array
        8500, // Function end
    ];

    // The raw positions outrun this document (it is ~600 chars); out-of-range
    // selections panic deep inside grapheme handling in mitos (helix's copy of
    // this test was never registered, so the overflow was never hit). Clamp
    // each position into the document, keeping the same rapid-move shape.
    let len = view::doc_mut!(app.editor, &doc_id).text().len_chars();
    let positions: Vec<usize> = positions
        .into_iter()
        .map(|pos| pos.min(len.saturating_sub(1)))
        .collect();

    let start_time = std::time::Instant::now();

    for &pos in &positions {
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(pos, pos));

        // This should use cached results when possible
        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }

    let elapsed = start_time.elapsed();
    let snapshot = metrics::snapshot();

    println!("Performance test results:");
    println!("  Total cursor moves: {}", positions.len());
    println!("  Total time: {:?}", elapsed);
    println!(
        "  Average time per move: {:?}",
        elapsed / positions.len() as u32
    );
    println!(
        "  Operations per second: {:.2}",
        positions.len() as f64 / elapsed.as_secs_f64()
    );

    // Verify performance metrics
    assert!(
        elapsed < Duration::from_secs(1),
        "Performance test exceeded 1 second threshold"
    );

    assert_eq!(
        snapshot.cursor_move_calls,
        positions.len() as u64,
        "All cursor moves should be recorded"
    );

    // Cache should have improved performance
    if snapshot.region_detection_calls < positions.len() as u64 {
        println!(
            "  Cache effectiveness: {} cached vs {} total",
            snapshot.region_detection_calls,
            positions.len()
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_memory_usage_with_multiple_views() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;

    // Create multiple views to test memory management
    let mut view_ids = Vec::new();
    for _ in 0..10 {
        let doc_id = app
            .editor
            .tree
            .get(*view_ids.last().unwrap_or(&app.editor.tree.focus()))
            .document_id();
        app.editor.switch(doc_id, Action::VerticalSplit);
        let view_id = app.editor.tree.focus();
        view_ids.push(view_id);

        // Trigger IME context creation
        app.editor.set_mode(Mode::Insert);
        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }

    // Get memory usage metrics
    let metrics = registry::get_registry_metrics();
    println!("Memory usage with 10 views:");
    println!("  Current contexts: {}", metrics.current_contexts());
    println!("  Total created: {}", metrics.total_contexts_created());

    // Memory usage should be reasonable
    assert!(
        metrics.current_contexts() <= 20,
        "Too many active IME contexts"
    );

    // Close half of the views
    for i in (0..5).map(|n| n * 2) {
        if view_ids.len() > i {
            app.editor.close(view_ids[i]);
        }
    }

    // Run cleanup
    registry::prune_orphans(&app.editor);

    // Verify cleanup was effective
    let after_cleanup = registry::get_registry_metrics();
    assert!(
        after_cleanup.current_contexts() < metrics.current_contexts(),
        "Cleanup should have reduced active contexts"
    );

    println!("  After cleanup: {}", after_cleanup.current_contexts());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_large_file_performance() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;
    let view_id = app.editor.tree.focus();

    // Create a very large file
    let large_content = "let str = \"测试内容\"; // String content\n"
        .repeat(5000)
        + "\n\nfn main() {\n    println!(\"Large file test\");\n}";

    let doc_id = overwrite_document_text(&mut app, view_id, &large_content);

    // Set language
    app.editor.set_document_language(doc_id, Some("rust"))?;

    app.editor.set_mode(Mode::Insert);
    metrics::reset();

    // Test cursor movement in large file
    let test_positions = vec![100, 1000, 5000, 10000, 20000, 50000];

    let start_time = std::time::Instant::now();

    for &pos in &test_positions {
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(pos, pos));

        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }

    let elapsed = start_time.elapsed();
    println!("Large file performance test:");
    println!("  File size: {} bytes", large_content.len());
    println!("  Test moves: {}", test_positions.len());
    println!("  Total time: {:?}", elapsed);
    println!(
        "  Time per move: {:?}",
        elapsed / test_positions.len() as u32
    );

    // Performance should remain good even with large files
    assert!(
        elapsed < Duration::from_millis(500),
        "Large file test exceeded 500ms threshold"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cache_effectiveness() -> anyhow::Result<()> {
    let _ime_env = ime_test_env();
    let mut app = TestApplication::new(
        Args::default(),
        ime_test_config(),
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(loader::workspace_trust_dir()),
    )?;
    let view_id = app.editor.tree.focus();

    // Create document with mixed regions
    let doc_id = overwrite_document_text(
        &mut app,
        view_id,
        indoc! {r#"
            fn cache_test() {
                // Code region 1
                let code1 = 42;

                // String region
                let string = "中文测试";

                // Code region 2
                let code2 = 100;

                // Comment
                /* Multi-line comment
                   with Chinese: 测试内容
                */

                // Back to code
                let end = true;
            }
        "#},
    );

    // Set language
    app.editor.set_document_language(doc_id, Some("rust"))?;

    app.editor.set_mode(Mode::Insert);
    metrics::reset();

    // Define test positions for each region type
    let test_cases = vec![
        (25, "code"),    // "let code1 = 42"
        (45, "string"),  // "let string = \"中文测试\""
        (65, "code"),    // "let code2 = 100"
        (85, "comment"), // comment
        (120, "code"),   // "let end = true"
    ];

    // First pass - should trigger cache misses
    for &(pos, _region) in &test_cases {
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(pos, pos));
        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }

    let after_first = metrics::snapshot();

    // Second pass: the span cache guarantees a hit while the cursor MOVES
    // WITHIN the last detected node, and re-detects on entering a different
    // node. (Helix's copy of this test asserted that re-walking the same
    // positions re-detects less, but its span cache has the same one-slot
    // design — that test was never registered in helix, so the assumption was
    // never exercised. Exercise the actual guarantee: small moves that stay
    // inside each position's node, which is what the cache exists for.)
    let len = view::doc_mut!(app.editor, &doc_id).text().len_chars();
    for &(pos, _) in &test_cases {
        // Walk pos -> pos+1 -> pos+2. The +1 move stays inside the node the
        // entry detection cached, so it must be a cache hit; only entering the
        // node costs a detection.
        let nudge1 = (pos + 1).min(len.saturating_sub(1));
        let nudge2 = (pos + 2).min(len.saturating_sub(1));
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(nudge1, nudge1));
        ime::handle_cursor_move(&mut app.editor, view_id)?;
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(nudge2, nudge2));
        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }

    let after_second = metrics::snapshot();

    println!("Cache effectiveness test:");
    println!(
        "  First pass - Region detections: {}",
        after_first.region_detection_calls
    );
    println!(
        "  Second pass - Region detections: {}",
        after_second.region_detection_calls
    );

    // Second pass: 3 moves per position but (at most) 1 detection per node
    // entry — strictly fewer detections than the moves it performs.
    assert!(
        after_second.region_detection_calls < test_cases.len() as u64 * 3,
        "Movement within a node must be served from the span cache"
    );
    assert!(
        after_first.region_cache_hits + after_second.region_cache_hits > 0,
        "The span cache must serve at least the within-node moves"
    );

    // A "code" cursor carries no span (the code region is the whole file, which
    // the cache deliberately does not store), so only the sensitive-region moves
    // can be hits. Measure the hit rate over the sensitive positions only, by
    // moving repeatedly inside one comment span and checking the cache serves
    // the stays.
    let inside = {
        let doc = view::doc_mut!(app.editor, &doc_id);
        char_offset(&doc.text().to_string(), "Code region 1")
    };
    let before = metrics::snapshot();
    for step in 0..5 {
        let pos = inside + step;
        let doc = view::doc_mut!(app.editor, &doc_id);
        doc.set_selection(view_id, Selection::single(pos, pos));
        ime::handle_cursor_move(&mut app.editor, view_id)?;
    }
    let after = metrics::snapshot();
    let hits = after.region_cache_hits - before.region_cache_hits;
    let detections = after.region_detection_calls - before.region_detection_calls;
    println!(
        "  Inside-comment walk: {} hits / {} detections over 5 moves",
        hits, detections
    );
    assert!(
        hits >= 3,
        "Staying inside one comment node must be served from the span cache"
    );
    assert!(
        detections <= 2,
        "Walking within one node must not re-run region detection each step"
    );

    Ok(())
}
