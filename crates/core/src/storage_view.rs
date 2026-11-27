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
    /// `f` in the cleanup settings panel (`Tab` from the folder list): start a T3 thread to
    /// choose T3's cleanup settings.
    CleanupSettingsInT3,
    Quit,
}

#[derive(Debug, Clone, Default)]
pub struct StorageView {
    selected: Option<RowId>,
    index: usize,
    first: usize,
    folded: HashSet<GroupId>,
    help: bool,
    /// The T3 CLEANUP SETTINGS panel, when open.
    panel: Option<Panel>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Panel {
    index: usize,
    /// The first list line shown.
    first: usize,
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
        self.reconcile_panel(inventory, w, h);
        draw::render(self, &rows, inventory, dashboard, layout, palette)
    }

    /// Whether `Tab` can switch between the folder list and the cleanup settings panel: T3's
    /// settings are known, help is closed and the terminal is big enough.
    fn panel_switchable(&self, inventory: &Inventory, w: usize, h: usize) -> bool {
        !self.help && draw::Layout::fits_size(w, h) && Self::band(inventory).is_some()
    }

    /// Whether help is open over the list or the panel: every key but the theme keys goes to it.
    pub fn help_open(&self) -> bool {
        self.help
    }

    /// The panel is open, for the host's footer and tests.
    pub fn panel_open(&self) -> bool {
        self.panel.is_some()
    }

    /// The panel row the cursor is on, as the `f` handoff names it.
    pub fn panel_selection(&self, inventory: &Inventory) -> Option<&'static str> {
        let band = Self::band(inventory)?;
        let panel = self.panel?;
        draw::panel::rows(band)
            .get(panel.index)
            .map(|row| row.word())
    }

    fn band(inventory: &Inventory) -> Option<&data::Cleanup> {
        inventory
            .cleanup
            .as_ref()
            .filter(|band| band.panel_available())
    }

    /// Close the panel when the band went away; keep the selection and scroll in range.
    fn reconcile_panel(&mut self, inventory: &Inventory, w: usize, h: usize) {
        let Some(panel) = &mut self.panel else {
            return;
        };
        let Some(band) = Self::band(inventory) else {
            self.panel = None;
            return;
        };
        let count = draw::panel::rows(band).len();
        panel.index = panel.index.min(count.saturating_sub(1));
        let geometry = draw::panel::Geometry::new(band, w, h);
        panel.first = geometry.scroll(panel.first, panel.index);
    }

    fn panel_key(
        &mut self,
        key: &str,
        inventory: &Inventory,
        w: usize,
        h: usize,
    ) -> Option<Effect> {
        let band = Self::band(inventory)?;
        let count = draw::panel::rows(band).len();
        let page = draw::panel::Geometry::new(band, w, h).page();
        let panel = self.panel.as_mut()?;
        match key {
            "tab" | "shift-tab" | "escape" => self.panel = None,
            "down" | "j" => panel.index = (panel.index + 1).min(count - 1),
            "up" | "k" => panel.index = panel.index.saturating_sub(1),
            "pagedown" | "J" => panel.index = (panel.index + page).min(count - 1),
            "pageup" | "K" => panel.index = panel.index.saturating_sub(page),
            "home" | "g" => panel.index = 0,
            "end" | "G" => panel.index = count - 1,
            "f" if draw::Layout::fits_size(w, h) => return Some(Effect::CleanupSettingsInT3),
            // Read it again: the settings and the folders, as on the list.
            "r" if !inventory.scan.scanning() => return Some(Effect::Rescan),
            _ => {}
        }
        self.reconcile_panel(inventory, w, h);
        None
    }

    /// Returns a host effect only for an action currently available in the view.
    /// Keys use the terminal parser's existing names (for example, `"enter"`).
    pub fn key(&mut self, key: &str, inventory: &Inventory, w: usize, h: usize) -> Option<Effect> {
        match key {
            "q" => return Some(Effect::Quit),
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
            // Help covers the panel and the list; nothing else reaches what it hides.
            _ if self.help => return None,
            // Storage's own key: back to the dashboard, from the panel too.
            "s" => return Some(Effect::Back),
            _ if self.panel.is_some() && Self::band(inventory).is_none() => self.panel = None,
            _ if self.panel.is_some() => return self.panel_key(key, inventory, w, h),
            "escape" => return Some(Effect::Back),
            "tab" | "shift-tab" => {
                if self.panel_switchable(inventory, w, h) {
                    self.panel = Some(Panel::default());
                    self.reconcile_panel(inventory, w, h);
                }
                return None;
            }
            _ => {}
        }
        if key == "escape" {
            return Some(Effect::Back);
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
        if self.panel.is_some() {
            self.reconcile_panel(inventory, w, h);
            if let (Some(band), Some(panel)) = (Self::band(inventory), &mut self.panel) {
                let geometry = draw::panel::Geometry::new(band, w, h);
                if let Some(index) = geometry.row_at(panel.first, y) {
                    panel.index = index;
                }
            }
            self.reconcile_panel(inventory, w, h);
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
