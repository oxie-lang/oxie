//! Starts the Oxie language server, `oxie lsp`, for Zed.

use zed_extension_api::{self as zed, serde_json, settings::LspSettings, LanguageServerId, Result};

struct OxieExtension;

impl zed::Extension for OxieExtension {
    fn new() -> Self {
        OxieExtension
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        // "lsp": { "oxie": { "binary": { "path", "arguments", "env" } } } in Zed's settings
        // overrides the command; otherwise `oxie` is found on the worktree's PATH.
        let binary = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.binary);
        let args = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_else(|| vec!["lsp".to_string()]);
        let mut env = worktree.shell_env();
        if let Some(extra) = binary.as_ref().and_then(|binary| binary.env.clone()) {
            env.extend(extra);
        }
        let command = match binary.and_then(|binary| binary.path) {
            Some(path) => path,
            None => worktree.which("oxie").ok_or_else(|| {
                "oxie is not on PATH. Install it with `brew install oxie-lang/tap/oxie` or \
                 `cargo install oxie`, or set lsp.oxie.binary.path in Zed's settings."
                    .to_string()
            })?,
        };
        Ok(zed::Command { command, args, env })
    }

    // "lsp": { "oxie": { "initialization_options": { "cargoCheck": false } } } turns off
    // `cargo check` on save.
    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        Ok(LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.initialization_options))
    }
}

zed::register_extension!(OxieExtension);
