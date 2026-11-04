//! Terminal input and presentation hooks. Editor services are constructed by `view`.

pub(crate) mod auto_reload;
pub mod completion;
mod diagnostics;
pub(crate) mod hover;
pub mod ime;
pub(crate) mod navigation;
mod prompt;
pub(crate) mod signature_help;
pub(crate) mod workspace_trust;

/// Install presentation hooks in this application's editor hub.
pub fn register_hooks(events: &event::EventHub) {
    completion::register_hooks(events);
    signature_help::register_hooks(events);
    diagnostics::register_hooks(events);
    ime::register_hooks(events);
}

/// Registrations whose callbacks belong to this application's frontend.
pub(crate) fn bind_editor(
    editor: &view::Editor,
    jobs: crate::job::JobSender,
    prompts: crate::compositor::PromptSessions,
) -> event::HookRegistration {
    // The IME cursor-move hook registers permanently on the editor's own hub
    // (it needs the `JobSender` to dispatch debounced work back here), so its
    // registration outlives this call and only `prompt`'s is return-owned.
    ime::bind_editor(editor, jobs.clone());
    prompt::register_hook(editor, jobs, prompts)
}
