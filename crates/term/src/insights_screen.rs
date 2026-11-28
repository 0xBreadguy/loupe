//! Terminal key routing and borrowed report presentation for one Insights visit.

use std::{io, path::Path, sync::Arc};

use loupe_core::{
    clock,
    handoff::{self, HandoffState},
    insights_view::{self as view, Body, Click, InsightsView, Motion, Tab},
    model::State,
    platform::local_time,
    product::{self, Database, Origin},
    recap::Which,
    recap_view::Source,
    terminal::Canvas,
};
use loupe_term::{
    Encoder,
    insights_data::{InsightsData, ReadError, Report, Selection, Snapshot, Value},
    platform::{self, Wake},
};
use crate::comparison_screen::{self, CompareView};

#[derive(Default)]
pub(super) struct InsightsScreen {
    data: InsightsData,
    database: Option<Database>,
    view: Option<InsightsView>,
    help: bool,
    staged: bool,
    revision: u64,
    kept_revision: u64,
    notice: Option<(String, Vec<String>)>,
    compare: CompareView,
    return_to: Option<Selection>,
}

/// Product provenance is cheap to retain on Live. Its path stays in Host.
#[derive(Clone, Copy)]
pub(super) enum InsightsSource {
    Demo,
    Records(Origin),
}

impl From<&Database> for InsightsSource {
    fn from(database: &Database) -> Self {
        match database {
            Database::Demo => Self::Demo,
            Database::Path { origin, .. } => Self::Records(*origin),
        }
    }
}

pub(super) enum Effect {
    Stay,
    Paint,
    Back,
    OptimizeInT3,
    Quit,
}

impl InsightsScreen {
    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub(super) fn open(&mut self) {
        self.data.open(clock::wall_now());
        self.view = Some(InsightsView::new());
        self.staged = true;
    }

    pub(super) fn close(&mut self) {
        self.data.close();
        self.database = None;
        self.view = None;
        self.help = false;
        self.staged = false;
        self.notice = None;
        self.compare = CompareView::default();
        self.return_to = None;
    }

    pub(super) fn help_open(&self) -> bool {
        self.help
    }

    /// A key on Insights. `i`, the screen's own key, and the theme and calm keys are the
    /// presenter's.
    pub(super) fn key(&mut self, name: &str, source: InsightsSource) -> Effect {
        let Some(selected) = self.data.selection() else { return Effect::Stay };
        match name {
            "q" => return Effect::Quit,
            "escape" if self.help => {
                self.help = false;
                return Effect::Paint;
            }
            "escape" if !matches!(selected.report, Report::Compare(_)) => return Effect::Back,
            "?" | "h" => {
                self.help = !self.help;
                return Effect::Paint;
            }
            // While help is open every other key goes to it, and it has none.
            _ if self.help => return Effect::Stay,
            "r" => {
                if self
                    .data
                    .outcome()
                    .is_some_and(|snapshot| snapshot.outcome.is_ok())
                {
                    self.kept_revision = self.revision;
                }
                self.data.refresh(clock::wall_now());
                self.staged = true;
                self.notice = None;
                return Effect::Paint;
            }
            "f" => {
                return if self.active_handoff_prompt().is_some() { Effect::OptimizeInT3 } else { Effect::Stay };
            }
            _ => {}
        }
        if let Report::Compare(older) = selected.report {
            let mut next = selected;
            match name {
                "v" | "escape" => {
                    next = self.return_to.take().unwrap_or(Selection::ENTRY);
                }
                "[" => next.report = Report::Compare(older.saturating_add(1)),
                "]" => next.report = Report::Compare(older.saturating_sub(1)),
                "left" | "right" => {
                    next.period = match (selected.period, name) {
                        (Which::Last7Days, "left") | (Which::ThisWeek, "left") => Which::Last7Days,
                        (Which::Last7Days, _) => Which::ThisWeek,
                        (period, "left") => view::step_period(period, -1),
                        (period, _) => view::step_period(period, 1),
                    };
                    next.report = Report::Compare(0);
                }
                "p" | "P" => {
                    if let Some(Value::Compare(report)) = self.data.visible()
                        .and_then(|s| s.outcome.as_ref().ok()).map(|item| &item.value)
                    {
                        self.compare.step_project(report, if name == "p" { 1 } else { -1 });
                        return Effect::Paint;
                    }
                }
                "tab" | "shift-tab" => {
                    self.compare.toggle();
                    return Effect::Paint;
                }
                _ => {
                    if let Some(motion) = motion(name) {
                        self.compare.scroll(motion);
                        return Effect::Paint;
                    }
                }
            }
            return self.select(selected, next, source);
        }
        if name == "v" {
            self.return_to = Some(selected);
            return self.select(selected, Selection { report: Report::Compare(0), period: Which::Last7Days }, source);
        }
        if let Some(motion) = motion(name) {
            let Some(view) = self.view.as_mut() else { return Effect::Stay };
            view.scroll(motion);
            return Effect::Paint;
        }
        // Tab and the arrows change what is read; they need a selection to change.
        match next_selection(selected, name) {
            Some(next) => self.select(selected, next, source),
            None => Effect::Stay,
        }
    }

    /// A click: a report's tab name, or a period, on the last frame. Nothing while help is open.
    pub(super) fn press(&mut self, x: i32, y: i32, source: InsightsSource) -> Effect {
        if self.help {
            return Effect::Stay;
        }
        let (Some(view), Some(selected)) = (self.view.as_ref(), self.data.selection()) else {
            return Effect::Stay;
        };
        if matches!(selected.report, Report::Compare(_)) {
            return Effect::Stay;
        }
        let mut next = selected;
        match view.click(x, y) {
            Some(Click::Tab(tab)) => {
                next.report = match tab {
                    Tab::Recap => Report::Recap,
                    Tab::Tools => Report::Tools,
                };
            }
            Some(Click::Period(period)) => next.period = period,
            Some(Click::Step(delta)) => next.period = view::step_period(selected.period, delta),
            None => return Effect::Stay,
        }
        self.select(selected, next, source)
    }

    fn select(&mut self, selected: Selection, next: Selection, source: InsightsSource) -> Effect {
        if next == selected || !self.data.select(next, clock::wall_now()) {
            return Effect::Stay;
        }
        if next.period != selected.period {
            self.view.as_mut().unwrap().scroll(Motion::Start);
        }
        if matches!(next.report, Report::Compare(_)) {
            self.compare.scroll(Motion::Start);
        }
        self.staged = self.data.loading_since().is_some();
        self.completed(source);
        Effect::Paint
    }

    /// Build a non-secret optimization handoff `(action_label, title, prompt)` for the currently
    /// visible Insights report (Tools or Recap), or `None` while loading / empty.
    pub(super) fn active_handoff_prompt(&self) -> Option<(&'static str, String, String)> {
        let visible = self.data.visible()?;
        let ok = visible.outcome.as_ref().ok()?;
        match &ok.value {
            Value::Tools(report) if !report.is_empty() => {
                let (title, prompt) = handoff::tools_prompt(report);
                Some(("optimize slow & failing tools", title, prompt))
            }
            Value::Recap(report) if !report.is_empty() => {
                let (title, prompt) = handoff::recap_prompt(report);
                Some(("optimize workflow from Recap", title, prompt))
            }
            _ => None,
        }
    }

    pub(super) fn poll(&mut self, source: InsightsSource) -> bool {
        if !self.data.poll() {
            return false;
        }
        self.completed(source);
        true
    }

    /// Called after writing the newly staged frame to the terminal. A start
    /// failure has no worker to wake the presenter, so ask for a paint now.
    fn dispatch(&mut self, source: InsightsSource, path: Option<&Path>, wake: &Arc<Wake>) -> bool {
        if !std::mem::take(&mut self.staged) {
            return false;
        }
        let database = self.database.get_or_insert_with(|| match source {
            InsightsSource::Demo => Database::Demo,
            InsightsSource::Records(origin) => Database::Path {
                path: path.expect("the product database is in Host").to_path_buf(),
                origin,
            },
        });
        let wake = Arc::clone(wake);
        if self.data.dispatch(database, move || wake.waker().wake())
            && self.data.loading_since().is_none()
        {
            self.completed(source);
            return true;
        }
        false
    }

    fn completed(&mut self, source: InsightsSource) {
        self.revision += 1;
        self.notice = self.data.outcome().and_then(|snapshot| {
            snapshot
                .outcome
                .as_ref()
                .err()
                .map(|error| notice(error, source))
        });
    }

    fn frame(
        &mut self,
        state: &State,
        source: InsightsSource,
        (cols, rows): (usize, usize),
    ) -> &Canvas {
        let selection = self.data.selection().unwrap();
        let started = self.data.loading_since();
        let outcome = self.data.outcome();
        let visible = self.data.visible();
        let report = visible.and_then(borrow_report);
        let body = if started.is_some() {
            Body::Loading
        } else if let Some((message, tips)) = &self.notice {
            Body::Unreadable { message, tips }
        } else {
            Body::Report(report.expect("a finished read has a report or an error"))
        };
        let previous = if matches!(body, Body::Report(_)) {
            None
        } else {
            visible.zip(report).map(|(snapshot, report)| view::Kept {
                report,
                read_at: snapshot.finished_at,
                revision: self.kept_revision,
            })
        };

        self.view.as_mut().unwrap().paint(&view::Input {
            state,
            palette: &state.palette,
            local: &local_time,
            tab: match selection.report {
                Report::Recap => Tab::Recap,
                Report::Tools => Tab::Tools,
                Report::Compare(_) => unreachable!("Compare has its own canvas"),
            },
            requested: selection.period,
            source: match source {
                InsightsSource::Demo => Source::Demo,
                InsightsSource::Records(_) => Source::Records,
            },
            body,
            previous,
            started,
            read_at: outcome.map(|snapshot| snapshot.finished_at),
            help: self.help,
            explicit_db: matches!(source, InsightsSource::Records(Origin::Db)),
            revision: self.revision,
            cols,
            rows,
        })
    }

    pub(super) fn present_key(
        &mut self,
        state: &State,
        source: InsightsSource,
        path: Option<&Path>,
        size: (usize, usize),
        handoff: &HandoffState,
        encoder: &mut Encoder,
        full: &mut bool,
        wake: &Arc<Wake>,
    ) -> io::Result<()> {
        self.present_key_with(
            state,
            source,
            path,
            size,
            handoff,
            encoder,
            full,
            wake,
            &mut write_frame,
        )
    }

    /// [`InsightsScreen::present_key`] with each frame handed to `sink` (the stream check hashes
    /// it; the dashboard writes it).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn present_key_with(
        &mut self,
        state: &State,
        source: InsightsSource,
        path: Option<&Path>,
        size: (usize, usize),
        handoff: &HandoffState,
        encoder: &mut Encoder,
        full: &mut bool,
        wake: &Arc<Wake>,
        sink: &mut FrameSink<'_>,
    ) -> io::Result<()> {
        // write_stdout writes directly to the tty before dispatch, even on demo.
        self.paint_with(state, source, size, handoff, encoder, full, sink)?;
        if self.dispatch(source, path, wake) {
            self.paint_with(state, source, size, handoff, encoder, full, sink)?;
        }
        Ok(())
    }

    pub(super) fn paint(
        &mut self,
        state: &State,
        source: InsightsSource,
        size: (usize, usize),
        handoff: &HandoffState,
        encoder: &mut Encoder,
        full: &mut bool,
    ) -> io::Result<()> {
        self.paint_with(state, source, size, handoff, encoder, full, &mut write_frame)
    }

    /// [`InsightsScreen::paint`] with the canvas, its bytes and whether it was a full repaint
    /// handed to `sink`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_with(
        &mut self,
        state: &State,
        source: InsightsSource,
        size: (usize, usize),
        handoff: &HandoffState,
        encoder: &mut Encoder,
        full: &mut bool,
        sink: &mut FrameSink<'_>,
    ) -> io::Result<()> {
        let was_full = *full;
        if matches!(self.data.selection().map(|s| s.report), Some(Report::Compare(_))) {
            let report = self.data.visible().and_then(|s| s.outcome.as_ref().ok())
                .and_then(|item| match &item.value { Value::Compare(value) => Some(value), _ => None });
            let notice = self.notice.as_ref().map(|(message, _)| message.as_str());
            let canvas = self.compare.paint(state, size, report, notice, self.help);
            let bytes = encoder.encode(&canvas, was_full);
            *full = false;
            return sink(&canvas, &bytes, was_full);
        }
        let tab = self
            .data
            .selection()
            .map(|sel| match sel.report {
                Report::Recap => Tab::Recap,
                Report::Tools => Tab::Tools,
                Report::Compare(_) => unreachable!("Compare has its own canvas"),
            })
            .unwrap_or(Tab::Recap);
        let help = self.help;
        let actionable = self.active_handoff_prompt().is_some();
        let canvas = self.frame(state, source, size);
        let show_compare = state.themes && !help && handoff.is_idle();
        if help || actionable || !handoff.is_idle() || show_compare {
            let mut decorated = canvas.clone();
            handoff::decorate_insights_canvas(
                &mut decorated,
                tab,
                help,
                actionable,
                handoff,
                &state.palette,
            );
            if show_compare {
                comparison_screen::hint(&mut decorated, &state.palette);
            }
            let bytes = encoder.encode(&decorated, was_full);
            *full = false;
            return sink(&decorated, &bytes, was_full);
        }
        let bytes = encoder.encode(canvas, was_full);
        *full = false;
        sink(canvas, &bytes, was_full)
    }

    /// Whether a read is running (the stream check waits for it instead of for a wake).
    #[cfg(feature = "frame-stream")]
    pub(super) fn loading(&self) -> bool {
        self.data.loading_since().is_some()
    }
}

pub(super) type FrameSink<'a> = dyn FnMut(&Canvas, &[u8], bool) -> io::Result<()> + 'a;

fn write_frame(_: &Canvas, bytes: &[u8], _: bool) -> io::Result<()> {
    if !bytes.is_empty() {
        platform::write_stdout(bytes)?;
    }
    Ok(())
}

fn borrow_report(snapshot: &Snapshot) -> Option<view::Report<'_>> {
    // The typed value contains the returned period and exact dates, including fallback.
    Some(match &snapshot.outcome.as_ref().ok()?.value {
        Value::Recap(value) => view::Report::Recap(value),
        Value::Tools(value) => view::Report::Tools(value),
        Value::Compare(_) => return None,
    })
}

fn next_selection(mut selection: Selection, key: &str) -> Option<Selection> {
    // The requested period is the cursor, even when the report fell forward.
    match key {
        "tab" | "shift-tab" => {
            selection.report = match selection.report {
                Report::Recap => Report::Tools,
                Report::Tools => Report::Recap,
                Report::Compare(_) => return None,
            };
        }
        "left" => selection.period = view::step_period(selection.period, -1),
        "right" => selection.period = view::step_period(selection.period, 1),
        _ => return None,
    }
    Some(selection)
}

fn motion(key: &str) -> Option<Motion> {
    Some(match key {
        "up" | "k" => Motion::Rows(-1),
        "down" | "j" => Motion::Rows(1),
        "pageup" | "K" => Motion::Page(-1),
        "pagedown" | "J" => Motion::Page(1),
        "home" | "g" => Motion::Start,
        "end" | "G" => Motion::End,
        _ => return None,
    })
}

fn notice(error: &ReadError, source: InsightsSource) -> (String, Vec<String>) {
    let (ReadError::Source(error), InsightsSource::Records(origin)) = (error, source) else {
        return (error.to_string(), Vec::new());
    };
    let advice = product::advice(error, origin);
    let mut lines = advice.lines();
    let first = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches("loupe: ");
    let mut message: String = first
        .chars()
        .take(1)
        .flat_map(char::to_uppercase)
        .chain(first.chars().skip(1))
        .collect();
    if !message.ends_with('.') {
        message.push('.');
    }
    let home = loupe_core::platform::home_dir();
    let home = home
        .as_ref()
        .and_then(|path| path.to_str())
        .filter(|path| path.len() > 1);
    let shorten = |line: &str| match home {
        Some(home) => line.replace(home, "~"),
        None => line.to_owned(),
    };
    let tips = lines
        .map(str::trim)
        .filter(|line| !line.starts_with("To try the dashboard without T3 data:"))
        .map(shorten)
        .collect();
    (shorten(&message), tips)
}
