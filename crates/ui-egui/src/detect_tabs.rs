//! Splitting what Detect found into the two things a person is choosing between: an API provider
//! (a key, billed per token) and a harness (a coding agent already installed). Without the split,
//! eight more providers push the harnesses off the screen.

use craft_anything_ai::Detected;

/// Which half of the detected list is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetectTab {
    #[default]
    Providers,
    Harnesses,
}

/// The tab a found backend belongs to: an agent program is a harness, everything else answers over
/// HTTP and is a provider (a local model server included).
pub fn tab_of(d: &Detected) -> DetectTab {
    if d.suggestion.kind.is_harness() { DetectTab::Harnesses } else { DetectTab::Providers }
}

/// How many of each were found, for the tab labels.
pub fn counts(detected: &[Detected]) -> (usize, usize) {
    detected.iter().fold((0, 0), |(p, h), d| match tab_of(d) {
        DetectTab::Providers => (p + 1, h),
        DetectTab::Harnesses => (p, h + 1),
    })
}

/// Which tab to show. A tab the person clicked stays, even when it is empty: that is how its "nothing
/// of this kind was found" line is read. Only the automatic first choice prefers the tab that has
/// something, so adding the only provider never opens on an empty screen.
pub fn resolve(chosen: Option<DetectTab>, providers: usize, harnesses: usize) -> DetectTab {
    match chosen {
        Some(tab) => tab,
        None if providers == 0 && harnesses > 0 => DetectTab::Harnesses,
        None => DetectTab::Providers,
    }
}

/// Draw the two tabs and return the one that is showing. An empty tab is dimmed, not disabled: it
/// still opens, and says what is missing.
pub fn bar(ui: &mut egui::Ui, chosen: &mut Option<DetectTab>, detected: &[Detected]) -> DetectTab {
    let (providers, harnesses) = counts(detected);
    let showing = resolve(*chosen, providers, harnesses);
    ui.horizontal_wrapped(|ui| {
        for (tab, label, count) in [(DetectTab::Providers, "Providers", providers), (DetectTab::Harnesses, "Harnesses", harnesses)] {
            let mut text = egui::RichText::new(format!("{} ({count})", crate::i18n::tr(label)));
            if count == 0 {
                text = text.weak();
            }
            if ui.selectable_label(showing == tab, text).clicked() {
                *chosen = Some(tab);
            }
        }
    });
    showing
}

/// Whether a found agent can be chosen to build and read. The rest (Kimi Code, Cursor CLI, Qoder
/// CLI for now) are shown so they're found and their sign-in state read, but adding one would only
/// make a backend that can't work, so no button offers it.
pub fn runnable(d: &Detected) -> bool {
    d.agent.as_ref().is_none_or(|a| craft_anything_ai::cli_agents::can_run(&a.id))
}

/// A found agent's sign-in state, version and program, from its typed [`AgentInstall`], so the
/// card never reshapes data into a sentence somewhere else. Returns whether the person clicked
/// "Sign in" (a click, never automatic: the login tool runs only from here).
pub fn agent_detail(ui: &mut egui::Ui, agent: &craft_anything_ai::cli_agents::AgentInstall) -> bool {
    use craft_anything_ai::cli_agents::AgentStatus;
    let t = crate::theme::Tokens::get(ui.ctx());
    let (status, color) = match agent.status {
        AgentStatus::Ready => (crate::i18n::tr("Ready"), t.success),
        AgentStatus::NeedsSignIn => (crate::i18n::tr("Needs sign-in"), t.warning),
        AgentStatus::Unverified => (crate::i18n::tr("Sign-in not checked"), t.text_faint),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(status).color(color).size(crate::theme::CAPTION));
        if agent.stale {
            let when = agent
                .checked_at
                .map(|t| {
                    let minutes = t.elapsed().as_secs() / 60;
                    if minutes == 0 { "just now".to_string() } else { format!("{minutes} min ago") }
                })
                .unwrap_or_else(|| "earlier".to_string());
            ui.label(
                egui::RichText::new(crate::i18n::trf("Couldn't check just now; last known {when}.", &[("when", when.as_str())]))
                    .color(t.warning)
                    .size(crate::theme::CAPTION),
            );
        }
        if let Some(version) = &agent.version {
            ui.label(egui::RichText::new(format!("v{version}")).color(t.text_faint).size(crate::theme::CAPTION));
        }
        ui.label(egui::RichText::new(agent.program.display().to_string()).color(t.text_faint).size(crate::theme::CAPTION));
    });
    let mut clicked = false;
    if !craft_anything_ai::cli_agents::can_run(&agent.id) {
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "Can't build or read yet: this version finds it and shows whether it's signed in, but it can't be chosen as a backend.",
            ))
            .color(t.text_dim)
            .size(crate::theme::CAPTION),
        );
    }
    if agent.status == AgentStatus::NeedsSignIn {
        if craft_anything_ai::cli_agents::can_sign_in(&agent.id) {
            clicked = ui.link(crate::i18n::tr("Sign in")).clicked();
        } else {
            ui.label(egui::RichText::new(crate::i18n::tr("Sign in to it once in a terminal, then look again.")).color(t.text_dim).size(crate::theme::CAPTION));
        }
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_ai::{BackendConfig, BackendKind, Detected};

    fn detected(kind: BackendKind) -> Detected {
        let suggestion = BackendConfig { name: "X".into(), kind, model: "m".into(), base_url: None, ssh_host: None, enabled: true };
        Detected { suggestion, reason: String::new(), needs_key: false, agent: None }
    }

    fn found_agent(id: &str) -> Detected {
        let agent = craft_anything_ai::cli_agents::AgentInstall {
            id: id.into(),
            display: id.into(),
            program: "prog".into(),
            source: craft_anything_ai::cli_agents::InstallSource::Path,
            version: None,
            status: craft_anything_ai::cli_agents::AgentStatus::Unverified,
            sign_in: craft_anything_ai::cli_agents::SignInState::Unknown,
            checked_at: None,
            stale: false,
        };
        Detected {
            suggestion: BackendConfig { name: id.into(), kind: BackendKind::AgentCli, model: id.into(), base_url: None, ssh_host: None, enabled: true },
            reason: String::new(),
            needs_key: false,
            agent: Some(agent),
        }
    }

    /// Only agents a run adapter drives may be added: the status-only ones get no button.
    #[test]
    fn only_agents_that_can_run_are_offered_as_backends() {
        assert!(runnable(&found_agent("claude-code")));
        assert!(!runnable(&found_agent("kimi-code")));
        assert!(!runnable(&found_agent("cursor-cli")));
        assert!(!runnable(&found_agent("qoder-cli")));
        assert!(runnable(&detected(BackendKind::Anthropic)), "a provider isn't an agent at all");
    }

    /// An agent app is a harness; an API backend and a local server are providers.
    #[test]
    fn the_two_kinds_land_on_their_own_tab() {
        assert_eq!(tab_of(&detected(BackendKind::AgentCli)), DetectTab::Harnesses);
        assert_eq!(tab_of(&detected(BackendKind::Openai)), DetectTab::Providers);
        assert_eq!(tab_of(&detected(BackendKind::OpenaiCompatible)), DetectTab::Providers);
        let all = [detected(BackendKind::AgentCli), detected(BackendKind::Anthropic), detected(BackendKind::Gemini)];
        assert_eq!(counts(&all), (2, 1));
    }

    /// The first choice prefers the tab with something; a click is always honoured, so an empty tab
    /// can show what it is missing.
    #[test]
    fn the_default_gives_way_but_a_click_does_not() {
        assert_eq!(resolve(None, 0, 3), DetectTab::Harnesses, "only harnesses were found");
        assert_eq!(resolve(None, 2, 0), DetectTab::Providers);
        assert_eq!(resolve(None, 0, 0), DetectTab::Providers, "nothing at all: the first tab");
        assert_eq!(resolve(Some(DetectTab::Harnesses), 2, 0), DetectTab::Harnesses, "a clicked empty tab stays");
        assert_eq!(resolve(Some(DetectTab::Providers), 0, 3), DetectTab::Providers, "a clicked empty tab stays");
        assert_eq!(resolve(Some(DetectTab::Harnesses), 2, 3), DetectTab::Harnesses);
    }
}
