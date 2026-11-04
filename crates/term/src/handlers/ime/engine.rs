use language::syntax::ImeSensitiveRegion;
use view::document::Mode;

#[derive(Debug, Clone, Copy)]
pub struct ImeContext {
    pub mode: Mode,
    pub saved_state: Option<bool>,
    pub current_region: Option<ImeSensitiveRegion>,
    /// Cached IME state to avoid frequent system API calls.
    /// This is updated whenever we change IME state, and can be used
    /// to avoid querying the system when the state hasn't changed.
    pub cached_ime_state: Option<bool>,
    /// Cached cursor byte position to avoid redundant region detection.
    /// When cursor position hasn't changed, we can skip region detection.
    pub cached_cursor_byte_pos: Option<usize>,
    /// Cached byte span for the last detected sensitive region (per document version).
    pub cached_region_span: Option<ImeRegionSpan>,
}

impl ImeContext {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            saved_state: None,
            current_region: None,
            cached_ime_state: None,
            cached_cursor_byte_pos: None,
            cached_region_span: None,
        }
    }

    pub fn reset(&mut self, mode: Mode) {
        self.mode = mode;
        self.saved_state = None;
        self.current_region = None;
        // Keep cached_ime_state as it may still be valid
        // Clear cached cursor position on reset
        self.cached_cursor_byte_pos = None;
        self.cached_region_span = None;
    }
}

pub struct ImeEngine<'ctx> {
    ctx: &'ctx mut ImeContext,
}

#[derive(Debug, Clone, Copy)]
pub struct ImeRegionSpan {
    pub doc_version: i32,
    pub start: usize,
    pub end: usize,
    pub region: ImeSensitiveRegion,
}

impl<'ctx> ImeEngine<'ctx> {
    pub fn new(ctx: &'ctx mut ImeContext) -> Self {
        Self { ctx }
    }

    pub fn on_region_change(
        &mut self,
        region: ImeSensitiveRegion,
        current_ime_enabled: bool,
    ) -> Option<bool> {
        if self.ctx.current_region == Some(region) {
            log::debug!("IME engine: region unchanged ({:?}), no action", region);
            return None;
        }

        let prev_region = self.ctx.current_region;
        log::trace!(
            "IME engine: region change from {:?} to {:?}, current_ime={}, saved_state={:?}",
            prev_region,
            region,
            current_ime_enabled,
            self.ctx.saved_state
        );
        self.ctx.current_region = Some(region);

        if is_sensitive(region) {
            // Moving to sensitive region: restore saved state if exists
            if let Some(saved) = self.ctx.saved_state {
                log::trace!(
                    "IME engine: moving to sensitive region, restoring saved state={}",
                    saved
                );
                return desired_state(saved, current_ime_enabled);
            }
            // No saved state: don't enable IME automatically
            // The user needs to manually enable IME first, then it will be restored
            log::trace!("IME engine: moving to sensitive region, no saved state, not enabling IME");
            return None;
        }

        // Non-sensitive region: save state if coming from sensitive region
        if prev_region.is_some_and(is_sensitive) {
            // Coming from sensitive region: save the state
            log::trace!(
                "IME engine: moving from sensitive to non-sensitive, saving state={}",
                current_ime_enabled
            );
            self.ctx.saved_state = Some(current_ime_enabled);
        }
        log::trace!("IME engine: moving to non-sensitive region, disabling IME");
        desired_state(false, current_ime_enabled)
    }

    pub fn on_exit_insert(&mut self, current_ime_enabled: bool) -> Option<bool> {
        // Only save IME state if we're currently in a sensitive region
        // If we're in a non-sensitive region (Code), the IME should be disabled,
        // and we don't want to overwrite the saved state from previous sensitive region visits
        if let Some(region) = self.ctx.current_region {
            if is_sensitive(region) {
                // In sensitive region: save the current IME state
                // This allows us to restore the state when re-entering Insert mode
                // The saved state represents whether IME was enabled when we last left Insert mode
                log::trace!(
                    "IME engine: exiting insert from sensitive region {:?}, IME is enabled={}, updating saved_state",
                    region, current_ime_enabled
                );
                self.ctx.saved_state = Some(current_ime_enabled);
            } else {
                // In non-sensitive region: don't overwrite saved_state
                // The saved_state should still contain the state from the last sensitive region visit
                log::trace!("IME engine: exiting insert from non-sensitive region, keeping saved_state={:?}", self.ctx.saved_state);
            }
        } else {
            // No current region: this should not happen if region detection is working correctly.
            // Don't save state as fallback, as we can't determine if we're in a sensitive region.
            log::warn!(
                "IME engine: exiting insert with no current region, not saving state (region should have been detected)"
            );
        }
        self.ctx.current_region = None;
        self.ctx.mode = Mode::Normal;
        desired_state(false, current_ime_enabled)
    }

    pub fn on_enter_insert(
        &mut self,
        region: ImeSensitiveRegion,
        current_ime_enabled: bool,
    ) -> Option<bool> {
        let prev_saved_state = self.ctx.saved_state;
        self.ctx.mode = Mode::Insert;
        self.ctx.current_region = Some(region);

        if is_sensitive(region) {
            // Restore saved state from previous sensitive region visit
            if let Some(saved) = self.ctx.saved_state {
                log::trace!(
                    "IME engine: entering insert in sensitive region {:?}, saved_state={}, current_ime={}, prev_saved_state={:?}",
                    region, saved, current_ime_enabled, prev_saved_state
                );
                // Always call set_ime_enabled to restore the saved state
                return Some(saved);
            }
            // No saved state: this is the first time entering a sensitive region
            // Don't enable IME automatically - wait for user to manually toggle if needed
            log::trace!(
                "IME engine: first entry to sensitive region {:?}, not enabling IME automatically (no saved state), prev_saved_state={:?}",
                region, prev_saved_state
            );
            return None;
        }

        desired_state(false, current_ime_enabled)
    }
}

/// Check if a region is IME-sensitive.
///
/// IME-sensitive regions are those where IME should be enabled:
/// - String content (excluding leading quotes)
/// - Comment content (excluding header symbols)
/// - Entire file (when syntax parsing is unavailable)
///
/// # Arguments
/// * `region` - The IME sensitive region to check
///
/// # Returns
/// `true` if the region is IME-sensitive, `false` otherwise
pub fn is_sensitive(region: ImeSensitiveRegion) -> bool {
    matches!(
        region,
        ImeSensitiveRegion::StringContent
            | ImeSensitiveRegion::CommentContent
            | ImeSensitiveRegion::EntireFile
    )
}

fn desired_state(target: bool, current: bool) -> Option<bool> {
    if target == current {
        None
    } else {
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_into_code_disables_and_saves_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::CommentContent);
        ctx.saved_state = Some(true);
        let mut engine = ImeEngine::new(&mut ctx);

        let action = engine.on_region_change(ImeSensitiveRegion::Code, true);

        assert_eq!(action, Some(false));
        assert_eq!(ctx.saved_state, Some(true)); // Saved from sensitive region
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::Code));
    }

    #[test]
    fn entering_sensitive_region_restores_saved_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::StringContent);
        ctx.saved_state = Some(true);
        let mut engine = ImeEngine::new(&mut ctx);

        let action = engine.on_region_change(ImeSensitiveRegion::CommentContent, false);

        assert_eq!(action, Some(true));
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
    }

    #[test]
    fn entering_sensitive_region_without_saved_state_keeps_ime_disabled() {
        let mut ctx = ImeContext::new(Mode::Insert);
        let mut engine = ImeEngine::new(&mut ctx);

        // No saved state: don't enable IME automatically
        let action = engine.on_region_change(ImeSensitiveRegion::StringContent, false);

        assert_eq!(action, None); // No action - IME stays disabled
        assert_eq!(ctx.saved_state, None); // State not saved
    }

    #[test]
    fn mode_switch_exit_and_enter_updates_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.saved_state = Some(true);
        ctx.current_region = Some(ImeSensitiveRegion::CommentContent);
        {
            let mut engine = ImeEngine::new(&mut ctx);
            // Exit from sensitive region: should save current state
            let exit_action = engine.on_exit_insert(true);
            assert_eq!(exit_action, Some(false));
        }
        assert_eq!(ctx.mode, Mode::Normal);
        assert_eq!(ctx.saved_state, Some(true));

        {
            let mut engine = ImeEngine::new(&mut ctx);
            let enter_action = engine.on_enter_insert(ImeSensitiveRegion::CommentContent, false);
            assert_eq!(enter_action, Some(true));
        }
        assert_eq!(ctx.mode, Mode::Insert);
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
    }

    #[test]
    fn mode_switch_exit_from_code_preserves_saved_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.saved_state = Some(true); // Saved from previous sensitive region visit
        ctx.current_region = Some(ImeSensitiveRegion::Code); // Currently in code region
        {
            let mut engine = ImeEngine::new(&mut ctx);
            // Exit from non-sensitive region: should NOT overwrite saved_state
            let exit_action = engine.on_exit_insert(false); // IME is disabled in code region
            assert_eq!(
                exit_action, None,
                "IME already disabled in code region, no change needed"
            );
        }
        assert_eq!(ctx.mode, Mode::Normal);
        assert_eq!(ctx.saved_state, Some(true)); // Preserved, not overwritten

        {
            // Enter insert mode in sensitive region: should restore saved state
            let mut engine = ImeEngine::new(&mut ctx);
            let enter_action = engine.on_enter_insert(ImeSensitiveRegion::CommentContent, false);
            assert_eq!(enter_action, Some(true)); // Restored from saved_state
        }
        assert_eq!(ctx.mode, Mode::Insert);
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
    }

    #[test]
    fn moving_from_comment_to_code_and_back_restores_ime() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::CommentContent);
        ctx.saved_state = None;

        // Move from comment to code: IME should be disabled, state saved
        {
            let mut engine = ImeEngine::new(&mut ctx);
            let action1 = engine.on_region_change(ImeSensitiveRegion::Code, true);
            assert_eq!(action1, Some(false));
        }
        assert_eq!(ctx.saved_state, Some(true)); // Saved from sensitive region
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::Code));

        // Move back to comment: IME should be restored
        {
            let mut engine = ImeEngine::new(&mut ctx);
            let action2 = engine.on_region_change(ImeSensitiveRegion::CommentContent, false);
            assert_eq!(action2, Some(true));
        }
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
    }

    #[test]
    fn moving_from_code_to_comment_restores_saved_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::Code);
        ctx.saved_state = Some(false); // Saved from previous sensitive region visit (IME was disabled)

        // Move from code to comment: should restore saved state (false = disabled)
        {
            let mut engine = ImeEngine::new(&mut ctx);
            let action = engine.on_region_change(ImeSensitiveRegion::CommentContent, true);
            assert_eq!(action, Some(false)); // Restore to disabled
        }
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
        assert_eq!(ctx.saved_state, Some(false));
    }

    #[test]
    fn moving_from_code_to_comment_without_saved_state_keeps_ime_disabled() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::Code);
        ctx.saved_state = None; // No saved state from sensitive region

        // Move from code to comment: no saved state, don't enable IME
        {
            let mut engine = ImeEngine::new(&mut ctx);
            let action = engine.on_region_change(ImeSensitiveRegion::CommentContent, false);
            assert_eq!(action, None); // No action - IME stays disabled
        }
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::CommentContent));
        assert_eq!(ctx.saved_state, None); // State not saved
    }

    #[test]
    fn moving_between_sensitive_regions_preserves_state() {
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::CommentContent);
        ctx.saved_state = Some(true);

        // Move from comment to string: should use saved state
        {
            let mut engine = ImeEngine::new(&mut ctx);
            let action = engine.on_region_change(ImeSensitiveRegion::StringContent, false);
            assert_eq!(action, Some(true));
        }
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::StringContent));
        assert_eq!(ctx.saved_state, Some(true));
    }

    #[test]
    fn entire_file_is_sensitive() {
        // EntireFile should be considered a sensitive region
        assert!(is_sensitive(ImeSensitiveRegion::EntireFile));
        assert!(is_sensitive(ImeSensitiveRegion::StringContent));
        assert!(is_sensitive(ImeSensitiveRegion::CommentContent));
        assert!(!is_sensitive(ImeSensitiveRegion::Code));
    }

    #[test]
    fn entire_file_region_change_does_not_disable_ime() {
        // When current_region is EntireFile, `handle_cursor_move` skips processing;
        // the engine should still treat an unchanged region as a no-op.
        let mut ctx = ImeContext::new(Mode::Insert);
        ctx.current_region = Some(ImeSensitiveRegion::EntireFile);
        ctx.saved_state = Some(true);

        // Simulate "moving" within EntireFile (region unchanged)
        let mut engine = ImeEngine::new(&mut ctx);
        let action = engine.on_region_change(ImeSensitiveRegion::EntireFile, true);

        // Region unchanged, no action needed
        assert_eq!(action, None);
        assert_eq!(ctx.current_region, Some(ImeSensitiveRegion::EntireFile));
    }
}
