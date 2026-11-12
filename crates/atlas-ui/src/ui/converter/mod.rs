//! One upstream session, disposable wheel nodes, immutable worker commands.
#[cfg(feature = "ui-lab")]
pub(crate) mod crash_lab;
mod geometry;
#[cfg(feature = "conversion-lab")]
pub(crate) mod lab;
mod render;
mod wheel;
mod wheel_view;

use crate::app::App;
use atlas_core::convert::actions::{self, Action, ActionSettings};
use atlas_core::convert::{self, Outcome, Plan, Report, Settings, Tool, Tools};
use atlas_core::ext::{registry::ToolInstallRequest, tool_package};
use atlas_model::app_state::BgResult;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) enum Phase {
    Discovering,
    Choosing,
    Running {
        cancel: Arc<AtomicBool>,
        done: usize,
        total: usize,
        current: PathBuf,
    },
    Finished(Report),
}

/// Which side of the ring is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Convert,
    Actions,
}

/// One Actions-ring sector: a built-in action or an extension command that
/// opted in with `ring = true`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RingAction {
    Native(Action),
    Extension {
        ext_id: String,
        cmd_id: String,
        title: String,
        extension: String,
    },
}

pub(crate) struct Session {
    sources: Vec<PathBuf>,
    tools: Tools,
    catalog: Vec<convert::ConversionOption>,
    mode: Mode,
    /// The user switched sides; rescans keep their choice.
    mode_chosen: bool,
    actions: Vec<RingAction>,
    /// Every discovered engine (the wheel's `tools` are policy-filtered).
    action_tools: Tools,
    action_settings: ActionSettings,
    /// Provider-installed engines admitted into the wheel.
    extra: Vec<Tool>,
    /// Installable engines that would add formats for this selection.
    locked: Vec<Tool>,
    /// Extension ring commands, resolved on the UI lane at discovery time.
    ring_extensions: Vec<RingAction>,
    phase: Phase,
    selected: usize,
    /// Sector under the pointer: previews in the info column, never commits.
    hovered: Option<usize>,
    settings: Settings,
    /// Wheel center in window coordinates; captured from the pointer on the
    /// first frame so the menu opens where the user is looking.
    anchor: Option<gpui::Point<gpui::Pixels>>,
    minimized: bool,
    /// Start of the opening animation.
    opened: std::time::Instant,
    /// The ring lives in its own entity so its animation never re-renders the window.
    wheel: Option<gpui::Entity<wheel_view::WheelView>>,
    discovery: u64,
    installers: BTreeMap<Tool, ToolInstallRequest>,
    disabled_providers: BTreeMap<Tool, String>,
    package: Option<PackageJob>,
    package_error: Option<String>,
    #[cfg(feature = "conversion-lab")]
    wheel_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    tx: flume::Sender<Event>,
    rx: flume::Receiver<Event>,
}

enum Event {
    Discovered {
        revision: u64,
        tools: Tools,
        catalog: Vec<convert::ConversionOption>,
        action_tools: Tools,
        actions: Vec<Action>,
        extra: Vec<Tool>,
        locked: Vec<Tool>,
    },
    PackageProgress(tool_package::Progress),
    PackageFinished(Result<PathBuf, String>),
    Destination(PathBuf),
    Progress {
        done: usize,
        total: usize,
        current: PathBuf,
    },
    Finished(Report),
}

struct PackageJob {
    tool: Tool,
    cancel: Arc<AtomicBool>,
    progress: tool_package::Progress,
}

impl Session {
    pub(crate) fn visible(&self) -> bool {
        !self.minimized
    }
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }
    fn options(&self) -> &[convert::ConversionOption] {
        &self.catalog
    }
    /// Sector count of the side that is showing.
    fn len(&self) -> usize {
        match self.mode {
            Mode::Convert => self.catalog.len(),
            Mode::Actions => self.actions.len(),
        }
    }
    fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.selected = 0;
            self.hovered = None;
            // Fresh entity: per-sector easing state belongs to the other side.
            self.wheel = None;
        }
        self.mode_chosen = true;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(package) = &self.package {
            package.cancel.store(true, Ordering::Relaxed);
        }
        if let Phase::Running { cancel, .. } = &self.phase {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl App {
    pub(crate) fn open_converter(&mut self) {
        if let Some(session) = self.converter.as_mut().filter(|s| s.running()) {
            session.minimized = false;
            return;
        }
        if self
            .state
            .active_tab_data()
            .is_none_or(|tab| tab.location.fs_dir().is_none())
        {
            self.toast_error(rust_i18n::t!("convert_local_only").into_owned());
            return;
        }
        let sources = self.selected_paths();
        self.open_converter_for(sources);
    }

    pub(crate) fn anchor_converter(&mut self, at: gpui::Point<gpui::Pixels>) {
        if let Some(session) = self.converter.as_mut() {
            session.anchor = Some(at);
        }
    }

    pub(crate) fn open_converter_for(&mut self, sources: Vec<PathBuf>) {
        if sources
            .iter()
            .any(|path| atlas_core::vault::live::is_private(path))
        {
            self.toast_error(rust_i18n::t!("vault_conversion_unavailable").to_string());
            return;
        }
        if let Some(session) = self.converter.as_mut().filter(|s| s.running()) {
            session.minimized = false;
            return;
        }
        if sources.is_empty() {
            self.toast_error(rust_i18n::t!("convert_select").into_owned());
            return;
        }
        if !self.allow_conversion_action(sources.len()) {
            return;
        }
        let (tx, rx) = flume::unbounded();
        let ring = &self.state.settings.ring;
        let action_settings = ActionSettings {
            edge: ring.resize_edge.clamp(16, 16384),
            quality: ring.compress_quality.clamp(10, 100),
            output_dir: None,
        };
        let mode = if ring.open_actions {
            Mode::Actions
        } else {
            Mode::Convert
        };
        self.converter = Some(Session {
            sources,
            tools: Tools::default(),
            catalog: Vec::new(),
            mode,
            mode_chosen: false,
            actions: Vec::new(),
            action_tools: Tools::default(),
            action_settings,
            extra: Vec::new(),
            locked: Vec::new(),
            ring_extensions: Vec::new(),
            phase: Phase::Discovering,
            selected: 0,
            hovered: None,
            settings: Settings::default(),
            anchor: None,
            opened: std::time::Instant::now(),
            wheel: None,
            minimized: false,
            discovery: 0,
            installers: BTreeMap::new(),
            disabled_providers: BTreeMap::new(),
            package: None,
            package_error: None,
            #[cfg(feature = "conversion-lab")]
            wheel_bounds: None,
            tx: tx.clone(),
            rx,
        });
        self.refresh_conversion_tools();
    }

    /// Snapshot registry state on the UI lane; filesystem/codecs stay on workers.
    pub(crate) fn refresh_conversion_tools(&mut self) {
        let ring_extensions = self.ring_extension_actions();
        let Some(session) = self.converter.as_mut().filter(|s| !s.running()) else {
            return;
        };
        session.ring_extensions = ring_extensions;
        session.installers = Tool::ALL
            .into_iter()
            .filter_map(|tool| {
                self.ext_registry
                    .install_request_for_tool(tool.executable())
                    .filter(|request| {
                        !request.tool.archive_root.is_empty()
                            && matches!(request.tool.archive_format.as_str(), "zip" | "tgz")
                    })
                    .map(|request| (tool, request))
            })
            .collect();
        session.disabled_providers.clear();
        for provider in self
            .ext_registry
            .extensions
            .iter()
            .filter(|extension| !extension.enabled)
        {
            for spec in &provider.manifest.tools {
                if (spec.os.is_empty() || spec.os == std::env::consts::OS)
                    && (spec.arch.is_empty() || spec.arch == std::env::consts::ARCH)
                {
                    if let Some(tool) = Tool::ALL
                        .into_iter()
                        .find(|tool| tool.executable() == spec.name)
                    {
                        session
                            .disabled_providers
                            .insert(tool, provider.manifest.id.clone());
                    }
                }
            }
        }
        session.discovery += 1;
        let revision = session.discovery;
        let scan_sources = session.sources.clone();
        let tx = session.tx.clone();
        if matches!(session.phase, Phase::Choosing) {
            session.phase = Phase::Discovering;
        }
        let managed = self.ext_registry.managed_tools();
        let installable: Vec<Tool> = session.installers.keys().copied().collect();
        let enabled: Vec<Action> = self
            .state
            .settings
            .ring
            .actions
            .iter()
            .filter_map(|id| Action::from_id(id))
            .collect();
        let wake = self.tx.clone();
        self.rt.spawn_blocking(move || {
            let all = Tools::discover_with(managed.clone());
            let extra: Vec<Tool> = all
                .managed_by(&managed)
                .into_iter()
                .filter(|tool| *tool != Tool::Ghostscript)
                .collect();
            let catalog = convert::wheel_options(&scan_sources, &all, &extra);
            let locked = convert::locked_engines(&scan_sources, &all, &extra, &installable);
            let actions = actions::available(&scan_sources, &all, &enabled);
            let _ = tx.send(Event::Discovered {
                revision,
                tools: all.clone().native_and(&extra),
                catalog,
                action_tools: all,
                actions,
                extra,
                locked,
            });
            let _ = wake.send(BgResult::ConversionWake);
        });
    }

    pub(crate) fn drain_converter(&mut self) {
        let mut reload = false;
        let mut toast = None;
        let mut finished_ok = false;
        if let Some(session) = self.converter.as_mut() {
            while let Ok(event) = session.rx.try_recv() {
                match event {
                    Event::Discovered {
                        revision,
                        tools,
                        catalog,
                        action_tools,
                        actions,
                        extra,
                        locked,
                    } if revision == session.discovery && !session.running() => {
                        let selected = session
                            .catalog
                            .get(session.selected)
                            .map(|option| option.target.extension);
                        let selected_action = session.actions.get(session.selected).cloned();
                        session.tools = tools;
                        session.catalog = catalog;
                        session.action_tools = action_tools;
                        session.extra = extra;
                        session.locked = locked;
                        session.actions = actions.into_iter().map(RingAction::Native).collect();
                        session
                            .actions
                            .extend(session.ring_extensions.iter().cloned());
                        if !session.mode_chosen {
                            // The preferred side, unless it has nothing to offer.
                            session.mode = match session.mode {
                                Mode::Actions if session.actions.is_empty() => Mode::Convert,
                                Mode::Convert
                                    if session.catalog.is_empty()
                                        && !session.actions.is_empty() =>
                                {
                                    Mode::Actions
                                }
                                mode => mode,
                            };
                        }
                        session.selected = match session.mode {
                            Mode::Convert => selected.and_then(|extension| {
                                session
                                    .catalog
                                    .iter()
                                    .position(|option| option.target.extension == extension)
                            }),
                            Mode::Actions => selected_action.and_then(|action| {
                                session.actions.iter().position(|a| *a == action)
                            }),
                        }
                        .unwrap_or(0);
                        if matches!(session.phase, Phase::Discovering) {
                            session.phase = Phase::Choosing;
                        }
                    }
                    Event::Discovered { .. } => {}
                    Event::PackageProgress(progress) => {
                        if let Some(package) = &mut session.package {
                            package.progress = progress;
                        }
                    }
                    Event::PackageFinished(result) => {
                        let cancelled = session
                            .package
                            .take()
                            .is_some_and(|package| package.cancel.load(Ordering::Relaxed));
                        match result {
                            Ok(_) => {
                                session.package_error = None;
                                reload = true;
                            }
                            Err(error) if !cancelled => session.package_error = Some(error),
                            Err(_) => {}
                        }
                    }
                    Event::Destination(path) => session.settings.output_dir = Some(path),
                    Event::Progress {
                        done,
                        total,
                        current,
                    } => {
                        if let Phase::Running {
                            done: completed,
                            total: count,
                            current: path,
                            ..
                        } = &mut session.phase
                        {
                            if done >= *completed {
                                *completed = done;
                                *count = total;
                                *path = current;
                            }
                        }
                    }
                    Event::Finished(report) => {
                        let outputs = report.outputs();
                        let errors = report.errors();
                        let cancelled = report.cancelled();
                        let acting = session.mode == Mode::Actions;
                        if errors.is_empty() && cancelled == 0 {
                            let msg = if outputs.len() == 1 {
                                let name =
                                    outputs[0].file_name().unwrap_or_default().to_string_lossy();
                                if acting {
                                    rust_i18n::t!("ring_toast_done_single", name = name)
                                        .into_owned()
                                } else {
                                    rust_i18n::t!("convert_toast_finished_single", name = name)
                                        .into_owned()
                                }
                            } else if acting {
                                rust_i18n::t!("ring_toast_done_multi", count = outputs.len())
                                    .into_owned()
                            } else {
                                rust_i18n::t!("convert_toast_finished_multi", count = outputs.len())
                                    .into_owned()
                            };
                            toast = Some((atlas_model::overlay::ToastKind::Info, msg));
                            finished_ok = true;
                            break;
                        } else if !errors.is_empty() {
                            let err_str = errors[0].to_string();
                            toast = Some((
                                atlas_model::overlay::ToastKind::Error,
                                rust_i18n::t!("convert_toast_failed", error = err_str).into_owned(),
                            ));
                            session.phase = Phase::Finished(report);
                            session.minimized = false;
                        } else {
                            toast = Some((
                                atlas_model::overlay::ToastKind::Info,
                                rust_i18n::t!("convert_cancelled").into_owned(),
                            ));
                            finished_ok = true;
                            break;
                        }
                    }
                }
            }
        }
        if finished_ok {
            self.converter = None;
        }
        if let Some((kind, msg)) = toast {
            match kind {
                atlas_model::overlay::ToastKind::Error => self.toast_error(msg),
                atlas_model::overlay::ToastKind::Info => self.toast_info(msg),
            }
        }
        if reload {
            self.reload_ext_registry();
        }
    }

    fn install_conversion_engine(&mut self, tool: Tool) {
        let Some(session) = self
            .converter
            .as_mut()
            .filter(|s| !s.running() && s.package.is_none())
        else {
            return;
        };
        let Some(request) = session.installers.get(&tool).cloned() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        session.package = Some(PackageJob {
            tool,
            cancel: cancel.clone(),
            progress: tool_package::Progress::Download {
                bytes: 0,
                total: (request.tool.download_bytes != 0).then_some(request.tool.download_bytes),
            },
        });
        session.package_error = None;
        let tx = session.tx.clone();
        let wake = self.tx.clone();
        self.rt.spawn(async move {
            let events = tx.clone();
            let progress_wake = wake.clone();
            let result = tool_package::install(
                request.provider_dir,
                request.tool,
                cancel,
                move |progress| {
                    let _ = events.send(Event::PackageProgress(progress));
                    let _ = progress_wake.send(BgResult::ConversionWake);
                },
            )
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::PackageFinished(result));
            let _ = wake.send(BgResult::ConversionWake);
        });
    }

    fn activate_selected_conversion_engine(&mut self) {
        let Some(session) = self.converter.as_ref() else {
            return;
        };
        let Some(tool) = session
            .catalog
            .get(session.selected)
            .and_then(|option| option.missing.first())
            .copied()
        else {
            return;
        };
        if let Some(id) = session.disabled_providers.get(&tool).cloned() {
            self.handle_ext_action(crate::app::ext::ExtAction::SetEnabled { id, enabled: true });
        } else {
            self.install_conversion_engine(tool);
        }
    }

    fn cancel_conversion_package(&mut self) {
        if let Some(package) = self.converter.as_ref().and_then(|s| s.package.as_ref()) {
            package.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Extension commands for the current sources, resolved on the UI lane.
    fn ring_extension_actions(&self) -> Vec<RingAction> {
        let Some(session) = self.converter.as_ref() else {
            return Vec::new();
        };
        self.ext_registry
            .ring_commands(&session.sources)
            .into_iter()
            .map(|(ext_id, command)| {
                let extension = self
                    .ext_registry
                    .get(&ext_id)
                    .map(|e| e.manifest.name.clone())
                    .unwrap_or_else(|| ext_id.clone());
                RingAction::Extension {
                    ext_id,
                    cmd_id: command.id,
                    title: command.title,
                    extension,
                }
            })
            .collect()
    }

    fn toggle_ring_mode(&mut self) {
        if let Some(session) = self
            .converter
            .as_mut()
            .filter(|s| matches!(s.phase, Phase::Choosing))
        {
            let next = match session.mode {
                Mode::Convert => Mode::Actions,
                Mode::Actions => Mode::Convert,
            };
            session.set_mode(next);
        }
    }

    fn start_action(&mut self) {
        let Some(session) = self.converter.as_mut() else {
            return;
        };
        if !matches!(session.phase, Phase::Choosing) {
            return;
        }
        let Some(action) = session.actions.get(session.selected).cloned() else {
            return;
        };
        let sources = session.sources.clone();
        let action = match action {
            RingAction::Extension { ext_id, cmd_id, .. } => {
                self.converter = None;
                self.start_ext_command_for(&ext_id, &cmd_id, sources);
                return;
            }
            RingAction::Native(action) => action,
        };
        let mut settings = session.action_settings.clone();
        settings.output_dir = session.settings.output_dir.clone();
        let tools = session.action_tools.clone();
        let tx = session.tx.clone();
        let wake = self.tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        session.phase = Phase::Running {
            cancel: cancel.clone(),
            done: 0,
            total: sources.len(),
            current: sources[0].clone(),
        };
        session.minimized = true;
        let label = render::action_label(action);
        let msg = if sources.len() == 1 {
            let name = sources[0].file_name().unwrap_or_default().to_string_lossy();
            rust_i18n::t!("ring_toast_starting_single", action = label, name = name).into_owned()
        } else {
            rust_i18n::t!(
                "ring_toast_starting_multi",
                action = label,
                count = sources.len()
            )
            .into_owned()
        };
        self.toast_info(msg);
        self.rt.spawn_blocking(move || {
            let progress_tx = tx.clone();
            let progress_wake = wake.clone();
            let report = actions::execute(
                action,
                &sources,
                &settings,
                &tools,
                &cancel,
                |done, total, current| {
                    let _ = progress_tx.send(Event::Progress {
                        done,
                        total,
                        current: current.to_path_buf(),
                    });
                    let _ = progress_wake.send(BgResult::ConversionWake);
                },
            );
            let _ = tx.send(Event::Finished(report));
            let _ = wake.send(BgResult::ConversionWake);
        });
    }

    fn start_conversion(&mut self) {
        if self
            .converter
            .as_ref()
            .is_some_and(|s| s.mode == Mode::Actions)
        {
            self.start_action();
            return;
        }
        let count = self
            .converter
            .as_ref()
            .map_or(0, |session| session.sources.len());
        if !self.allow_conversion_action(count) {
            return;
        }
        let Some(session) = self.converter.as_mut() else {
            return;
        };
        if !matches!(session.phase, Phase::Choosing) {
            return;
        }
        let options = session.options();
        let Some(option) = options.get(session.selected).filter(|o| o.available()) else {
            return;
        };
        let extension = option.target.extension;
        let sources = session.sources.clone();
        let settings = session.settings.clone();
        let tools = session.tools.clone();
        let extra = session.extra.clone();
        let tx = session.tx.clone();
        let wake = self.tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        session.phase = Phase::Running {
            cancel: cancel.clone(),
            done: 0,
            total: sources.len(),
            current: sources[0].clone(),
        };
        session.minimized = true;

        let count = sources.len();
        let msg = if count == 1 {
            let name = sources[0].file_name().unwrap_or_default().to_string_lossy();
            rust_i18n::t!(
                "convert_toast_starting_single",
                name = name,
                ext = extension
            )
            .into_owned()
        } else {
            rust_i18n::t!(
                "convert_toast_starting_multi",
                count = count,
                ext = extension
            )
            .into_owned()
        };
        self.toast_info(msg);
        self.rt.spawn_blocking(move || {
            let report = match Plan::build_wheel(&sources, extension, settings, tools, &extra) {
                Ok(plan) => convert::execute(&plan, &cancel, |done, total, current| {
                    let _ = tx.send(Event::Progress {
                        done,
                        total,
                        current: current.to_path_buf(),
                    });
                    let _ = wake.send(BgResult::ConversionWake);
                }),
                Err(error) => Report {
                    outcomes: vec![Outcome::Failed {
                        source: sources[0].clone(),
                        error: format!("{error:#}"),
                    }],
                },
            };
            let _ = tx.send(Event::Finished(report));
            let _ = wake.send(BgResult::ConversionWake);
        });
    }

    fn dismiss_converter(&mut self) {
        if let Some(session) = self.converter.as_mut().filter(|s| s.running()) {
            session.minimized = true;
        } else {
            self.converter = None;
        }
    }

    fn cancel_conversion(&mut self) {
        if let Some(Session {
            phase: Phase::Running { cancel, .. },
            ..
        }) = &self.converter
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn choose_conversion_directory(&mut self) {
        let Some(session) = &self.converter else {
            return;
        };
        let tx = session.tx.clone();
        let wake = self.tx.clone();
        self.rt.spawn(async move {
            if let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await {
                let _ = tx.send(Event::Destination(folder.path().to_path_buf()));
                let _ = wake.send(BgResult::ConversionWake);
            }
        });
    }
}
