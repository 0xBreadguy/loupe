//! The Insights comparison page. It borrows the visit's report while drawing.
use loupe_core::{
    comparison::{self, Comparison, Metrics, Window},
    insights_view::Motion,
    model::State,
    recap,
    terminal::{self, Canvas, Palette},
};

#[derive(Default)]
pub(super) struct CompareView {
    project: Option<String>,
    projects: bool,
    scroll: usize,
    page: usize,
}

impl CompareView {
    pub(super) fn scroll(&mut self, motion: Motion) {
        self.scroll = match motion {
            Motion::Rows(n) => self.scroll.saturating_add_signed(n as isize),
            Motion::Page(n) => self
                .scroll
                .saturating_add_signed(n as isize * self.page.max(1) as isize),
            Motion::Start => 0,
            Motion::End => usize::MAX,
        };
    }

    pub(super) fn toggle(&mut self) {
        self.projects = !self.projects;
        self.scroll = 0;
    }

    pub(super) fn step_project(&mut self, report: &Comparison, direction: isize) {
        let current = self
            .project
            .as_ref()
            .and_then(|id| report.projects.iter().position(|p| p.id == *id))
            .map_or(0, |i| i + 1);
        let next =
            (current as isize + direction).rem_euclid(report.projects.len() as isize + 1) as usize;
        self.project = next.checked_sub(1).map(|i| report.projects[i].id.clone());
        self.projects = false;
        self.scroll = 0;
    }

    pub(super) fn paint(
        &mut self,
        state: &State,
        size: (usize, usize),
        report: Option<&Comparison>,
        notice: Option<&str>,
        help: bool,
    ) -> Canvas {
        let p = &state.palette;
        let (cols, rows) = size;
        let mut c = Canvas::new(cols, rows);
        let (w, h) = (cols as i32, rows as i32);
        c.fill(0, 0, w, h, p.bg);
        if w < 36 || h < 11 {
            c.text(
                0,
                0,
                &terminal::fit("Enlarge the terminal to compare", cols),
                p.text2,
            );
            return c;
        }
        c.fill(0, 0, w, 1, p.panel);
        c.text_bold(1, 0, "Loupe  ·  INSIGHTS COMPARE", p.bright);
        if state.demo {
            c.right(w - 1, 0, "DEMO", p.dim);
        }
        c.fill(0, h - 1, w, 1, p.panel);
        let keys = if w >= 98 {
            "v insights  Tab models/projects  p/P project  ←/→ period  [/] history  ? help  q quit"
        } else {
            "v back  Tab view  p/P project  [/] history  ? help"
        };
        c.text(1, h - 1, &terminal::fit(keys, cols - 2), p.text2);
        if help {
            for (i, line) in [
                "COMPARE KEYS",
                "Tab           switch model and project tables",
                "p / P         choose the next or previous project",
                "← / →         select a week or month",
                "[ / ]         browse older or newer periods",
                "↑ / ↓         scroll rows   ·   r refresh",
                "v or Esc      return to Insights   ·   ? close help",
            ]
            .iter()
            .enumerate()
            {
                c.text(1, 2 + i as i32, &terminal::fit(line, cols - 2), p.text2);
            }
            return c;
        }
        let Some(report) = report else {
            c.text(
                1,
                2,
                &terminal::fit(notice.unwrap_or("Loading comparison…"), cols - 2),
                if notice.is_some() { p.hot } else { p.dim },
            );
            return c;
        };
        let current = &report.selected;
        let prior = &report.prior;
        let date = |w: &Window| format!("{} → {}", w.from, w.through);
        let dates = format!("{}  ·  prior {}", date(current), date(prior));
        c.text(1, 1, &terminal::fit(&dates, cols - 2), p.text2);
        let project = self
            .project
            .as_ref()
            .and_then(|id| report.projects.iter().find(|p| p.id == *id));
        let name = match (self.projects, project, &self.project) {
            (true, _, _) => "All projects",
            (_, Some(project), _) => &project.name,
            (_, None, Some(_)) => "Selected project · no activity",
            _ => "All projects",
        };
        c.text_bold(1, 2, &terminal::fit(name, cols - 2), p.fg);
        let (selected, baseline) = if self.projects || self.project.is_none() {
            (current.total, prior.total)
        } else {
            project.map_or((Metrics::default(), Metrics::default()), |p| {
                (p.selected, p.prior)
            })
        };
        let all_models = !self.projects && self.project.is_none();
        let current_usage = all_models
            .then_some(current.usage_history.as_ref())
            .flatten();
        let prior_usage = all_models.then_some(prior.usage_history.as_ref()).flatten();
        let tokens = current_usage.map_or(selected.tokens, |usage| usage.tokens);
        let prior_tokens = prior_usage.map_or(baseline.tokens, |usage| usage.tokens);
        let mut total_line = format!(
            "Reported {}    Fresh {}    Cached {}    Output {}",
            comparison::token_text(tokens),
            token_component(tokens.fresh, tokens.turns),
            token_component(tokens.cached, tokens.turns),
            token_component(tokens.output, tokens.turns)
        );
        if tokens.cache_written > 0. {
            total_line.push_str(&format!(
                "    Cache write {}",
                terminal::tokens(tokens.cache_written)
            ));
        }
        c.text(1, 3, &terminal::fit(&total_line, cols - 2), p.text2);
        let prior_line = format!(
            "Prior reported {}   ·   {}",
            comparison::token_text(prior_tokens),
            coverage(current, prior)
        );
        c.text(1, 4, &terminal::fit(&prior_line, cols - 2), p.dim);
        let source = current_usage.map_or("T3 thread totals", comparison::UsageWindow::source);
        let mut scope = source.to_owned();
        if current_usage.is_some() != prior_usage.is_some() {
            scope.push_str(if prior_usage.is_some() {
                " · prior uses Usage history"
            } else {
                " · prior uses thread totals"
            });
        }
        if selected.compactions.count > 0 {
            scope.push_str(" · Compactions ");
            scope.push_str(&selected.compactions.label());
        }
        c.text(
            1,
            5,
            &terminal::fit(notice.unwrap_or(&scope), cols - 2),
            if notice.is_some() { p.warn } else { p.dim },
        );

        let shown_models = all_models.then(|| report.model_totals());
        let table: Vec<_> = if self.projects {
            report
                .projects
                .iter()
                .map(|p| (p.name.as_str(), p.selected, p.prior))
                .collect()
        } else {
            let models = shown_models
                .as_deref()
                .or_else(|| project.map(|p| p.models.as_slice()))
                .unwrap_or_default();
            models
                .iter()
                .map(|m| (m.name.as_str(), m.selected, m.prior))
                .collect()
        };
        let first = 7;
        self.page = (h - first - 2).max(0) as usize;
        self.scroll = self
            .scroll
            .min(table.len().saturating_sub(self.page.max(1)));
        let width = w.min(148) - 2;
        let details = width >= 100;
        let runs = width >= 65;
        let name_w = (width
            - 24
            - if details {
                55
            } else if runs {
                24
            } else {
                0
            })
        .min(44);
        let heading = if self.projects { "PROJECT" } else { "MODEL" };
        c.fill(1, 6, width, 1, p.panel);
        c.text_bold(1, 6, heading, p.text2);
        c.text(name_w, 6, "TOKENS / PRIOR", p.text2);
        if runs {
            c.text(name_w + 24, 6, "DONE / PRIOR", p.text2);
            c.text(name_w + 38, 6, "FAIL / PRIOR", p.text2);
        }
        if details {
            c.text(name_w + 51, 6, "MEDIAN", p.text2);
            c.text(
                name_w + 63,
                6,
                if self.projects {
                    "COMPACTED"
                } else {
                    "TOK/RUN (N)"
                },
                p.text2,
            );
        }
        if table.is_empty() {
            c.text(
                1,
                first,
                &terminal::fit("No recorded activity in either period", cols - 2),
                p.dim,
            );
        }
        for (row, (name, left, right)) in table.iter().skip(self.scroll).take(self.page).enumerate()
        {
            let y = first + row as i32;
            c.text(
                1,
                y,
                &terminal::fit(name, (name_w - 1).max(1) as usize),
                p.fg,
            );
            let values = format!(
                "{} / {}",
                comparison::token_text(left.tokens),
                comparison::token_text(right.tokens)
            );
            c.text(name_w, y, &terminal::fit(&values, 22), p.accent);
            if runs {
                c.text(
                    name_w + 24,
                    y,
                    &terminal::fit(&format!("{} / {}", left.completed, right.completed), 13),
                    p.text2,
                );
                c.text(
                    name_w + 38,
                    y,
                    &terminal::fit(&format!("{} / {}", left.failed, right.failed), 12),
                    if left.failed > 0 { p.warn } else { p.dim },
                );
            }
            if details {
                let median = left
                    .median_ms
                    .map(recap::span_text)
                    .unwrap_or_else(|| "—".into());
                c.text(name_w + 51, y, &terminal::fit(&median, 11), p.text2);
                let rate = if self.projects {
                    let stats = left.compactions;
                    if stats.count == 0 {
                        "—".into()
                    } else if stats.measured + stats.estimated == 0 {
                        format!("{}×", stats.count)
                    } else {
                        let approx = if stats.estimated > 0 { "~" } else { "" };
                        format!(
                            "{}× {approx}{}",
                            stats.count,
                            terminal::tokens(stats.reduced)
                        )
                    }
                } else {
                    left.tokens_per_reported_run
                        .map(|n| format!("{} ({})", terminal::tokens(n), left.reported_runs))
                        .unwrap_or_else(|| "—".into())
                };
                c.text(
                    name_w + 63,
                    y,
                    &terminal::fit(&rate, (width - name_w - 62).max(1) as usize),
                    p.text2,
                );
            }
        }
        if self.scroll + self.page < table.len() {
            c.text(
                1,
                h - 2,
                &format!("↓ {} more", table.len() - self.scroll - self.page),
                p.dim,
            );
        }
        c
    }
}

fn token_component(value: f64, turns: u32) -> String {
    if turns == 0 {
        "—".into()
    } else {
        terminal::tokens(value)
    }
}

fn coverage(current: &Window, prior: &Window) -> String {
    let first = current
        .first_record
        .as_deref()
        .or(prior.first_record.as_deref());
    if let Some(first) = first
        && first > prior.from.as_str()
    {
        return format!("Records begin {first}");
    }
    if current.so_far {
        "Matched elapsed time".into()
    } else {
        "Completed periods".into()
    }
}

pub(super) fn hint(canvas: &mut Canvas, p: &Palette) {
    let Some(y) = canvas.h.checked_sub(1) else {
        return;
    };
    let label = "v Compare";
    let row = &canvas.cells[y * canvas.w..];
    if let Some(x) = row
        .windows(label.len() + 4)
        .position(|gap| gap.iter().all(|cell| cell.ch == ' '))
    {
        canvas.text((x + 2) as i32, y as i32, label, p.dim);
    }
}
