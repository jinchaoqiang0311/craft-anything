//! One-click sign-in for a coding agent: a click opens a terminal window of its own and starts
//! the agent's sign-in command there. Read-only for this computer (nothing is stored here); the
//! window keeps running after Craft Anything closes, and Settings → AI picks up the new state on
//! its next refresh.

use crate::cli_agents::{KNOWN, KnownAgent, SignInCommand};
use craft_anything_engine::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_evidence::{ErrorCode, ToolError};
use craft_anything_platform::process::Launch;
use craft_anything_platform::{fs, which};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SignInParams {
    /// The agent's id in the known list, e.g. `claude-code` or `kimi-code`.
    pub agent: String,
    /// The program to start; the agent's usual program when missing.
    #[serde(default)]
    pub program: Option<String>,
}

pub fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "agent_sign_in",
        title: "Sign in to a coding agent",
        summary: "Open a terminal window of its own and start the sign-in command of the coding agent `agent` (from the detected agents; `program` overrides the usual program). The window outlives Craft Anything; Settings → AI shows the new sign-in state on its next refresh. Nothing is stored on this computer by this tool.",
        beginner: "Opens a terminal where you can sign in to your agent.",
        toolset: Toolset::Setup,
        effects: Effects::EXECUTES_FOREIGN_CODE.union(Effects::LAUNCHES_PROCESS),
        requires: Requires::Nothing,
        input_schema: schema_of::<SignInParams>,
        run: agent_sign_in,
    }]
}

fn agent_sign_in(_ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SignInParams = parse_params(params)?;
    let (agent, program, command) = plan(&p.agent, p.program.as_deref())?;
    let (terminal, pid) = open_window(agent, &program, command)?;
    Ok(ToolOutput::Value(json!({
        "agent": agent.id,
        "program": program.display().to_string(),
        "terminal": terminal,
        "pid": pid,
        "tip": command.tip,
    })))
}

/// Windows Terminal first (a real tab with scrollback), then cmd's own console window; both keep
/// the window after Craft Anything closes.
#[cfg(windows)]
fn open_window(agent: &'static KnownAgent, program: &Path, command: SignInCommand) -> Result<(&'static str, u32), ToolError> {
    let script = std::env::temp_dir().join(format!("craft-sign-in-{}.cmd", agent.id));
    fs::write_atomic(&script, script_text(agent, program, &command).as_bytes())?;
    match which::which("wt") {
        Some(wt) => {
            let pid = Launch::new(wt)
                .args(["new-tab", "cmd.exe", "/k"])
                .arg(&script)
                .label("Windows Terminal")
                .remediation("Install Windows Terminal, or run the sign-in command in a terminal yourself.")
                .detach()?;
            Ok(("windows-terminal", pid))
        }
        None => {
            let pid = Launch::new("cmd.exe").arg("/k").arg(&script).label("cmd").remediation("Run the sign-in command in a terminal yourself.").detach()?;
            Ok(("cmd", pid))
        }
    }
}

/// 待确认: the terminal programs and their flags on macOS and Linux aren't checked, so this isn't
/// guessed; the person gets the exact command to run instead.
#[cfg(not(windows))]
fn open_window(agent: &'static KnownAgent, program: &Path, command: SignInCommand) -> Result<(&'static str, u32), ToolError> {
    Err(ToolError::new(ErrorCode::FeatureMissing, "Opening a sign-in window is only supported on Windows for now.")
        .with_remediation(manual_line(agent, program, command)))
}

/// What a click should run, worked out before anything starts (so a refused one never opens a
/// window).
fn plan(agent_id: &str, program: Option<&str>) -> Result<(&'static KnownAgent, PathBuf, SignInCommand), ToolError> {
    let agent = KNOWN.iter().find(|a| a.id == agent_id).ok_or_else(|| {
        let offered: Vec<&str> = KNOWN.iter().filter(|a| a.sign_in_command.is_some()).map(|a| a.id).collect();
        ToolError::new(ErrorCode::BadInput, format!("No coding agent called \"{agent_id}\".")).with_remediation(format!("Use one of: {}.", offered.join(", ")))
    })?;
    let command = agent.sign_in_command.ok_or_else(|| {
        ToolError::new(ErrorCode::FeatureMissing, format!("{} has no confirmed sign-in command yet.", agent.display))
            .with_remediation(format!("Open a terminal and start {}; its own login steps aren't confirmed here.", agent.display))
    })?;
    let chosen = match program.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => crate::cli_agents::find_agent_program(agent.id).ok_or_else(|| {
            ToolError::new(ErrorCode::EngineMissing, format!("{} isn't installed under a known name.", agent.display))
                .with_remediation(format!("Install {}, or choose its program in Settings → AI.", agent.display))
        })?,
    };
    Ok((agent, chosen, command))
}

/// Start the command in a terminal yourself (the remediation where no window can be opened).
#[cfg(not(windows))]
fn manual_line(agent: &KnownAgent, program: &Path, command: SignInCommand) -> String {
    let mut line = format!("\"{}\"", program.display());
    for arg in command.args {
        line.push(' ');
        line.push_str(arg);
    }
    let tip = command.tip.map(|t| format!(" {t}")).unwrap_or_default();
    format!("Start {line} in a terminal, then sign in.{tip} (agent: {}).", agent.display)
}

/// The batch file a window runs: a title, the tip (when the person must type something), then
/// the agent's sign-in command.
fn script_text(agent: &KnownAgent, program: &Path, command: &SignInCommand) -> String {
    let mut s = String::from("@echo off\r\n");
    s.push_str(&format!("title Sign in to {}\r\n", agent.display));
    if let Some(tip) = command.tip {
        s.push_str(&format!("echo {tip}\r\n"));
    }
    s.push('"');
    s.push_str(&program.display().to_string());
    s.push('"');
    for arg in command.args {
        s.push(' ');
        s.push_str(arg);
    }
    s.push_str("\r\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_without_a_confirmed_login_command_is_refused_before_anything_starts() {
        let err = plan("qoder-cli", Some("C:\\nowhere\\qoder.exe")).unwrap_err();
        assert_eq!(err.code, ErrorCode::FeatureMissing);
        assert!(err.remediation.unwrap_or_default().contains("Qoder CLI"), "says what to run instead");
    }

    #[test]
    fn an_unknown_agent_lists_the_ones_that_can_sign_in() {
        let err = plan("nope", None).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadInput);
        assert!(err.remediation.unwrap_or_default().contains("claude-code"));
    }

    #[test]
    fn the_script_runs_the_chosen_program_and_shows_the_tip() {
        let codebuddy = KNOWN.iter().find(|a| a.id == "codebuddy").unwrap();
        let text = script_text(codebuddy, Path::new(r"C:\Program Files\CodeBuddy\codebuddy.exe"), &codebuddy.sign_in_command.unwrap());
        assert!(text.starts_with("@echo off\r\n"), "{text}");
        assert!(text.contains("title Sign in to CodeBuddy"));
        assert!(text.contains("echo Type /login and press Enter."));
        assert!(text.contains(r#""C:\Program Files\CodeBuddy\codebuddy.exe""#));
        let claude = KNOWN.iter().find(|a| a.id == "claude-code").unwrap();
        let text = script_text(claude, Path::new("claude"), &claude.sign_in_command.unwrap());
        assert!(text.contains("\"claude\" auth login\r\n"), "runs `claude auth login`: {text}");
        assert!(!text.contains("echo Type"), "Claude needs no typed step: {text}");
    }
}
