//! Storage's standalone cell grid and local controls. Effects are returned to the host.

use crate::model::State;
use crate::storage_view::data::{Entry, Inventory, Scan, Sizes, ThreadState};
use crate::terminal::{Canvas, Palette};
use std::cmp::Reverse;
use std::collections::HashSet;

pub mod adapter;
pub mod data;
mod draw;

/// The terminal owns these effects. Every other change stays inside the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Back,
    Rescan,
    OpenInT3(String),
    CleanInT3,
    Quit,
}

#[derive(Debug, Clone, Default)]
pub struct StorageView {
    selected: Option<RowId>,
    index: usize,
    first: usize,
    folded: HashSet<GroupId>,
    help: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GroupId {
    Project(String),
    SettledOrGone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowId {
    Group(GroupId),
    Entry(String),
    Other,
}

struct Group<'a> {
    id: GroupId,
    title: &'a str,
    entries: Vec<&'a Entry>,
    sizes: Sizes,
    folded: bool,
}

impl<'a> Group<'a> {
    fn new(
        id: GroupId,
        title: &'a str,
        entries: &'a [Entry],
        scanning: bool,
        folded: &HashSet<GroupId>,
    ) -> Self {
        let mut entries: Vec<_> = entries.iter().collect();
        if !scanning {
            entries.sort_by_key(|entry| Reverse(entry.sizes.footprint()));
        }
        Self {
            folded: folded.contains(&id),
            id,
            title,
            sizes: Sizes::group(&entries, scanning),
            entries,
        }
    }
}

enum Row<'a> {
    Group(Group<'a>),
    Entry { entry: &'a Entry, last: bool },
    Other(Sizes),
}

impl Row<'_> {
    fn id(&self) -> RowId {
        match self {
            Self::Group(group) => RowId::Group(group.id.clone()),
            Self::Entry { entry, .. } => RowId::Entry(entry.id.clone()),
            Self::Other(_) => RowId::Other,
        }
    }

    fn has_id(&self, id: &RowId) -> bool {
        match (self, id) {
            (Self::Group(group), RowId::Group(id)) => &group.id == id,
            (Self::Entry { entry, .. }, RowId::Entry(id)) => &entry.id == id,
            (Self::Other(_), RowId::Other) => true,
            _ => false,
        }
    }

    fn sizes(&self) -> Sizes {
        match self {
            Self::Group(group) => group.sizes,
            Self::Entry { entry, .. } => entry.sizes,
            Self::Other(sizes) => *sizes,
        }
    }

    fn open_target(&self) -> Option<&str> {
        match self {
            Self::Entry { entry, .. } if entry.state != ThreadState::Gone => entry
                .open_target
                .as_deref()
                .filter(|target| !target.is_empty()),
            _ => None,
        }
    }

    fn gone(&self) -> bool {
        matches!(self, Self::Entry { entry, .. } if entry.state == ThreadState::Gone)
    }
}

struct Rows<'a> {
    items: Vec<Row<'a>>,
    largest: u128,
}

impl<'a> Rows<'a> {
    fn new(inventory: &'a Inventory, folded: &HashSet<GroupId>) -> Self {
        let mut rows = Self {
            items: Vec::new(),
            largest: 0,
        };
        if matches!(inventory.scan, Scan::Unavailable) {
            return rows;
        }

        let scanning = inventory.scan.scanning();
        let mut projects: Vec<_> = inventory
            .projects
            .iter()
            .filter(|project| !project.threads.is_empty())
            .map(|project| {
                Group::new(
                    GroupId::Project(project.id.clone()),
                    &project.title,
                    &project.threads,
                    scanning,
                    folded,
                )
            })
            .collect();
        if !scanning {
            projects.sort_by_key(|group| Reverse(group.sizes.footprint()));
        }
        if !inventory.settled_or_gone.is_empty() {
            projects.push(Group::new(
                GroupId::SettledOrGone,
                "Settled or gone",
                &inventory.settled_or_gone,
                scanning,
                folded,
            ));
        }
        for group in projects {
            rows.push_group(group);
        }
        if let Some(sizes) = inventory.displayed_other().0 {
            rows.largest = rows.largest.max(sizes.footprint().unwrap_or_default());
            rows.items.push(Row::Other(sizes));
        }
        rows
    }

    fn push_group(&mut self, group: Group<'a>) {
        self.largest = self
            .largest
            .max(group.sizes.footprint().unwrap_or_default());
        let start = self.items.len();
        if !group.folded {
            let len = group.entries.len();
            self.items.extend(
                group
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, &entry)| Row::Entry {
                        entry,
                        last: i + 1 == len,
                    }),
            );
        }
        self.items.insert(start, Row::Group(group));
    }
}

impl StorageView {
    /// Builds the view for the current viewport. The dashboard is used only for its header.
    /// Use the same dimensions and inventory when routing input and drawing a frame.
    pub fn render(
        &mut self,
        inventory: &Inventory,
        dashboard: &State,
        w: usize,
        h: usize,
        palette: &Palette,
    ) -> Canvas {
        let layout = draw::Layout::new(inventory, w, h);
        let rows = Rows::new(inventory, &self.folded);
        self.reconcile(&rows, layout.lines());
        draw::render(self, &rows, inventory, dashboard, layout, palette)
    }

    /// Returns a host effect only for an action currently available in the view.
    /// Keys use the terminal parser's existing names (for example, `"enter"`).
    pub fn key(&mut self, key: &str, inventory: &Inventory, w: usize, h: usize) -> Option<Effect> {
        match key {
            "q" => return Some(Effect::Quit),
            "s" => return Some(Effect::Back),
            "?" | "h" => {
                if draw::Layout::fits_size(w, h) {
                    self.help = !self.help;
                }
                return None;
            }
            "escape" if self.help => {
                self.help = false;
                return None;
            }
            "escape" => return Some(Effect::Back),
            _ => {}
        }

        let layout = draw::Layout::new(inventory, w, h);
        if self.help || !layout.fits() {
            return None;
        }
        if key == "r" {
            return (!inventory.scan.scanning()).then_some(Effect::Rescan);
        }

        let rows = Rows::new(inventory, &self.folded);
        let lines = layout.lines();
        self.reconcile(&rows, lines);
        let row = rows.items.get(self.index)?;
        let page = Self::capacity(rows.items.len(), lines).max(1);
        let to = match key {
            "down" | "j" => self.index + 1,
            "up" | "k" => self.index.saturating_sub(1),
            "pagedown" | "J" => self.index.saturating_add(page),
            "pageup" | "K" => self.index.saturating_sub(page),
            "home" | "g" => 0,
            "end" | "G" => rows.items.len() - 1,
            "space" | "enter" if matches!(row, Row::Group(_)) => {
                if let Row::Group(group) = row {
                    if !self.folded.remove(&group.id) {
                        self.folded.insert(group.id.clone());
                    }
                }
                return None;
            }
            "enter" | "o" => return row.open_target().map(|id| Effect::OpenInT3(id.to_owned())),
            "f" => return Some(Effect::CleanInT3),
            _ => return None,
        };
        self.select(&rows, to, lines);
        None
    }

    /// Build a non-secret cleanup context for the currently selected Storage row.
    /// Returns `(context, preferred_project_id)`.
    pub fn selected_context(
        &self,
        inventory: &Inventory,
        w: usize,
        h: usize,
    ) -> Option<(crate::handoff::StorageContext, Option<String>)> {
        let layout = draw::Layout::new(inventory, w, h);
        if !layout.fits() {
            return None;
        }
        let rows = Rows::new(inventory, &self.folded);
        let row = rows.items.get(self.index.min(rows.items.len().saturating_sub(1)))?;
        let (target, entries, preferred_project): (
            crate::handoff::StorageTarget,
            Vec<&Entry>,
            Option<String>,
        ) = match row {
            Row::Group(group) => match &group.id {
                GroupId::Project(id) => (
                    crate::handoff::StorageTarget::Project {
                        id: id.clone(),
                        title: group.title.to_owned(),
                        thread_count: group.entries.len(),
                        sizes: group.sizes,
                    },
                    group.entries.clone(),
                    Some(id.clone()),
                ),
                GroupId::SettledOrGone => (
                    crate::handoff::StorageTarget::SettledOrGone {
                        count: group.entries.len(),
                        sizes: group.sizes,
                    },
                    group.entries.clone(),
                    None,
                ),
            },
            Row::Entry { entry, .. } => {
                let project_id = inventory
                    .projects
                    .iter()
                    .find(|p| p.threads.iter().any(|t| t.id == entry.id))
                    .map(|p| p.id.clone())
                    .or_else(|| inventory.settled_projects.get(&entry.id).cloned());
                (
                    crate::handoff::StorageTarget::Entry {
                        title: entry.title.clone(),
                        path: entry.worktree_path.clone(),
                        state: entry.state,
                        sizes: entry.sizes,
                    },
                    vec![*entry],
                    project_id,
                )
            }
            Row::Other(sizes) => (
                crate::handoff::StorageTarget::Other {
                    sizes: *sizes,
                    temp_label: inventory
                        .temporary_volume
                        .as_ref()
                        .map(|v| v.label.clone()),
                },
                Vec::new(),
                None,
            ),
        };
        let ctx = crate::handoff::storage_context_from_inventory(inventory, target, &entries);
        Some((ctx, preferred_project))
    }

    /// Select a visible row from a zero-based terminal click. Section headers, summary
    /// rows, details and footer are inert.
    pub fn select_at(&mut self, inventory: &Inventory, w: usize, h: usize, y: usize) {
        let layout = draw::Layout::new(inventory, w, h);
        if self.help || !layout.fits() {
            return;
        }
        let rows = Rows::new(inventory, &self.folded);
        let lines = layout.lines();
        self.reconcile(&rows, lines);
        if let Some(offset) = y.checked_sub(layout.list_top as usize) {
            let index = self.first + offset;
            if offset < Self::capacity(rows.items.len(), lines) && index < rows.items.len() {
                self.select(&rows, index, lines);
            }
        }
    }

    fn select(&mut self, rows: &Rows<'_>, index: usize, lines: usize) {
        self.index = index.min(rows.items.len().saturating_sub(1));
        self.selected = rows.items.get(self.index).map(Row::id);
        self.keep_in_view(rows.items.len(), lines);
    }

    fn reconcile(&mut self, rows: &Rows<'_>, lines: usize) {
        if rows.items.is_empty() {
            self.first = 0;
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|id| rows.items.iter().position(|row| row.has_id(id)))
            .unwrap_or(self.index);
        self.select(rows, index, lines);
    }

    fn capacity(total: usize, lines: usize) -> usize {
        if total > lines && lines > 1 {
            lines - 1
        } else {
            lines
        }
    }

    fn keep_in_view(&mut self, total: usize, lines: usize) {
        let capacity = Self::capacity(total, lines).max(1);
        self.first = self
            .first
            .min(self.index)
            .min(total.saturating_sub(capacity));
        self.first = self.first.max((self.index + 1).saturating_sub(capacity));
    }
}
