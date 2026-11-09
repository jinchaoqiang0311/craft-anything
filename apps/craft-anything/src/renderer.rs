//! Windows: which way the window draws, and the next one to try after a start that failed (#794).
//!
//! DX12 on the graphics card is the usual way. Some systems refuse it in a way no driver update
//! fixes: there `CreatePipelineState` rejects egui's own pipeline (`egui_pipeline`, "The parameter
//! is incorrect", 0x80070057) on AMD, Intel and NVIDIA cards alike, and on WARP too, while Vulkan or
//! OpenGL draw fine. Software (WARP) alone was the only retry, so such a start failed every time.
//!
//! So the renderers form a chain, DX12 → WARP → Vulkan → OpenGL. Each start writes the renderer it
//! uses here before the window is asked for; a start that never opened its window (see
//! [`crate::start_guard`]) makes the next one take the next renderer, and a renderer that worked is
//! kept. That covers a driver that crashes the process natively as well as a wgpu error the app
//! catches. Vulkan and OpenGL come last because loading their drivers is what crashed some Intel
//! systems before any window (#196, #394): they are reached only once DX12 has failed.
//!
//! DX12 has no graphics card on some systems: it offers only Windows' software renderer (WARP),
//! which draws on the CPU. There a frame took about 480 ms on the machine this was measured on,
//! against about 55 ms through OpenGL on the same card, so a start that would land on WARP asks
//! which backend has a card instead and takes it once. The card is looked for in the chain's own
//! order, and DX12 is asked first: on a system with a DX12 card no Vulkan or OpenGL driver is ever
//! loaded. A card start that never opens its window goes back to WARP for this version.
//!
//! The choice is kept for this app version only: an update starts from DX12 again, so a system that
//! was fixed meanwhile is back on the fast way. `WGPU_BACKEND` and `CRAFT_ANYTHING_RENDERER` still
//! decide when set, and then nothing here is used.

use std::path::{Path, PathBuf};

/// Beside the settings, with `desktop.log` and the start mark.
const FILE: &str = "renderer";
/// The file's own format, so a later one can read it or start afresh.
const FORMAT: u32 = 1;

/// A way to draw the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    /// DX12 on the graphics card.
    Dx12,
    /// DX12 with Windows' software renderer.
    Warp,
    Vulkan,
    Gl,
}

/// The order they are tried in.
const CHAIN: [Renderer; 4] = [Renderer::Dx12, Renderer::Warp, Renderer::Vulkan, Renderer::Gl];

impl Renderer {
    pub fn name(self) -> &'static str {
        match self {
            Renderer::Dx12 => "dx12",
            Renderer::Warp => "warp",
            Renderer::Vulkan => "vulkan",
            Renderer::Gl => "gl",
        }
    }

    /// What people read in the start-failure dialog.
    #[cfg_attr(not(windows), expect(dead_code, reason = "only Windows picks its renderer"))]
    pub fn title(self) -> &'static str {
        match self {
            Renderer::Dx12 => "DirectX 12",
            Renderer::Warp => "Windows' software renderer",
            Renderer::Vulkan => "Vulkan",
            Renderer::Gl => "OpenGL",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        CHAIN.into_iter().find(|r| r.name() == name)
    }

    /// The one to try after this one failed; `None` after the last.
    pub fn next(self) -> Option<Self> {
        CHAIN.iter().position(|r| *r == self).and_then(|i| CHAIN.get(i.saturating_add(1))).copied()
    }

    /// Whether this one draws with WARP.
    #[cfg_attr(not(windows), expect(dead_code, reason = "only Windows picks its renderer"))]
    pub fn software(self) -> bool {
        self == Renderer::Warp
    }

    #[cfg(windows)]
    pub fn backends(self) -> eframe::wgpu::Backends {
        use eframe::wgpu::Backends;
        match self {
            Renderer::Dx12 | Renderer::Warp => Backends::DX12,
            Renderer::Vulkan => Backends::VULKAN,
            Renderer::Gl => Backends::GL,
        }
    }
}

/// The renderer for this start: the remembered one, or the next after it when the last start never
/// opened its window. After the last there's nothing new to try, so it starts over from DX12 (the
/// driver may have been updated meanwhile) rather than staying on one that failed.
pub fn choose(remembered: Option<Renderer>, last_start_failed: bool) -> Renderer {
    let current = remembered.unwrap_or(Renderer::Dx12);
    if last_start_failed { current.next().unwrap_or(Renderer::Dx12) } else { current }
}

/// The renderer for this start, as [`choose`] gives it, with one correction: when it would be DX12
/// and DX12 has only Windows' software renderer, a backend that has a graphics card draws instead
/// (`card` says which, if any). OpenGL was the card attempt and never opened its window: the start
/// after that stays on WARP rather than going round the chain to it again. `card` is asked only
/// when it matters, so a start that already knows its renderer loads no other driver.
pub fn decide(remembered: Option<Renderer>, last_start_failed: bool, card: impl FnOnce() -> Option<Renderer>) -> Renderer {
    if last_start_failed && remembered == Some(Renderer::Gl) {
        return Renderer::Warp;
    }
    let chosen = choose(remembered, last_start_failed);
    if !last_start_failed && chosen == Renderer::Dx12 { card().unwrap_or(Renderer::Dx12) } else { chosen }
}

/// The backend that draws this window on a graphics card, in the chain's order, or `None` when only
/// software can draw. DX12 is asked first: a card there means no Vulkan or OpenGL driver is loaded
/// (see the module's note on #196 and #394).
#[cfg(windows)]
pub fn card_renderer() -> Option<Renderer> {
    [Renderer::Dx12, Renderer::Vulkan, Renderer::Gl].into_iter().find(|r| draws_on_a_card(*r))
}

/// Whether that backend offers a device that isn't the software renderer. Any trouble finding out
/// (a driver that won't load, or a panic inside wgpu) means no, so the start keeps the way that
/// works.
#[cfg(windows)]
fn draws_on_a_card(renderer: Renderer) -> bool {
    // wgpu asks the driver here, and a misbehaving one can panic inside it.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let descriptor = eframe::wgpu::InstanceDescriptor { backends: renderer.backends(), ..eframe::wgpu::InstanceDescriptor::new_without_display_handle() };
        let instance = eframe::wgpu::Instance::new(descriptor);
        let adapters = pollster::block_on(instance.enumerate_adapters(renderer.backends()));
        adapters.iter().any(|a| a.get_info().device_type != eframe::wgpu::DeviceType::Cpu)
    }))
    .unwrap_or(false)
}

/// Whether a start that failed before its window should start again at once with the next
/// renderer, and which: only when wgpu said what went wrong (any other early bug isn't the
/// renderer's fault, and the next start moves on anyway), and only while there is a next one, so
/// it never loops.
pub fn relaunch_with(current: Renderer, error: &str) -> Option<Renderer> {
    if error.contains("wgpu") { current.next() } else { None }
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The renderer remembered for this app version, if any. A file from another version, another
/// format or one that can't be read means none.
pub fn remembered(dir: &Path, app_version: &str) -> Option<Renderer> {
    let text = std::fs::read_to_string(file(dir)).ok()?;
    let mut format = None;
    let mut version = None;
    let mut renderer = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("format", v)) => format = v.trim().parse::<u32>().ok(),
            Some(("app", v)) => version = Some(v.trim()),
            Some(("renderer", v)) => renderer = Renderer::from_name(v.trim()),
            _ => {}
        }
    }
    (format == Some(FORMAT) && version == Some(app_version)).then_some(renderer).flatten()
}

/// Remembers the renderer this start uses, before the window is asked for. Best effort: a folder
/// that can't be written to only means no fallback.
pub fn remember(dir: &Path, app_version: &str, renderer: Renderer) {
    let text = format!("format={FORMAT}\napp={app_version}\nrenderer={}\n", renderer.name());
    if std::fs::create_dir_all(dir).is_ok()
        && let Err(e) = craft_anything_platform::fs::write_atomic(&file(dir), text.as_bytes())
    {
        log::warn!("couldn't remember the renderer: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #794: DX12 and WARP both refused egui's pipeline, Vulkan drew. Each failed start moves on one
    /// step, a start that opened its window stays where it is, and after OpenGL it starts over.
    #[test]
    fn each_failed_start_tries_the_next_renderer_and_a_working_one_is_kept() {
        assert_eq!(choose(None, false), Renderer::Dx12, "a first start draws the usual way");
        assert_eq!(choose(None, true), Renderer::Warp, "the old fallback is still the first retry");
        assert_eq!(choose(Some(Renderer::Warp), true), Renderer::Vulkan);
        assert_eq!(choose(Some(Renderer::Vulkan), true), Renderer::Gl);
        assert_eq!(choose(Some(Renderer::Gl), true), Renderer::Dx12, "after the last, start over");
        assert_eq!(choose(Some(Renderer::Vulkan), false), Renderer::Vulkan, "one that worked is kept");
    }

    /// DX12 with only WARP draws on the CPU and is several times slower: a card through another
    /// backend draws instead, once, and a start that never opens its window stays on WARP.
    #[test]
    fn a_card_draws_instead_of_windows_software_renderer() {
        assert_eq!(decide(None, false, || Some(Renderer::Gl)), Renderer::Gl, "DX12 has no card: OpenGL draws");
        assert_eq!(decide(Some(Renderer::Dx12), false, || Some(Renderer::Vulkan)), Renderer::Vulkan);
        assert_eq!(decide(None, false, || None), Renderer::Dx12, "only software on this system: the usual way");
        assert_eq!(decide(None, false, || Some(Renderer::Dx12)), Renderer::Dx12, "a DX12 card is the usual way");
    }

    #[test]
    fn a_card_start_that_never_opened_its_window_stays_on_software() {
        assert_eq!(decide(Some(Renderer::Gl), true, || Some(Renderer::Gl)), Renderer::Warp, "not round to OpenGL again");
        assert_eq!(decide(Some(Renderer::Dx12), true, || Some(Renderer::Gl)), Renderer::Warp, "the chain's next after a failed DX12");
        assert_eq!(decide(Some(Renderer::Warp), true, || Some(Renderer::Gl)), Renderer::Vulkan, "the chain still moves on");
    }

    #[test]
    fn only_a_graphics_error_starts_again_and_never_past_the_last() {
        let text = "internal error before the window was shown: wgpu error: Validation Error\n\nCaused by:\n  In Device::create_render_pipeline, label = 'egui_pipeline'";
        assert_eq!(relaunch_with(Renderer::Dx12, text), Some(Renderer::Warp));
        assert_eq!(relaunch_with(Renderer::Warp, text), Some(Renderer::Vulkan));
        assert_eq!(relaunch_with(Renderer::Gl, text), None, "nothing left: the dialog says why");
        assert_eq!(relaunch_with(Renderer::Dx12, "internal error before the window was shown: settings"), None);
    }

    #[test]
    fn the_choice_is_kept_for_this_version_only() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("config");
        assert_eq!(remembered(&dir, "0.4.0"), None, "nothing yet");
        remember(&dir, "0.4.0", Renderer::Vulkan);
        assert_eq!(remembered(&dir, "0.4.0"), Some(Renderer::Vulkan));
        assert_eq!(remembered(&dir, "0.4.1"), None, "an update starts from DX12 again");
        remember(&dir, "0.4.0", Renderer::Gl);
        assert_eq!(remembered(&dir, "0.4.0"), Some(Renderer::Gl), "the latest is kept");
    }

    #[test]
    fn a_file_we_cannot_read_means_none_and_no_crash() {
        let d = tempfile::tempdir().unwrap();
        for text in ["", "renderer=vulkan", "format=2\napp=0.4.0\nrenderer=vulkan", "format=1\napp=0.4.0\nrenderer=metal", "\u{0}\u{ff}garbage"] {
            std::fs::write(file(d.path()), text).unwrap();
            assert_eq!(remembered(d.path(), "0.4.0"), None, "{text:?}");
        }
        // A folder that is a file: nothing written, nothing remembered.
        let not_a_folder = d.path().join("not-a-folder");
        std::fs::write(&not_a_folder, "").unwrap();
        remember(&not_a_folder, "0.4.0", Renderer::Gl);
        assert_eq!(remembered(&not_a_folder, "0.4.0"), None);
    }

    #[test]
    fn names_round_trip() {
        for r in CHAIN {
            assert_eq!(Renderer::from_name(r.name()), Some(r));
        }
        assert_eq!(Renderer::Gl.next(), None);
    }
}
