//! CodeBuddy Code (`codebuddy -p --output-format stream-json`).
//!
//! Tencent's CodeBuddy Code follows Claude Code's headless contract: `-p` prints one answer and
//! exits, `--output-format stream-json` writes one JSON event per line, and a prompt given without
//! a positional argument comes from stdin. Its stream has the same `system`/`user`/`assistant`/
//! `result` shapes, so the parser below reads it the same way.
//!
//! The tool server arrives as an inline `--mcp-config` string for this run, with
//! `--strict-mcp-config` so no other registration can add servers, and `--settings` marks the server
//! enabled and allowed. CodeBuddy's print mode does not support `--permission-prompt-tool`
//! (its headless page says so even though the CLI reference lists the flag), so approvals of its
//! built-in tools are granted with `--allowedTools` instead of being routed to the person.
//!
//! Clean room runs keep no built-in tools (`--tools ""`): every read and edit goes through our tool
//! server, which keeps the original out of reach. A deep builder may use its built-in tools, and
//! they are the ones `--allowedTools` names.

use super::*;

/// CodeBuddy's built-in tools a build may use.
const BUILD_TOOLS: &[&str] = &["Bash", "PowerShell", "Read", "Write", "Edit", "MultiEdit", "Glob", "Grep", "TodoWrite", "NotebookEdit"];

/// CodeBuddy's built-in tools a deep analysis may use.
const ANALYSIS_TOOLS: &[&str] = &["Read", "Glob", "Grep"];

/// What the person does about a turn that stopped because nobody is signed in. The login stays
/// in CodeBuddy itself; nothing here is signed in for them.
const SIGN_IN_FIX: &str =
    "Sign in to CodeBuddy: open a terminal and run `codebuddy`, then type /login and finish signing in; start the build again after that.";

/// Why a failed `result` says it failed: its own words or subtype, plus the `errors` list it
/// keeps beside them. In 2.181.0 that list is the only place a sign-in problem is named; the
/// subtype alone ("error_during_execution") says nothing the person can act on.
fn why_words(v: &Value) -> String {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let mut parts = Vec::new();
    let base = if text("result").is_empty() { text("subtype") } else { text("result") };
    if !base.is_empty() {
        parts.push(base);
    }
    for e in v.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        let e = e.trim();
        if !e.is_empty() && !parts.iter().any(|p| p == e) {
            parts.push(e.to_string());
        }
    }
    if parts.is_empty() { "no reason given".into() } else { parts.join(": ") }
}

/// Whether the words ask for a sign-in (CodeBuddy's own "Authentication required. Please use
/// /login", or anything else in the same family).
fn asks_to_sign_in(text: &str) -> bool {
    let m = text.to_ascii_lowercase();
    ["authenticat", "oauth", "log in", "login", "not logged", "api key", "unauthorized"].iter().any(|w| m.contains(w))
}

/// The error shown for a turn that stopped because nobody is signed in.
fn signed_out() -> AgentEvent {
    AgentEvent::Error { message: "CodeBuddy isn't signed in on this computer.".into(), remediation: Some(SIGN_IN_FIX.into()), log: None }
}

/// The model ids CodeBuddy itself names in `--help` ("Currently supported: (a, b, c)"). The list
/// belongs to the installed version, so it is read from there rather than kept in code; nothing
/// beyond what the CLI prints is assumed.
fn models_in_help(help: &str) -> Vec<String> {
    const MARK: &str = "currently supported:";
    let Some(rest) = help.to_ascii_lowercase().find(MARK).and_then(|at| help.get(at + MARK.len()..)) else { return Vec::new() };
    let Some(open) = rest.find('(') else { return Vec::new() };
    let Some(close) = rest.get(open..).and_then(|r| r.find(')')) else { return Vec::new() };
    let mut models: Vec<String> = Vec::new();
    for id in rest.get(open + 1..open + close).unwrap_or_default().split(',').map(str::trim) {
        if id.is_empty() || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-._/:".contains(c)) {
            continue;
        }
        if !models.iter().any(|m| m == id) {
            models.push(id.to_string());
        }
        if models.len() == 200 {
            break;
        }
    }
    models
}

#[derive(Default)]
pub struct CodeBuddy {
    /// The folder chosen for it ([`BackendOptions::config_dir`]); `None`: its usual one.
    folder: Option<std::path::PathBuf>,
}

impl AgentAdapter for CodeBuddy {
    fn ids(&self) -> &'static [&'static str] {
        &["codebuddy", "cbc", "codebuddy-code"]
    }

    fn prompt_on_stdin(&self) -> bool {
        true
    }

    fn args(&self, cx: &ArgsCx) -> Vec<String> {
        let mut a = cx.options.args.clone();
        a.extend(["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into()]);
        a.extend(["--append-system-prompt".into(), cx.preamble.to_string()]);
        if let Some(m) = &cx.options.cli_model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(r) = cx.resume {
            a.extend(["--resume".into(), r.to_string()]);
        }
        let mcp_json = json!({"mcpServers": {MCP_NAME: {"command": cx.mcp.command, "args": cx.mcp.args}}}).to_string();
        let settings = json!({
            "enabledMcpjsonServers": [MCP_NAME],
            "permissions": {"allow": [format!("mcp__{MCP_NAME}")]}
        })
        .to_string();
        a.extend(["--strict-mcp-config".into(), "--mcp-config".into(), mcp_json, "--settings".into(), settings]);

        // A question never reaches this adapter (`answers_questions` is false), so `question` is
        // only read here to keep the tool set honest if that changes.
        if cx.question || (!cx.builder && cx.reads_checked) {
            a.extend(["--tools".into(), String::new()]);
        } else if cx.builder {
            if !cx.reads_checked {
                a.extend(["--tools".into(), "default".into()]);
            } else {
                a.extend(["--tools".into(), String::new()]);
            }
        } else {
            a.extend(["--tools".into(), ANALYSIS_TOOLS.join(",")]);
        }

        // Built-in tools are allowed by name; our server is allowed as a whole. Print mode has no
        // permission-prompt tool, so what is not named here would stop a headless run.
        // `--allowedTools` takes one comma-separated value.
        let mut allowed: Vec<String> = if cx.builder && !cx.reads_checked {
            BUILD_TOOLS.iter().map(|t| t.to_string()).collect()
        } else if !cx.builder && !cx.reads_checked && !cx.question {
            ANALYSIS_TOOLS.iter().map(|t| t.to_string()).collect()
        } else {
            Vec::new()
        };
        allowed.push(format!("mcp__{MCP_NAME}"));
        a.extend(["--allowedTools".into(), allowed.join(",")]);
        // Our own approval tool asks the person; CodeBuddy's matching question tool is refused, so
        // a build cannot quietly ask it instead.
        a.extend(["--disallowedTools".into(), "AskUserQuestion".into()]);
        a
    }

    /// CodeBuddy's stream-json: text and tool calls as they come, the session id from any line, and
    /// from the final `result` its usage and cost, or a failure in plain words.
    fn parse(&self, p: &mut StreamParser, v: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some(id) = v.get("session_id").and_then(Value::as_str) {
            p.session_id = Some(id.to_string());
        }
        match s("type").as_str() {
            "system" if s("subtype") == "init" => {
                let failed = v.get("mcp_servers").and_then(Value::as_array).and_then(|servers| {
                    servers.iter().find(|server| {
                        server.get("name").and_then(Value::as_str) == Some(MCP_NAME)
                            && matches!(server.get("status").and_then(Value::as_str), Some("failed" | "needs-auth"))
                    })
                });
                if let Some(server) = failed {
                    p.failure = Some(TurnFailure::new(FailureKind::ToolsUnavailable));
                    let status = server.get("status").and_then(Value::as_str).unwrap_or_default();
                    out.push(AgentEvent::Error {
                        message: format!("Craft Anything's tools did not start in CodeBuddy (server status: {status})."),
                        remediation: Some(
                            "Check the Craft Anything CLI and the agent failure log, then start a new build attempt after fixing the tool connection.".into(),
                        ),
                        log: None,
                    });
                }
            }
            "assistant" => {
                if let Some(code) = v.get("error").and_then(Value::as_str) {
                    let why = v.pointer("/message/content").map(content_text).filter(|t| !t.is_empty()).unwrap_or_else(|| code.to_string());
                    if asks_to_sign_in(&why) {
                        p.failure = Some(TurnFailure::new(FailureKind::Authentication));
                        return vec![signed_out()];
                    }
                    p.failure = Some(TurnFailure::new(FailureKind::Unknown));
                    return vec![AgentEvent::Error {
                        message: format!("CodeBuddy could not take its turn: {why}"),
                        remediation: Some("Check the chosen model, the account's limits and CodeBuddy's settings, then try again.".into()),
                        log: None,
                    }];
                }
                for b in v.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default() {
                    match b.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "text" => out.push(AgentEvent::TextDelta { text: b.get("text").and_then(Value::as_str).unwrap_or_default().to_string() }),
                        "tool_use" => {
                            let id = b.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                            let name = b.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                            out.push(p.started(&id, &name, b.get("input").cloned().unwrap_or(Value::Null)));
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for b in v.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default() {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default().to_string();
                        let err = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        let text = content_text(b.get("content").unwrap_or(&Value::Null));
                        out.push(p.finished(&id, !err, &text));
                    }
                }
            }
            "result" => {
                p.saw_result = true;
                p.cost_usd = v.get("total_cost_usd").and_then(Value::as_f64);
                if let Some(u) = v.get("usage") {
                    p.input_tokens = u
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        .saturating_add(u.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0));
                    p.output_tokens = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                }
                if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                    let why = why_words(v);
                    if asks_to_sign_in(&why) {
                        p.failure = Some(TurnFailure::new(FailureKind::Authentication));
                        out.push(signed_out());
                    } else {
                        p.failure.get_or_insert(TurnFailure::new(FailureKind::Unknown));
                        out.push(AgentEvent::Error {
                            message: format!("CodeBuddy could not finish the turn: {why}"),
                            remediation: Some("Check the chosen model, the account's limits and CodeBuddy's settings, then try again.".into()),
                            log: None,
                        });
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn required_flags(&self) -> (&'static [&'static str], &'static [&'static str]) {
        (&["--help"], &["--print", "--output-format", "--mcp-config", "--tools"])
    }

    /// `codebuddy "query"` opens the interactive REPL, so a failed flag check keeps this adapter.
    fn plain_fallback(&self) -> bool {
        false
    }

    fn resumes(&self) -> bool {
        true
    }

    /// CodeBuddy has no `--fork-session`, so a question would either change the builder's session
    /// or need one that isn't there. Questions are refused rather than answered on a session the
    /// build is still using.
    fn answers_questions(&self) -> bool {
        false
    }

    /// What CodeBuddy starts with, and what a build uses until the person chooses another.
    fn model_in_use(&self, options: Option<&BackendOptions>) -> Option<String> {
        Some(options.and_then(|options| options.cli_model.clone()).unwrap_or_else(|| "default-model".into()))
    }

    /// The models the installed CodeBuddy names in its own `--help`, which is the only honest
    /// source for a list that changes with the version. Bounded, and run off the UI thread by the
    /// caller; a CLI that names none returns no choices rather than an invented catalogue.
    fn model_choices(&self, config: &BackendConfig, options: &BackendOptions) -> Result<Vec<String>, ToolError> {
        let program = super::program_of(config, options)?;
        let help = self.process(&program).args(["--help"]).timeout(Duration::from_secs(20)).stdout_cap(1024 * 1024).stderr_cap(64 * 1024).run_checked()?;
        Ok(models_in_help(&help.stdout_text()))
    }

    fn model_choices_note(&self, models: &[String]) -> Option<String> {
        Some(if models.is_empty() {
            "CodeBuddy named no models on this computer; its default is used unless you type an id.".into()
        } else {
            "These are the models the installed CodeBuddy itself names (`codebuddy --help`).".into()
        })
    }

    fn check_tool_server(&self, stop: &CancelToken) -> Result<(), ToolError> {
        cli_can_start(craft_anything_platform::self_cli_path().as_deref(), stop)
    }

    /// CodeBuddy's own folder for sign-in, settings and sessions, moved for a chosen one, and
    /// background tasks turned off: `-p` can't follow one and would end the turn with an error.
    fn process(&self, program: &str) -> Process {
        let proc = Process::new(program).env("CODEBUDDY_CODE_DISABLE_BACKGROUND_TASKS", "1");
        match &self.folder {
            Some(dir) => proc.env("CODEBUDDY_CONFIG_DIR", dir),
            None => proc,
        }
    }

    fn folder_var(&self) -> Option<&'static str> {
        Some("CODEBUDDY_CONFIG_DIR")
    }

    fn in_folder(&self, folder: std::path::PathBuf) -> Option<Arc<dyn AgentAdapter>> {
        Some(Arc::new(CodeBuddy { folder: Some(folder) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp() -> McpCommand {
        McpCommand {
            command: r"C:\Program Files\Craft Anything\craft-anything-cli.exe".into(),
            args: vec!["mcp".into(), "--project".into(), r"C:\w\p\investigation.cany".into()],
            notes: None,
        }
    }

    fn args(access: Access, resume: Option<&str>) -> Vec<String> {
        let options = BackendOptions::default();
        build_args(&CodeBuddy::default(), &options, "make a game", "PRE ", &mcp(), resume, access)
    }

    /// Headless arguments, the inline server, and no prompt in argv.
    #[test]
    fn headless_arguments_carry_the_tool_server_inline() {
        let a = args(Access::Build, None);
        assert_eq!(a[0..4], ["-p", "--output-format", "stream-json", "--verbose"], "{a:?}");
        let i = a.iter().position(|x| x == "--mcp-config").expect("an inline mcp config");
        let config: serde_json::Value = serde_json::from_str(&a[i + 1]).expect("valid JSON");
        assert_eq!(config["mcpServers"][MCP_NAME]["command"].as_str(), Some(mcp().command.as_str()));
        assert!(a.iter().any(|x| x == "--strict-mcp-config"), "{a:?}");
        let s = a.iter().position(|x| x == "--settings").expect("settings");
        let settings: serde_json::Value = serde_json::from_str(&a[s + 1]).expect("valid JSON");
        assert_eq!(settings["enabledMcpjsonServers"][0], MCP_NAME);
        assert_eq!(settings["permissions"]["allow"][0], format!("mcp__{MCP_NAME}"));
        assert!(!a.iter().any(|x| x.contains("make a game")), "{a:?}");
    }

    /// A deep builder gets its own tools; a Clean room builder gets none and works through ours.
    #[test]
    fn clean_room_build_keeps_codebuddys_own_tools_out() {
        let deep = args(Access::Build, None);
        let i = deep.iter().position(|x| x == "--tools").expect("--tools");
        assert_eq!(deep[i + 1], "default", "{deep:?}");
        let allowed = deep.iter().position(|x| x == "--allowedTools").expect("--allowedTools");
        assert!(deep[allowed + 1].split(',').any(|t| t == "Bash"), "{deep:?}");

        let clean = args(Access::CleanRoomBuild, None);
        let i = clean.iter().position(|x| x == "--tools").expect("--tools");
        assert_eq!(clean[i + 1], "", "no built-in tools in Clean room: {clean:?}");
        let allowed = clean.iter().position(|x| x == "--allowedTools").expect("--allowedTools");
        assert!(clean[allowed + 1].split(',').any(|t| t == format!("mcp__{MCP_NAME}")), "{clean:?}");
        assert!(!clean[allowed + 1].split(',').any(|t| t == "Bash"), "{clean:?}");
    }

    /// A deep analysis reads with its own tools; a Clean room one only through ours.
    #[test]
    fn analysis_tools_follow_the_access_mode() {
        let deep = args(Access::Analyse, None);
        let i = deep.iter().position(|x| x == "--tools").expect("--tools");
        assert_eq!(deep[i + 1], "Read,Glob,Grep", "{deep:?}");
        let allowed = deep.iter().position(|x| x == "--allowedTools").expect("--allowedTools");
        assert!(deep[allowed + 1].split(',').any(|t| t == "Grep"), "{deep:?}");
        let clean = args(Access::CleanRoomAnalyse, None);
        let i = clean.iter().position(|x| x == "--tools").expect("--tools");
        assert_eq!(clean[i + 1], "", "{clean:?}");
        let allowed = clean.iter().position(|x| x == "--allowedTools").expect("--allowedTools");
        assert_eq!(clean[allowed + 1], format!("mcp__{MCP_NAME}"), "{clean:?}");
    }

    /// A saved session is continued with the documented flag.
    #[test]
    fn a_saved_session_is_resumed() {
        let a = args(Access::Build, Some("abc123"));
        let i = a.iter().position(|x| x == "--resume").expect("--resume");
        assert_eq!(a[i + 1], "abc123");
    }

    /// Text, tool calls and the final usage are read from CodeBuddy's stream.
    #[test]
    fn stream_json_is_parsed() {
        let adapter = Arc::new(CodeBuddy::default());
        let mut p = StreamParser::new(adapter.clone());
        let init = serde_json::json!({"type":"system","subtype":"init","session_id":"s1","mcp_servers":[{"name":MCP_NAME,"status":"connected"}]});
        assert!(adapter.parse(&mut p, &init).is_empty());
        assert_eq!(p.session_id.as_deref(), Some("s1"));
        let assistant = serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_use","id":"t1","name":"Read","input":{}}]}});
        let events = adapter.parse(&mut p, &assistant);
        assert!(matches!(&events[0], AgentEvent::TextDelta { .. }));
        let result = serde_json::json!({"type":"result","is_error":false,"total_cost_usd":0.02,"usage":{"input_tokens":10,"cache_read_input_tokens":2,"output_tokens":4}});
        assert!(adapter.parse(&mut p, &result).is_empty());
        assert!(p.saw_result);
        assert_eq!(p.input_tokens, 12);
        assert_eq!(p.output_tokens, 4);
        assert_eq!(p.cost_usd, Some(0.02));
    }

    /// A failed tool server and a failed result say why instead of ending quietly.
    #[test]
    fn failures_are_reported_in_plain_words() {
        let adapter = Arc::new(CodeBuddy::default());
        let mut p = StreamParser::new(adapter.clone());
        let init = serde_json::json!({"type":"system","subtype":"init","mcp_servers":[{"name":MCP_NAME,"status":"failed"}]});
        let events = adapter.parse(&mut p, &init);
        assert!(matches!(&events[0], AgentEvent::Error { .. }), "{events:?}");
        assert_eq!(p.failure.as_ref().map(|f| f.kind), Some(FailureKind::ToolsUnavailable));
        let result = serde_json::json!({"type":"result","is_error":true,"result":"quota exceeded"});
        let events = adapter.parse(&mut p, &result);
        assert!(matches!(&events[0], AgentEvent::Error { .. }), "{events:?}");
    }

    /// A turn that stops for a missing sign-in says so, with the command that fixes it, and is
    /// never retried. 2.181.0 keeps that text only in `errors`; its subtype says nothing useful.
    #[test]
    fn a_signed_out_turn_says_what_to_do() {
        let adapter = Arc::new(CodeBuddy::default());
        let mut p = StreamParser::new(adapter.clone());
        let result = serde_json::json!({
            "type": "result", "is_error": true, "subtype": "error_during_execution",
            "errors": ["Authentication required. Please use /login command to sign in to your account"]
        });
        let events = adapter.parse(&mut p, &result);
        let AgentEvent::Error { message, remediation, .. } = &events[0] else { panic!("{events:?}") };
        assert_eq!(message, "CodeBuddy isn't signed in on this computer.");
        let fix = remediation.as_deref().unwrap_or_default();
        assert!(fix.contains("/login") && fix.contains("codebuddy"), "{fix}");
        let failure = p.failure.expect("a failure");
        assert_eq!(failure.kind, FailureKind::Authentication);
        assert!(!failure.retryable(), "signing in is something only the person can do");
    }

    /// The model list is what the CLI itself names, never a catalogue kept in code.
    #[test]
    fn supported_models_are_read_from_the_clis_own_list() {
        let help = "  --model <model>  Model for the current session. Please provide the model ID. Currently supported: (default-model, deep-model, deepseek-v4.1-flash, custom-local:qiniu/conatus-free)\n";
        assert_eq!(models_in_help(help), ["default-model", "deep-model", "deepseek-v4.1-flash", "custom-local:qiniu/conatus-free"]);
        assert_eq!(models_in_help("  --model <model>  a model id\n"), Vec::<String>::new());
        assert_eq!(models_in_help("Currently supported: ()"), Vec::<String>::new());
        assert_eq!(CodeBuddy::default().model_in_use(None), Some("default-model".to_string()));
        let options = BackendOptions { cli_model: Some("deep-model".into()), ..Default::default() };
        assert_eq!(CodeBuddy::default().model_in_use(Some(&options)), Some("deep-model".to_string()));
    }

    /// It is registered under both names and is not handed to the plain adapter.
    #[test]
    fn registered_not_the_plain_adapter() {
        assert!(AgentAdapters::built_in().for_agent("codebuddy").ids().contains(&"codebuddy"));
        assert!(AgentAdapters::built_in().for_agent("cbc").ids().contains(&"codebuddy"));
        assert_eq!(CodeBuddy::default().required_flags(), (&["--help"][..], &["--print", "--output-format", "--mcp-config", "--tools"][..]));
        assert!(!CodeBuddy::default().plain_fallback());
        assert!(CodeBuddy::default().streams_json());
        assert!(CodeBuddy::default().resumes());
        assert!(!CodeBuddy::default().answers_questions());
        assert!(CodeBuddy::default().prompt_on_stdin());
    }
}
