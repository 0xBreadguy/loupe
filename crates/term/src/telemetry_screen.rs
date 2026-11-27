//! Interactive Telemetry screen inside `loupe` (`e` key).
//!
//! Mirrors `StorageScreen` and `InsightsScreen`: borrows `State.telemetry` (or the zero-I/O
//! demo report in `--demo` mode) and renders through `TelemetryView`'s cached body canvas.
//! Pressing `f` hands off to the shared [`T3Handoff`] controller to start a T3 Code thread
//! with Loupe's non-secret findings and fix options.

use crate::t3_handoff::T3Handoff;
use loupe_core::{
    clock,
    handoff::{self, HandoffState},
    host::{self, Host},
    model::State,
    product::{Database, Origin},
    telemetry::{self, Report, Verdict},
    telemetry_view::{Motion, TelemetryView},
    terminal::Canvas,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TelemetrySource {
    Demo,
    Machine,
    Unavailable,
}

impl TelemetrySource {
    pub(super) fn for_database(database: &Database) -> Self {
        match database {
            Database::Demo => Self::Demo,
            Database::Path {
                origin: Origin::Db, ..
            } => Self::Unavailable,
            Database::Path { .. } => Self::Machine,
        }
    }
}

pub(super) struct TelemetryScreen {
    source: TelemetrySource,
    view: Option<TelemetryView>,
    demo_report: Option<Report>,
    demo_revision: u64,
}

impl TelemetryScreen {
    pub(super) fn new(source: TelemetrySource) -> Self {
        Self {
            source,
            view: None,
            demo_report: None,
            demo_revision: 0,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub(super) fn open(&mut self, state: &State, host: &mut Host) {
        self.view = Some(TelemetryView::new());
        match self.source {
            TelemetrySource::Demo => {
                self.demo_report = Some(telemetry::demo::report(state.now));
                self.demo_revision = self.demo_revision.wrapping_add(1);
            }
            TelemetrySource::Machine => {
                if state.telemetry.is_none() {
                    host::force_telemetry_refresh(host);
                }
            }
            TelemetrySource::Unavailable => {}
        }
    }

    pub(super) fn close(&mut self) {
        self.view = None;
    }

    fn active_report<'a>(&'a self, state: &'a State) -> (Option<&'a Report>, u64) {
        match self.source {
            TelemetrySource::Demo => (self.demo_report.as_ref(), self.demo_revision),
            TelemetrySource::Machine => (state.telemetry.as_deref(), state.telemetry_revision),
            TelemetrySource::Unavailable => (None, 0),
        }
    }

    pub(super) fn render(
        &mut self,
        state: &State,
        host: &Host,
        handoff: &HandoffState,
        (cols, rows): (usize, usize),
    ) -> Option<Canvas> {
        if self.view.is_none() {
            return None;
        }
        let unavailable = self.source == TelemetrySource::Unavailable;
        let checking = self.source == TelemetrySource::Machine && host::telemetry_inflight(host);
        let (report_ref, rev) = match self.source {
            TelemetrySource::Demo => (self.demo_report.as_ref(), self.demo_revision),
            TelemetrySource::Machine => (state.telemetry.as_deref(), state.telemetry_revision),
            TelemetrySource::Unavailable => (None, 0),
        };
        let view = self.view.as_mut()?;
        Some(view.render_with_handoff(
            state,
            report_ref,
            rev,
            checking,
            unavailable,
            handoff,
            cols.max(1),
            rows.max(1),
        ))
    }

    /// Returns `true` when the user pressed `q` to quit Loupe. `e`, the screen's own key, is the
    /// presenter's (it goes back to the dashboard unless help is open).
    pub(super) fn key(
        &mut self,
        name: &str,
        state: &State,
        host: &mut Host,
        handoff: &mut T3Handoff,
    ) -> bool {
        let Some(view) = self.view.as_mut() else {
            return false;
        };
        match name {
            "q" => return true,
            "?" | "h" => {
                view.help = !view.help;
                view.scroll = 0;
                return false;
            }
            "escape" => {
                if view.help {
                    view.help = false;
                    view.scroll = 0;
                } else {
                    handoff.dismiss();
                    self.close();
                }
                return false;
            }
            _ if view.help => {
                match name {
                    "down" | "j" => view.scroll_motion(Motion::Rows(1)),
                    "up" | "k" => view.scroll_motion(Motion::Rows(-1)),
                    "pagedown" | "J" => view.scroll_motion(Motion::Page(1)),
                    "pageup" | "K" => view.scroll_motion(Motion::Page(-1)),
                    "home" | "g" => view.scroll_motion(Motion::Start),
                    "end" | "G" => view.scroll_motion(Motion::End),
                    _ => {}
                }
                return false;
            }
            "f" => {
                let Some(report) = self.active_report(state).0.cloned() else {
                    return false;
                };
                if report.verdict == Verdict::Off {
                    return false;
                }
                let (title, prompt) = handoff::telemetry_prompt(&report);
                let base_dir = report.inputs.data_folder.is_absolute().then_some(report.inputs.data_folder);
                handoff.begin_start(
                    "turn off telemetry",
                    title,
                    prompt,
                    None,
                    base_dir,
                    host.db.as_deref(),
                    true,
                    state,
                );
            }
            "tab" | "shift-tab" => {
                view.toggle_tab();
            }
            "r" => match self.source {
                TelemetrySource::Demo => {
                    self.demo_report = Some(telemetry::demo::report(clock::wall_now()));
                    self.demo_revision = self.demo_revision.wrapping_add(1);
                }
                TelemetrySource::Machine => {
                    host::force_telemetry_refresh(host);
                }
                TelemetrySource::Unavailable => {}
            },
            "down" | "j" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    v.move_selection(1, total);
                }
            }
            "up" | "k" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    v.move_selection(-1, total);
                }
            }
            "pagedown" | "J" => view.scroll_motion(Motion::Page(1)),
            "pageup" | "K" => view.scroll_motion(Motion::Page(-1)),
            "home" | "g" => {
                if view.tab == loupe_core::telemetry_view::Tab::Telemetry {
                    view.selected = 0;
                }
                view.scroll_motion(Motion::Start);
            }
            "end" | "G" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    if v.tab == loupe_core::telemetry_view::Tab::Telemetry && total > 0 {
                        v.selected = total - 1;
                    }
                    v.scroll_motion(Motion::End);
                }
            }
            _ => {}
        }
        false
    }

    /// A click: a tab's name in the header shows that view; a row on Telemetry selects it.
    pub(super) fn press(&mut self, x: i32, y: i32) {
        if let Some(view) = self.view.as_mut() {
            view.press(x, y);
        }
    }

    pub(super) fn help_open(&self) -> bool {
        self.view.as_ref().is_some_and(|view| view.help)
    }

    pub(super) fn wheel(
        &mut self,
        down: bool,
        state: &State,
        host: &mut Host,
        handoff: &mut T3Handoff,
    ) {
        self.key(if down { "down" } else { "up" }, state, host, handoff);
    }
}
