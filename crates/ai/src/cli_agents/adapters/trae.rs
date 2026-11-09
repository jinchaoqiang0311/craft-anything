//! TraeCode CLI 2.0 (`traecli exec`, task on stdin).
//!
//! `traecli exec` is the non-interactive form (the bare `traecli` starts its TUI). A prompt left
//! out, or given as `-`, comes from stdin, so the task never has to fit in a command line. A run
//! outside a build is held to a read-only sandbox. A headless build has no person to approve work,
//! so it uses `--permission-mode auto` instead: that already grants workspace-write and routes
//! eligible approvals through the CLI's auto-reviewer, and anything it refuses ends the turn rather
//! than waiting for an answer. 0.213.1 refuses a sandbox and a permission-mode override together,
//! so a build passes only the permission mode.
//!
//! Our tool server is added to this run only with `-c mcp_servers.<name>=...` (verified on 0.213.1
//! with `mcp list`); the person's `~/.trae/traecli.toml` is never written. The CLI documents
//! `--json` but not the event shape, so this adapter streams its text output; the event stream is
//! still to be checked after a live signed-in run. Clean room is refused because the CLI reads
//! files itself and there is no verified per-run way to disable those built-in tools.

use super::*;

pub struct TraeCode;

/// A TOML string for a `-c` value: a literal string where possible (Windows paths keep their
/// backslashes), or a basic string with escapes when the value holds a single quote.
fn toml_string(s: &str) -> String {
    if !s.contains('\'') {
        format!("'{s}'")
    } else {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

/// `mcp_servers.<name>={command=...,args=[...]}` for this run, parsed as TOML by `-c`.
fn mcp_override(mcp: &McpCommand) -> String {
    let args = mcp.args.iter().map(|a| toml_string(a)).collect::<Vec<_>>().join(",");
    format!("mcp_servers.{MCP_NAME}={{command={},args=[{args}]}}", toml_string(&mcp.command))
}

/// Model ids from `traecli models --json`: an array of ids, or of objects with an `id` or `model`
/// field. Anything else is left out rather than guessed at.
fn models_in_json(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(text.trim()) else { return Vec::new() };
    let Some(list) = value.as_array() else { return Vec::new() };
    let mut models: Vec<String> = Vec::new();
    for item in list.iter().take(200) {
        let id = item.as_str().or_else(|| item.get("id").and_then(Value::as_str)).or_else(|| item.get("model").and_then(Value::as_str));
        let Some(id) = id.map(str::trim).filter(|id| !id.is_empty() && id.len() <= 80) else { continue };
        if !models.iter().any(|m| m == id) {
            models.push(id.to_string());
        }
    }
    models
}

impl AgentAdapter for TraeCode {
    fn ids(&self) -> &'static [&'static str] {
        &["trae-code", "traecli"]
    }

    fn prompt_on_stdin(&self) -> bool {
        true
    }

    /// There is no separate system-prompt flag in `exec`, so the preamble travels with the task.
    fn stdin_text(&self, preamble: &str, prompt: &str) -> String {
        format!("{preamble}{prompt}")
    }

    fn args(&self, cx: &ArgsCx) -> Vec<String> {
        let mut a = cx.options.args.clone();
        a.extend(["exec".into(), "--color".into(), "never".into(), "--skip-git-repo-check".into()]);
        // `auto` already grants workspace-write, and 0.213.1 refuses a sandbox and a
        // permission-mode override together (`sandbox_mode` and `permission_mode` overrides
        // cannot both be set), so a build passes only the permission mode.
        if cx.builder {
            a.extend(["--permission-mode".into(), "auto".into()]);
        } else {
            a.extend(["--sandbox".into(), "read-only".into()]);
        }
        if let Some(m) = &cx.options.cli_model {
            a.extend(["-m".into(), m.clone()]);
        }
        a.extend(["-c".into(), mcp_override(cx.mcp)]);
        // No positional prompt: stdin carries it.
        a
    }

    /// What `traecli` runs with when the person hasn't chosen: its own default model.
    fn model_in_use(&self, options: Option<&BackendOptions>) -> Option<String> {
        Some(options.and_then(|options| options.cli_model.clone()).unwrap_or_else(|| "TraeCode default".into()))
    }

    /// The models `traecli models --json` lists on this computer. Empty is an honest answer: on
    /// 0.213.1 it is empty while signed out, so no ids are kept in code that the CLI hasn't named.
    fn model_choices(&self, config: &BackendConfig, options: &BackendOptions) -> Result<Vec<String>, ToolError> {
        let program = super::program_of(config, options)?;
        let out = Process::new(program).args(["models", "--json"]).timeout(Duration::from_secs(30)).stdout_cap(1024 * 1024).stderr_cap(64 * 1024).run()?;
        if !out.success {
            return Ok(Vec::new());
        }
        Ok(models_in_json(&out.stdout_text()))
    }

    fn model_choices_note(&self, models: &[String]) -> Option<String> {
        Some(if models.is_empty() {
            "traecli listed no models on this computer (it may need `traecli login`); its default is used unless you type an id.".into()
        } else {
            "Listed by `traecli models` on this computer.".into()
        })
    }

    /// `traecli login status` is the CLI's own answer: it exits zero when an account is signed in
    /// and non-zero otherwise (0.213.1 prints "Not logged in" and exits 1). The exit status is the
    /// structured signal, so no output text is read.
    fn signed_in(&self, program: &str) -> Option<bool> {
        let out = self.process(program).args(["login", "status"]).timeout(Duration::from_secs(10)).stdout_cap(64 * 1024).stderr_cap(64 * 1024).run().ok()?;
        Some(out.success)
    }

    fn sign_in_fix(&self) -> Option<String> {
        Some(
            "Sign in to TraeCode: open a terminal and run `traecli login` (for enterprise SSO, `traecli login --enterprise`), then start the build again."
                .into(),
        )
    }

    /// The tool server is added with `-c`, so a build needs it to be startable.
    fn check_tool_server(&self, stop: &CancelToken) -> Result<(), ToolError> {
        cli_can_start(craft_anything_platform::self_cli_path().as_deref(), stop)
    }

    fn streams_json(&self) -> bool {
        false
    }

    fn required_flags(&self) -> (&'static [&'static str], &'static [&'static str]) {
        (&["--help"], &["exec"])
    }

    /// `traecli "query"` opens its TUI, so a failed flag check keeps this adapter.
    fn plain_fallback(&self) -> bool {
        false
    }

    /// TraeCode reads files on its own and `exec` has no verified per-run way to disable those
    /// built-in tools, so a Clean room run can't keep the original out of reach.
    fn validate_access(&self, access: Access) -> Result<(), ToolError> {
        match access {
            Access::CleanRoomAnalyse | Access::CleanRoomBuild | Access::CleanRoomQuestion => Err(ToolError::invalid(
                "TraeCode reads files with its own built-in tools, which a headless run can't disable, so a Clean room run can't hold it back.",
            )
            .with_remediation("Use a deep run for TraeCode, or choose an agent that can be held to Craft Anything's tools.")),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp() -> McpCommand {
        McpCommand { command: "craft-anything-cli".into(), args: vec!["mcp".into()], notes: None }
    }

    fn args(builder: bool, model: Option<&str>) -> Vec<String> {
        let options = BackendOptions { cli_model: model.map(str::to_string), ..Default::default() };
        build_args(&TraeCode, &options, "make a game", "PRE ", &mcp(), None, if builder { Access::Build } else { Access::Analyse })
    }

    /// `exec` runs non-interactively, a build asks for the auto approval mode (which grants
    /// workspace-write) while analysis is read-only, the server is added with `-c` for this run
    /// only, and the task stays out of argv.
    #[test]
    fn exec_arguments_put_the_task_on_stdin() {
        let a = args(true, None);
        assert_eq!(a[0..2], ["exec", "--color"], "{a:?}");
        // 0.213.1 rejects a sandbox and a permission-mode override together; `auto` already grants
        // workspace-write, so a build passes only the mode.
        assert!(!a.iter().any(|x| x == "--sandbox"), "a build must not also set a sandbox: {a:?}");
        assert_eq!(a[a.iter().position(|x| x == "--permission-mode").expect("--permission-mode") + 1], "auto", "{a:?}");
        assert!(!a.iter().any(|x| x == "--ask-for-approval"), "the documented flag does not exist: {a:?}");
        let i = a.iter().position(|x| x == "-c").expect("-c");
        assert!(a[i + 1].contains("mcp_servers.craft-anything"), "{a:?}");
        assert!(a[i + 1].contains("args=['mcp']"), "{a:?}");
        assert!(!a.iter().any(|x| x.contains("make a game") || x.contains("PRE ")), "{a:?}");

        let analyse = args(false, None);
        assert_eq!(analyse[analyse.iter().position(|x| x == "--sandbox").expect("--sandbox") + 1], "read-only", "{analyse:?}");
        assert!(!analyse.iter().any(|x| x == "--permission-mode"), "no approval mode outside a build: {analyse:?}");
    }

    #[test]
    fn a_chosen_model_is_passed_through() {
        let a = args(false, Some("doubao"));
        let i = a.iter().position(|x| x == "-m").expect("-m");
        assert_eq!(a[i + 1], "doubao");
    }

    /// The preamble and task travel together on stdin.
    #[test]
    fn the_preamble_travels_with_the_task_on_stdin() {
        assert_eq!(TraeCode.stdin_text("Instructions:\n", "what is this?"), "Instructions:\nwhat is this?");
    }

    /// Clean room is refused, since the run isn't handed our tool server.
    #[test]
    fn clean_room_is_refused() {
        for access in [Access::CleanRoomAnalyse, Access::CleanRoomBuild, Access::CleanRoomQuestion] {
            assert!(TraeCode.validate_access(access).is_err(), "{access:?}");
        }
        for access in [Access::Analyse, Access::Build, Access::Question] {
            assert!(TraeCode.validate_access(access).is_ok(), "{access:?}");
        }
    }

    /// The list is read from `traecli models --json`; an empty or unknown answer yields no
    /// choices rather than invented ids.
    #[test]
    fn models_are_read_from_traeclis_own_json() {
        let json = r#"[{"id":"doubao-seed-2.5"}, "trae-pro", {"model":"trae-lite"}, {"name":"no id"}]"#;
        assert_eq!(models_in_json(json), ["doubao-seed-2.5", "trae-pro", "trae-lite"]);
        assert_eq!(models_in_json("[]"), Vec::<String>::new());
        assert_eq!(models_in_json("not json"), Vec::<String>::new());
        assert_eq!(TraeCode.model_in_use(None), Some("TraeCode default".to_string()));
    }

    /// A signed-out person is told the CLI's own login command, including the enterprise form.
    #[test]
    fn a_signed_out_person_is_told_how_to_log_in() {
        let fix = TraeCode.sign_in_fix().expect("a fix");
        assert!(fix.contains("traecli login"), "{fix}");
        assert!(fix.contains("--enterprise"), "{fix}");
    }

    #[test]
    fn registered_not_the_plain_adapter() {
        assert!(AgentAdapters::built_in().for_agent("trae-code").ids().contains(&"trae-code"));
        assert!(AgentAdapters::built_in().for_agent("traecli").ids().contains(&"trae-code"));
        assert_eq!(TraeCode.required_flags(), (&["--help"][..], &["exec"][..]));
        assert!(!TraeCode.plain_fallback());
        assert!(!TraeCode.streams_json());
        assert!(TraeCode.prompt_on_stdin());
        assert!(!TraeCode.resumes());
        assert!(!TraeCode.answers_questions());
    }
}
