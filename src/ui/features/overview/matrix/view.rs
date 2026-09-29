use std::collections::BTreeSet;

use ratatui::text::Line;

use crate::app::environments::{
    EnvironmentSession,
    comparison::{CellState, ComparisonRow, DifferenceReason},
    overview::{EnvironmentOverview, GroupId, OverviewRow, OverviewRowId},
};
use crate::app::plan::ResourceChangeKind;
use crate::ui::features::overview::OverviewInput;
use crate::ui::text_input::TextInput;

#[derive(Clone)]
pub(crate) struct MatrixCell {
    pub(crate) state: CellState,
    pub(crate) members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MatrixRowSelection {
    pub(crate) row_id: OverviewRowId,
    pub(crate) child_address: Option<String>,
}

pub(crate) enum MatrixSelectedItem {
    SameChanges,
    Resource {
        addresses: Vec<String>,
        cell: Option<MatrixCell>,
        grouped: bool,
    },
}

pub(super) struct SameChangeSummary {
    pub(super) rows: usize,
    pub(super) actions: ChangeCounts,
    pub(super) has_unknown: bool,
    pub(super) instance_counts_differ: bool,
}

#[derive(Default)]
pub(super) struct ChangeCounts {
    pub(super) deletes: usize,
    pub(super) replacements: usize,
    pub(super) unknown: usize,
}

pub(super) struct Row {
    pub(super) address: String,
    pub(super) group: Option<GroupId>,
    pub(super) group_members: Vec<String>,
    pub(super) selection: Option<SelectionKey>,
    pub(super) child: bool,
    pub(super) cells: Vec<MatrixCell>,
    pub(super) difference: Option<DifferenceReason>,
    pub(super) summary: Option<SameChangeSummary>,
    pub(super) has_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SelectionKey {
    SameSummary,
    Resource(MatrixRowSelection),
}

struct Search {
    previous: String,
}

#[derive(Default)]
pub(crate) struct MatrixView {
    pub(super) rows: Vec<Row>,
    pub(super) vertical: usize,
    pub(super) first_column: usize,
    pub(super) selected_environment: Option<usize>,
    pub(super) manual_horizontal_scroll: bool,
    pub(super) expanded: BTreeSet<GroupId>,
    pub(super) same_expanded: bool,
    filter: TextInput,
    pub(super) overview: Option<EnvironmentOverview>,
    pub(super) environments: Vec<usize>,
    pub(super) address_content_width: usize,
    pub(super) unknown_address_width: usize,
    pub(super) selected: Option<SelectionKey>,
    search: Option<Search>,
    revision: Option<u64>,
}

impl MatrixView {
    pub(crate) fn sync(
        &mut self,
        state: &EnvironmentSession,
        environments: &[usize],
        selected_environment: usize,
        overview: &EnvironmentOverview,
    ) {
        if self.selected_environment != Some(selected_environment) {
            self.selected_environment = Some(selected_environment);
            self.manual_horizontal_scroll = false;
        }
        if self.revision != Some(state.revision()) || self.environments != environments {
            self.overview = Some(overview.clone());
            self.environments = environments.to_vec();
            self.rebuild();
            self.revision = Some(state.revision());
        }
    }

    pub(crate) const fn searching(&self) -> bool {
        self.search.is_some()
    }

    pub(crate) fn selected_column(&self, environment: usize) -> Option<usize> {
        self.environments
            .iter()
            .position(|index| *index == environment)
    }

    fn selected_row(&self) -> Option<&MatrixRowSelection> {
        match self.selected.as_ref()? {
            SelectionKey::Resource(selection) => Some(selection),
            SelectionKey::SameSummary => None,
        }
    }

    pub(crate) fn relation_selection(&self) -> Option<(&OverviewRowId, Option<&str>)> {
        self.selected_row()
            .map(|selection| (&selection.row_id, selection.child_address.as_deref()))
    }

    pub(crate) fn selected_item(&self, environment: usize) -> Option<MatrixSelectedItem> {
        let selected = self.selected.as_ref()?;
        if matches!(selected, SelectionKey::SameSummary) {
            return Some(MatrixSelectedItem::SameChanges);
        }
        let identity = self.relation_selection()?;
        let row = self.rows.iter().find(|row| {
            row.selection
                .as_ref()
                .is_some_and(|selection| match selection {
                    SelectionKey::SameSummary => false,
                    SelectionKey::Resource(selection) => {
                        (&selection.row_id, selection.child_address.as_deref()) == identity
                    }
                })
        })?;

        let column = self.selected_column(environment);
        let cell = column.and_then(|column| row.cells.get(column)).cloned();
        let (addresses, grouped) = if row.group.is_some() {
            let addresses = cell
                .as_ref()
                .map_or_else(|| row.group_members.clone(), |cell| cell.members.clone());
            (addresses, true)
        } else {
            (vec![row.address.clone()], false)
        };
        Some(MatrixSelectedItem::Resource {
            addresses,
            cell,
            grouped,
        })
    }

    pub(crate) fn selected_expanded(&self) -> Option<bool> {
        let selected = self.selected.as_ref()?;
        let row = self
            .rows
            .iter()
            .find(|row| row.selection.as_ref() == Some(selected))?;
        match selected {
            SelectionKey::SameSummary => Some(self.same_expanded),
            SelectionKey::Resource(_) => row
                .group
                .as_ref()
                .map(|group| self.expanded.contains(group)),
        }
    }

    pub(crate) const fn filter(&self) -> &str {
        self.filter.text()
    }

    pub(crate) const fn filtered(&self) -> bool {
        !self.filter.text().is_empty()
    }

    pub(crate) fn apply(&mut self, input: OverviewInput, page_size: usize) {
        if self.searching() {
            self.edit_search(input);
            return;
        }
        match input {
            OverviewInput::Up => self.move_selection(-1),
            OverviewInput::Down => self.move_selection(1),
            OverviewInput::PageUp => {
                self.move_selection(-isize::try_from(page_size.max(1)).unwrap_or(isize::MAX));
            }
            OverviewInput::PageDown => {
                self.move_selection(isize::try_from(page_size.max(1)).unwrap_or(isize::MAX));
            }
            OverviewInput::Top => self.select_index(Some(0)),
            OverviewInput::Bottom => self.select_index(self.rows.len().checked_sub(1)),
            OverviewInput::Left => {
                let first = self.first_column.saturating_sub(1);
                self.manual_horizontal_scroll |= first != self.first_column;
                self.first_column = first;
            }
            OverviewInput::Right => {
                let first = self
                    .first_column
                    .saturating_add(1)
                    .min(self.environments.len().saturating_sub(1));
                self.manual_horizontal_scroll |= first != self.first_column;
                self.first_column = first;
            }
            OverviewInput::ToggleExpand => self.toggle_selected_expansion(),
            OverviewInput::SearchStart => {
                self.search = Some(Search {
                    previous: self.filter.text().to_owned(),
                });
                self.filter.move_end();
            }
            OverviewInput::SearchCancel => {
                self.filter.clear();
                self.rebuild();
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let current = self.selected_index().unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.cast_unsigned())
        };
        self.select_index((!self.rows.is_empty()).then(|| next.min(self.rows.len() - 1)));
    }

    fn select_index(&mut self, index: Option<usize>) {
        self.selected =
            index.and_then(|index| self.rows.get(index).and_then(|row| row.selection.clone()));
    }

    fn selected_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.selection.as_ref() == self.selected.as_ref())
    }

    fn toggle_selected_expansion(&mut self) {
        match self.selected.as_ref() {
            Some(SelectionKey::SameSummary) => self.same_expanded = !self.same_expanded,
            Some(SelectionKey::Resource(_)) => {
                let group = self
                    .rows
                    .iter()
                    .find(|row| row.selection.as_ref() == self.selected.as_ref())
                    .and_then(|row| row.group.clone());
                if let Some(group) = group {
                    if self.expanded.contains(&group) {
                        self.expanded.remove(&group);
                    } else {
                        self.expanded.insert(group);
                    }
                }
            }
            None => return,
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let old_index = self.selected_index();
        let old_selection = self.selected.clone();
        let Some(overview) = &self.overview else {
            self.rows.clear();
            self.address_content_width = 0;
            self.unknown_address_width = 0;
            self.selected = None;
            self.vertical = 0;
            return;
        };

        let all_groups = overview
            .rows
            .iter()
            .filter_map(|row| match row {
                OverviewRow::Group(group) => Some(group.id.clone()),
                OverviewRow::Individual(_) => None,
            })
            .collect();
        let layout_rows = rows(overview, self.filter.text(), &all_groups);
        (self.address_content_width, self.unknown_address_width) =
            address_widths(&layout_rows, self.environments.len() > 1);

        let collapsed_rows = rows(overview, self.filter.text(), &BTreeSet::new());
        let summary_start = collapsed_rows
            .iter()
            .position(|row| row.difference.is_none())
            .unwrap_or(collapsed_rows.len());
        let summary_rows = &collapsed_rows[summary_start..];
        let mut visible = rows(overview, self.filter.text(), &self.expanded);
        if self.environments.len() > 1 {
            let same_start = visible
                .iter()
                .position(|row| row.difference.is_none())
                .unwrap_or(visible.len());
            let same_rows = visible.split_off(same_start);
            if !same_rows.is_empty() {
                let summary = same_change_summary(summary_rows);
                visible.push(Row::same_summary(summary));
                if self.same_expanded {
                    visible.extend(same_rows);
                }
            }
        }
        self.rows = visible;
        self.selected = if self
            .rows
            .iter()
            .any(|row| row.selection.as_ref() == old_selection.as_ref())
        {
            old_selection
        } else if self.rows.is_empty() {
            None
        } else {
            self.rows
                .get(old_index.unwrap_or(0).min(self.rows.len() - 1))
                .and_then(|row| row.selection.clone())
        };
        if self.rows.is_empty() {
            self.vertical = 0;
        }
    }

    fn edit_search(&mut self, input: OverviewInput) {
        match input {
            OverviewInput::SearchConfirm => {
                self.search = None;
                return;
            }
            OverviewInput::SearchCancel => {
                let search = self.search.take().expect("active search");
                self.filter = TextInput::with_cursor_at_end(search.previous);
                self.rebuild();
                return;
            }
            OverviewInput::SearchChar(character) => self.filter.insert(character),
            OverviewInput::SearchBackspace => {
                if !self.filter.backspace() {
                    return;
                }
            }
            OverviewInput::SearchLeft => self.filter.move_left(),
            OverviewInput::SearchRight => self.filter.move_right(),
            OverviewInput::SearchHome => self.filter.move_home(),
            OverviewInput::SearchEnd => self.filter.move_end(),
            _ => return,
        }
        self.rebuild();
    }
}

pub(super) fn address_widths(rows: &[Row], under_summary: bool) -> (usize, usize) {
    rows.iter().filter(|row| row.summary.is_none()).fold(
        (Line::from("Address").width(), 0),
        |(content, unknown), row| {
            let lead = row_lead(row, false, under_summary);
            let row_width =
                2 + Line::from(lead.as_str()).width() + Line::from(row.address.as_str()).width();
            (
                // The widest address keeps one blank column before the first cell.
                content.max(row_width + 1),
                if row.has_unknown {
                    unknown.max(row_width + Line::from(" [unknown values] ").width())
                } else {
                    unknown
                },
            )
        },
    )
}

// Every row in a section that can hold groups keeps the expansion column so addresses line up.
pub(super) fn row_lead(row: &Row, expanded: bool, under_summary: bool) -> String {
    // Group members stay under their group even when their own instance is missing elsewhere.
    let same_section = row.difference.is_none() || row.child;
    let section = if under_summary && same_section {
        "  "
    } else {
        ""
    };
    let expansion = match (&row.group, same_section) {
        (Some(_), _) if expanded => "▾ ",
        (Some(_), _) => "▸ ",
        (None, true) => "  ",
        (None, false) => "",
    };
    let child = if row.child { "  " } else { "" };
    format!("{section}{expansion}{child}")
}

impl Row {
    const fn same_summary(summary: SameChangeSummary) -> Self {
        let has_unknown = summary.has_unknown;
        Self {
            address: String::new(),
            group: None,
            group_members: Vec::new(),
            selection: Some(SelectionKey::SameSummary),
            child: false,
            cells: Vec::new(),
            difference: None,
            summary: Some(summary),
            has_unknown,
        }
    }
}

fn rows(overview: &EnvironmentOverview, filter: &str, expanded: &BTreeSet<GroupId>) -> Vec<Row> {
    let mut rows = Vec::new();
    for row in &overview.rows {
        match row {
            OverviewRow::Individual(row) if row.address.contains(filter) => {
                rows.push(individual(
                    row,
                    false,
                    MatrixRowSelection {
                        row_id: OverviewRowId::Individual(row.address.clone()),
                        child_address: None,
                    },
                ));
            }
            OverviewRow::Group(group) => {
                let children: Vec<_> = group
                    .children
                    .iter()
                    .filter(|child| child.address.contains(filter))
                    .collect();
                if children.is_empty() {
                    continue;
                }
                if children.len() == 1 {
                    let child = children[0];
                    rows.push(individual(
                        child,
                        false,
                        MatrixRowSelection {
                            row_id: OverviewRowId::Group(group.id.clone()),
                            child_address: Some(child.address.clone()),
                        },
                    ));
                    continue;
                }
                let cells = group
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| {
                        let members: Vec<_> = children
                            .iter()
                            .filter(|child| matches!(child.cells[index], CellState::Change { .. }))
                            .collect();
                        MatrixCell {
                            state: cell.state.clone(),
                            members: members.iter().map(|child| child.address.clone()).collect(),
                        }
                    })
                    .collect();
                rows.push(Row {
                    address: group.display_address.clone(),
                    group: Some(group.id.clone()),
                    group_members: group
                        .children
                        .iter()
                        .map(|child| child.address.clone())
                        .collect(),
                    selection: Some(SelectionKey::Resource(MatrixRowSelection {
                        row_id: OverviewRowId::Group(group.id.clone()),
                        child_address: None,
                    })),
                    child: false,
                    cells,
                    difference: None,
                    summary: None,
                    has_unknown: group.has_unknown,
                });
                if expanded.contains(&group.id) {
                    rows.extend(children.into_iter().map(|child| {
                        individual(
                            child,
                            true,
                            MatrixRowSelection {
                                row_id: OverviewRowId::Group(group.id.clone()),
                                child_address: Some(child.address.clone()),
                            },
                        )
                    }));
                }
            }
            OverviewRow::Individual(_) => {}
        }
    }
    rows
}

fn individual(row: &ComparisonRow, child: bool, selection: MatrixRowSelection) -> Row {
    Row {
        address: row.address.clone(),
        group: None,
        group_members: Vec::new(),
        selection: Some(SelectionKey::Resource(selection)),
        child,
        cells: row
            .cells
            .iter()
            .map(|cell| MatrixCell {
                state: cell.clone(),
                members: vec![row.address.clone()],
            })
            .collect(),
        difference: row.difference,
        summary: None,
        has_unknown: false,
    }
}

fn same_change_summary(rows: &[Row]) -> SameChangeSummary {
    let mut actions = ChangeCounts::default();
    for row in rows {
        let Some(kind) = row.cells.iter().find_map(|cell| match cell.state {
            CellState::Change { kind, .. } => Some(kind),
            CellState::NoOp | CellState::Missing | CellState::Unavailable => None,
        }) else {
            continue;
        };
        match kind {
            ResourceChangeKind::Delete => actions.deletes += 1,
            ResourceChangeKind::Replace => actions.replacements += 1,
            ResourceChangeKind::Unknown | ResourceChangeKind::Unsupported => actions.unknown += 1,
            ResourceChangeKind::Create
            | ResourceChangeKind::Update
            | ResourceChangeKind::Read
            | ResourceChangeKind::Move
            | ResourceChangeKind::Import
            | ResourceChangeKind::NoOp => {}
        }
    }
    SameChangeSummary {
        rows: rows.len(),
        actions,
        has_unknown: rows.iter().any(|row| row.has_unknown),
        instance_counts_differ: rows.iter().any(group_instance_counts_differ),
    }
}

fn group_instance_counts_differ(row: &Row) -> bool {
    if row.group.is_none() {
        return false;
    }
    let mut counts = row
        .cells
        .iter()
        .filter(|cell| matches!(cell.state, CellState::Change { .. }))
        .map(|cell| cell.members.len());
    counts
        .next()
        .is_some_and(|first| counts.any(|count| count != first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::environments::comparison::ComparisonScope;

    #[test]
    fn individual_row_identity_keeps_the_full_resource_address() {
        let address = "module.application.terraform_data.api[\"primary\"]".to_owned();
        let overview = EnvironmentOverview {
            scope: ComparisonScope::All,
            rows: vec![OverviewRow::Individual(ComparisonRow {
                address: address.clone(),
                cells: Vec::new(),
                difference: None,
                has_unknown: false,
            })],
        };

        let rows = rows(&overview, "", &BTreeSet::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].selection,
            Some(SelectionKey::Resource(MatrixRowSelection {
                row_id: OverviewRowId::Individual(address),
                child_address: None,
            }))
        );
    }
}
