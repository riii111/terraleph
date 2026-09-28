mod render;
mod sidebar;

use std::ops::ControlFlow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Size;

use super::{
    overview::{
        self, OverviewInput,
        matrix::{MatrixCell, MatrixSelectedItem, MatrixView},
    },
    plan_review::{self, PlanReviewInput, PlanReviewViewState},
};
use crate::{
    app::{
        copy::CopyTarget,
        environments::{
            EnvironmentSession, EnvironmentState,
            comparison::{CellState, EnvironmentSelection as ComparisonSelection},
            overview::{
                EnvironmentOverviewWithRelations, environment_overview_with_relations_for_selection,
            },
        },
        plan::RelationNodeId,
        session::{Action, ReviewSessionState},
    },
    ui::{
        QuitConfirmationInput,
        features::overview::relations::RelationGraphScroll,
        input::normalize_key,
        primitives::molecules::dialog_scroll::DialogScroll,
        quit_confirmation_key_to_input,
        shell::environments::{self, EnvironmentPane, EnvironmentSelection},
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SidebarSetting {
    #[default]
    Uninitialized,
    Closed,
    Open,
}

pub(crate) struct EnvironmentView {
    selection: EnvironmentSelection,
    selected_environments: Option<Vec<usize>>,
    matrix: MatrixView,
    environment_relations: Option<EnvironmentOverviewWithRelations>,
    relation_revision: Option<u64>,
    relation_environments: Vec<usize>,
    relation_scrolls: Vec<RelationGraphScroll>,
    confirming_quit: bool,
    reviews: Vec<PlanReviewViewState>,
    notice: Option<String>,
    dialog: Option<EnvironmentDialog>,
    dialog_scroll: DialogScroll,
    focus: EnvironmentPane,
    last_right_focus: EnvironmentPane,
    sidebar_enabled: bool,
    sidebar: SidebarSetting,
    sidebar_width: u16,
    maximized: Option<EnvironmentPane>,
}

enum EnvironmentDialog {
    Help,
    Message(String),
}

#[derive(Clone, Copy)]
enum SelectedRowState {
    Absent,
    Unchanged,
}

impl SelectedRowState {
    fn scope_note(self, environment: &str) -> String {
        match self {
            Self::Absent => format!("selected row not in {environment}"),
            Self::Unchanged => format!("selected row unchanged in {environment}"),
        }
    }
}

fn overview_navigation_alias(key: KeyEvent, pane: EnvironmentPane) -> KeyEvent {
    let code = match (key.code, key.modifiers) {
        (KeyCode::Char('h'), KeyModifiers::NONE) if pane != EnvironmentPane::Environments => {
            KeyCode::Left
        }
        (KeyCode::Char('l'), KeyModifiers::NONE) if pane != EnvironmentPane::Environments => {
            KeyCode::Right
        }
        (KeyCode::Char('g'), KeyModifiers::NONE) => KeyCode::Home,
        (KeyCode::Char('G'), KeyModifiers::NONE) => KeyCode::End,
        _ => return key,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(crate) enum EnvironmentInput {
    Retry(usize),
    Review(usize, Box<Action>),
    Quit,
    Interrupt,
}

impl Default for EnvironmentView {
    fn default() -> Self {
        Self {
            selection: EnvironmentSelection::default(),
            selected_environments: None,
            matrix: MatrixView::default(),
            environment_relations: None,
            relation_revision: None,
            relation_environments: Vec::new(),
            relation_scrolls: Vec::new(),
            confirming_quit: false,
            reviews: Vec::new(),
            notice: None,
            dialog: None,
            dialog_scroll: DialogScroll::default(),
            focus: EnvironmentPane::Matrix,
            last_right_focus: EnvironmentPane::Matrix,
            sidebar_enabled: false,
            sidebar: SidebarSetting::Uninitialized,
            sidebar_width: 24,
            maximized: None,
        }
    }
}

impl EnvironmentView {
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        size: Size,
        state: &EnvironmentSession,
    ) -> Option<EnvironmentInput> {
        self.initialize(size, state);
        self.sync(state);
        let key = normalize_key(key);
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.selection.raw.is_none() && self.matrix.searching() {
                self.matrix.apply(OverviewInput::SearchCancel, 1);
                return None;
            }
            return Some(EnvironmentInput::Interrupt);
        }
        if self.confirming_quit {
            if state.acquiring() {
                return match key.code {
                    KeyCode::Enter => Some(EnvironmentInput::Interrupt),
                    KeyCode::Esc => {
                        self.confirming_quit = false;
                        None
                    }
                    _ => None,
                };
            }
            return match quit_confirmation_key_to_input(key) {
                QuitConfirmationInput::Confirm => {
                    self.confirming_quit = false;
                    Some(EnvironmentInput::Quit)
                }
                QuitConfirmationInput::Cancel => {
                    self.confirming_quit = false;
                    None
                }
                QuitConfirmationInput::Consume => None,
                QuitConfirmationInput::Forward(key) => {
                    self.confirming_quit = false;
                    self.handle_key(key, size, state)
                }
            };
        }
        if self.dialog.is_some() {
            return self.handle_dialog_key(key);
        }

        let editing = self.is_editing();
        let clearing_filter = self.selection.raw.is_none()
            && self.matrix.filtered()
            && self.maximized.is_none()
            && key.code == KeyCode::Esc;
        let key = if !editing && !clearing_filter && self.selection.raw.is_none() {
            overview_navigation_alias(key, self.active_pane(size.width))
        } else {
            key
        };
        let matrix_page = if !editing && !clearing_filter {
            self.overview_page_size(size, state)
        } else {
            1
        };
        if !editing
            && !clearing_filter
            && let ControlFlow::Break(result) = self.navigation(key, size, state)
        {
            return result;
        }
        if let Some(index) = self.selection.raw {
            return self.handle_review_key(
                key,
                size,
                state.plans()[index].review()?,
                state.can_start_apply(),
            );
        }

        if self.active_pane(size.width) == EnvironmentPane::Environments
            && let ControlFlow::Break(result) = self.handle_environment_key(key, state)
        {
            return result;
        }

        let input = overview::key_to_input(key, self.matrix.searching(), self.matrix.filtered())?;
        self.handle_overview_input(input, state, matrix_page)
    }

    fn initialize(&mut self, size: Size, state: &EnvironmentSession) {
        if self.sidebar != SidebarSetting::Uninitialized {
            return;
        }
        self.sidebar_enabled = state.plans().len() > 1;
        self.sidebar_width = environments::sidebar_width(state.plans());
        self.sidebar = if self.sidebar_enabled && size.width >= 90 {
            SidebarSetting::Open
        } else {
            SidebarSetting::Closed
        };
        self.focus = if self.sidebar == SidebarSetting::Open {
            EnvironmentPane::Environments
        } else {
            EnvironmentPane::Matrix
        };
        self.last_right_focus = EnvironmentPane::Matrix;
    }

    fn sync(&mut self, state: &EnvironmentSession) {
        self.reviews
            .resize_with(state.plans().len(), PlanReviewViewState::default);
        let environments = self.compared_environments(state.plans().len());
        self.relation_scrolls.resize(
            state.plans().len(),
            RelationGraphScroll {
                vertical: 0,
                horizontal: 0,
            },
        );
        let selection =
            ComparisonSelection::new(self.selected_environments.clone(), state.plans().len())
                .expect("the displayed comparison indexes form a valid selection");
        if self.relation_revision != Some(state.revision())
            || self.relation_environments != selection.indexes()
        {
            self.environment_relations = Some(environment_overview_with_relations_for_selection(
                state.plans(),
                &selection,
            ));
            self.relation_revision = Some(state.revision());
            self.relation_environments = selection.indexes().to_vec();
        }
        let relation_overview = self
            .environment_relations
            .as_ref()
            .expect("environment relations are initialized during sync");
        let matrix_overview = if environments.len() == state.plans().len() {
            state.overview()
        } else {
            &relation_overview.overview
        };
        self.matrix
            .sync(state, &environments, self.selection.column, matrix_overview);
    }

    fn selected_relation_node(&self, state: &EnvironmentSession) -> Option<&RelationNodeId> {
        if !self
            .compared_environments(state.plans().len())
            .contains(&self.selection.column)
        {
            return None;
        }
        if self.selected_row_state_in_environment().is_some() {
            return None;
        }
        let (row_id, _) = self.matrix.relation_selection()?;
        self.environment_relations
            .as_ref()?
            .relations
            .get(&self.selection.column)?
            .row_node_ids
            .get(row_id)?
            .as_ref()
    }

    // A selected row that has no change in the shown environment must not highlight a node there,
    // or a prod-only instance would read as present in dev through its group.
    fn selected_row_state_in_environment(&self) -> Option<SelectedRowState> {
        let Some(MatrixSelectedItem::Resource {
            cell: Some(cell), ..
        }) = self.matrix.selected_item(self.selection.column)
        else {
            return None;
        };
        match cell.state {
            CellState::Missing => Some(SelectedRowState::Absent),
            CellState::NoOp => Some(SelectedRowState::Unchanged),
            CellState::Change { .. } | CellState::Unavailable => None,
        }
    }

    fn is_editing(&self) -> bool {
        self.selection.raw.map_or_else(
            || self.matrix.searching(),
            |index| self.reviews[index].searching() || self.reviews[index].overlay().is_some(),
        )
    }

    fn navigation(
        &mut self,
        key: KeyEvent,
        size: Size,
        state: &EnvironmentSession,
    ) -> ControlFlow<Option<EnvironmentInput>> {
        if let Some(index) = self.selection.raw {
            return self.raw_navigation(key, index, size, state);
        }

        if self.active_pane(size.width) == EnvironmentPane::Relations
            && let ControlFlow::Break(result) = self.relations_navigation(key, size, state)
        {
            return ControlFlow::Break(result);
        }

        match key.code {
            KeyCode::Char('1') => {
                if self.sidebar_enabled && size.width >= 90 {
                    self.sidebar = SidebarSetting::Open;
                    self.focus = EnvironmentPane::Environments;
                    self.maximized = None;
                }
                return ControlFlow::Break(None);
            }
            KeyCode::Char('2') => {
                self.focus = EnvironmentPane::Matrix;
                self.last_right_focus = EnvironmentPane::Matrix;
                self.maximized = None;
                return ControlFlow::Break(None);
            }
            KeyCode::Char('3') => {
                self.focus = EnvironmentPane::Relations;
                self.last_right_focus = EnvironmentPane::Relations;
                self.maximized = None;
                return ControlFlow::Break(None);
            }
            KeyCode::Left | KeyCode::Right
                if self.active_pane(size.width) == EnvironmentPane::Matrix =>
            {
                return ControlFlow::Continue(());
            }
            KeyCode::Char('h' | 'l')
                if key.modifiers == KeyModifiers::NONE
                    && self.active_pane(size.width) == EnvironmentPane::Environments =>
            {
                return ControlFlow::Break(None);
            }
            KeyCode::Left | KeyCode::Right => {
                return ControlFlow::Break(None);
            }
            KeyCode::Char('b') if self.maximized.is_none() => {
                if self.sidebar_enabled && size.width >= 90 {
                    if self.sidebar == SidebarSetting::Open {
                        self.sidebar = SidebarSetting::Closed;
                        if self.focus == EnvironmentPane::Environments {
                            self.focus = self.last_right_focus;
                        }
                    } else {
                        self.sidebar = SidebarSetting::Open;
                    }
                }
                return ControlFlow::Break(None);
            }
            KeyCode::Char('f') => {
                if self.active_pane(size.width) != EnvironmentPane::Environments {
                    if self.maximized.is_some() {
                        self.maximized = None;
                    } else {
                        self.maximized = Some(self.active_pane(size.width));
                    }
                }
                return ControlFlow::Break(None);
            }
            KeyCode::Char('[' | ']') => {
                let delta = if key.code == KeyCode::Char('[') {
                    -1
                } else {
                    1
                };
                let index = adjacent_environment(self.selection.column, delta, state.plans().len());
                self.select_environment(index);
                return ControlFlow::Break(None);
            }
            KeyCode::Char('r') => {
                let index = self.selection.column;
                let retry = state
                    .plans()
                    .get(index)
                    .filter(|plan| matches!(plan.state(), EnvironmentState::Error))
                    .map(|_| EnvironmentInput::Retry(index));
                return ControlFlow::Break(retry);
            }
            KeyCode::Char('0' | 's') => {
                self.selection.raw = None;
                self.notice = None;
                return ControlFlow::Break(None);
            }
            KeyCode::Esc if self.maximized.is_some() => {
                self.maximized = None;
                return ControlFlow::Break(None);
            }
            KeyCode::Esc if self.matrix.filtered() => {
                return ControlFlow::Continue(());
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn scroll_relations_horizontally(&mut self, key: KeyCode) {
        let Some(scroll) = self.relation_scrolls.get_mut(self.selection.column) else {
            return;
        };
        match key {
            KeyCode::Left => scroll.horizontal = scroll.horizontal.saturating_sub(1),
            KeyCode::Right => scroll.horizontal = scroll.horizontal.saturating_add(1),
            _ => {}
        }
    }

    fn relations_navigation(
        &mut self,
        key: KeyEvent,
        size: Size,
        state: &EnvironmentSession,
    ) -> ControlFlow<Option<EnvironmentInput>> {
        match key.code {
            KeyCode::Left | KeyCode::Right => self.scroll_relations_horizontally(key.code),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_relations_vertically(KeyCode::Up, size),
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_relations_vertically(KeyCode::Down, size);
            }
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                self.scroll_relations_vertically(key.code, size);
            }
            KeyCode::Enter => return ControlFlow::Break(self.open(state, self.selection.column)),
            KeyCode::Char(' ' | '/') => return ControlFlow::Break(None),
            _ => return ControlFlow::Continue(()),
        }
        ControlFlow::Break(None)
    }

    fn scroll_relations_vertically(&mut self, key: KeyCode, size: Size) {
        let sidebar_visible = self.sidebar_visible(size.width);
        let layout = environments::overview_layout(
            ratatui::layout::Rect::new(0, 0, size.width, size.height),
            self.sidebar_width,
            sidebar_visible,
            self.maximized_for_width(size.width),
            !sidebar_visible && self.maximized_for_width(size.width).is_none(),
            true,
        );
        let page = layout.relations.height.saturating_sub(5).max(1);
        let Some(scroll) = self.relation_scrolls.get_mut(self.selection.column) else {
            return;
        };
        match key {
            KeyCode::Up => scroll.vertical = scroll.vertical.saturating_sub(1),
            KeyCode::Down => scroll.vertical = scroll.vertical.saturating_add(1),
            KeyCode::PageUp => scroll.vertical = scroll.vertical.saturating_sub(page),
            KeyCode::PageDown => scroll.vertical = scroll.vertical.saturating_add(page),
            KeyCode::Home => scroll.vertical = 0,
            KeyCode::End => scroll.vertical = u16::MAX,
            _ => {}
        }
    }

    fn raw_navigation(
        &mut self,
        key: KeyEvent,
        index: usize,
        _size: Size,
        state: &EnvironmentSession,
    ) -> ControlFlow<Option<EnvironmentInput>> {
        match key.code {
            KeyCode::Char('[' | ']') => {
                let delta = if key.code == KeyCode::Char('[') {
                    -1
                } else {
                    1
                };
                let next = adjacent_environment(index, delta, state.plans().len());
                return ControlFlow::Break(self.open(state, next));
            }
            KeyCode::Char('0' | 's') => {
                self.selection.raw = None;
                self.notice = None;
                return ControlFlow::Break(None);
            }
            KeyCode::Esc
                if state.plans()[index]
                    .review()
                    .is_some_and(|review| review.review().search_query().is_empty()) =>
            {
                self.selection.raw = None;
                return ControlFlow::Break(None);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn handle_environment_key(
        &mut self,
        key: KeyEvent,
        state: &EnvironmentSession,
    ) -> ControlFlow<Option<EnvironmentInput>> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_environment(self.selection.column.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = state.plans().len().saturating_sub(1);
                self.select_environment(self.selection.column.saturating_add(1).min(last));
            }
            KeyCode::Home => self.select_environment(0),
            KeyCode::End => self.select_environment(state.plans().len().saturating_sub(1)),
            KeyCode::PageUp => self.select_environment(self.selection.column.saturating_sub(5)),
            KeyCode::PageDown => {
                let last = state.plans().len().saturating_sub(1);
                self.select_environment(self.selection.column.saturating_add(5).min(last));
            }
            KeyCode::Char(' ') => self.toggle_comparison(state.plans().len()),
            KeyCode::Char('o') => self.select_only_environment(state.plans().len()),
            KeyCode::Char('a') => self.select_all_environments(),
            KeyCode::Enter | KeyCode::Char('v') => {
                return ControlFlow::Break(self.open(state, self.selection.column));
            }
            KeyCode::Char('c') => {
                if let Some(plan) = state.plans().get(self.selection.column) {
                    self.show_dialog(format!(
                        "Context\n{}\n\nEsc close",
                        environments::context(plan)
                    ));
                }
            }
            KeyCode::Char('y') => {
                return ControlFlow::Break(Some(EnvironmentInput::Review(
                    self.selection.column,
                    Box::new(Action::Copy(CopyTarget::Plan)),
                )));
            }
            KeyCode::Char('?') => self.help(),
            _ => return ControlFlow::Continue(()),
        }
        ControlFlow::Break(None)
    }

    fn handle_overview_input(
        &mut self,
        input: OverviewInput,
        state: &EnvironmentSession,
        matrix_page: usize,
    ) -> Option<EnvironmentInput> {
        self.notice = None;
        match input {
            OverviewInput::Quit => self.quit(),
            OverviewInput::Open => self.open_selected_matrix_row(state),
            OverviewInput::ViewPlan => self.open(state, self.selection.column),
            OverviewInput::Copy => Some(EnvironmentInput::Review(
                self.selection.column,
                Box::new(Action::Copy(CopyTarget::Plan)),
            )),
            OverviewInput::OpenContext => {
                if let Some(plan) = state.plans().get(self.selection.column) {
                    self.show_dialog(format!(
                        "Context\n{}\n\nEsc close",
                        environments::context(plan)
                    ));
                }
                None
            }
            OverviewInput::OpenHelp => {
                self.help();
                None
            }
            _ => {
                self.matrix.apply(input, matrix_page);
                None
            }
        }
    }

    fn raw_area(size: Size) -> ratatui::layout::Rect {
        ratatui::layout::Rect::new(
            0,
            1.min(size.height),
            size.width,
            size.height.saturating_sub(1),
        )
    }

    fn handle_review_key(
        &mut self,
        key: KeyEvent,
        size: Size,
        review: &ReviewSessionState,
        can_start_apply: bool,
    ) -> Option<EnvironmentInput> {
        let index = self.selection.raw?;
        let area = Self::raw_area(size);
        let view = &mut self.reviews[index];
        if view.overlay().is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => view.close_overlay(),
                KeyCode::Up => view.scroll_overlay(-1),
                KeyCode::Down => view.scroll_overlay(1),
                KeyCode::Char('k')
                    if view.overlay() == Some(plan_review::PlanReviewOverlay::Help) =>
                {
                    view.scroll_overlay(-1);
                }
                KeyCode::Char('j')
                    if view.overlay() == Some(plan_review::PlanReviewOverlay::Help) =>
                {
                    view.scroll_overlay(1);
                }
                KeyCode::PageUp => view.scroll_overlay(-8),
                KeyCode::PageDown => view.scroll_overlay(8),
                KeyCode::Left => view.scroll_overlay_left(),
                KeyCode::Right => view.scroll_overlay_right(),
                KeyCode::Char('h')
                    if view.overlay() == Some(plan_review::PlanReviewOverlay::Help) =>
                {
                    view.scroll_overlay_left();
                }
                KeyCode::Char('l')
                    if view.overlay() == Some(plan_review::PlanReviewOverlay::Help) =>
                {
                    view.scroll_overlay_right();
                }
                _ => {}
            }
            return None;
        }
        let input = plan_review::key_to_input(
            key,
            view.searching(),
            !review.review().search_query().is_empty(),
        )?;
        match input {
            PlanReviewInput::Quit => return self.quit(),
            PlanReviewInput::Copy => {
                return Some(EnvironmentInput::Review(
                    index,
                    Box::new(Action::Copy(CopyTarget::Plan)),
                ));
            }
            PlanReviewInput::Apply => {
                let review = review.review();
                if !review.apply_allowed() || !review.metadata().applyable() {
                    return None;
                }
                if !can_start_apply {
                    self.show_dialog(
                        "Apply waits until every environment plan is ready.\n\nEsc close"
                            .to_owned(),
                    );
                    return None;
                }
                return Some(EnvironmentInput::Review(
                    index,
                    Box::new(Action::OpenApplyConfirmation),
                ));
            }
            PlanReviewInput::OpenOverview => return None,
            _ => {}
        }
        let layout = plan_review::environment_layout(area, view, review);
        view.apply_with_matches(
            input,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            review.review().search_query(),
            layout.matches(),
        )
        .map(|query| EnvironmentInput::Review(index, Box::new(Action::ReviewSearchChanged(query))))
    }

    fn active_pane(&self, width: u16) -> EnvironmentPane {
        self.maximized_for_width(width).unwrap_or_else(|| {
            if self.focus == EnvironmentPane::Environments && self.sidebar_visible(width) {
                EnvironmentPane::Environments
            } else if self.focus == EnvironmentPane::Relations {
                EnvironmentPane::Relations
            } else {
                EnvironmentPane::Matrix
            }
        })
    }

    const fn sidebar_visible(&self, width: u16) -> bool {
        self.sidebar_enabled && matches!(self.sidebar, SidebarSetting::Open) && width >= 90
    }

    fn maximized_for_width(&self, width: u16) -> Option<EnvironmentPane> {
        self.maximized
            .filter(|pane| *pane != EnvironmentPane::Environments || width >= 90)
    }

    fn compared_environments(&self, count: usize) -> Vec<usize> {
        self.selected_environments
            .clone()
            .unwrap_or_else(|| (0..count).collect())
    }

    fn select_environment(&mut self, index: usize) {
        if self.selection.column != index {
            self.selection.column = index;
            self.notice = None;
        }
    }

    fn toggle_comparison(&mut self, count: usize) {
        let mut selected = self.compared_environments(count);
        if let Some(position) = selected
            .iter()
            .position(|index| *index == self.selection.column)
        {
            if selected.len() == 1 {
                self.notice =
                    Some("At least one environment must stay in the comparison.".to_owned());
                return;
            }
            selected.remove(position);
        } else {
            selected.push(self.selection.column);
            selected.sort_unstable();
        }
        self.set_comparison(selected, count);
    }

    fn select_only_environment(&mut self, count: usize) {
        self.set_comparison(vec![self.selection.column], count);
    }

    fn select_all_environments(&mut self) {
        self.selected_environments = None;
        self.notice = None;
    }

    fn set_comparison(&mut self, selected: Vec<usize>, count: usize) {
        let all = selected.len() == count && selected.iter().copied().eq(0..count);
        self.selected_environments = (!all).then_some(selected);
        self.notice = None;
    }

    fn open(&mut self, state: &EnvironmentSession, index: usize) -> Option<EnvironmentInput> {
        let plan = state.plans().get(index)?;
        if plan.review().is_none() {
            self.selection.raw = None;
            self.select_environment(index);
            self.show_dialog(format!(
                "{}: {}\n{}\n{}\n\nEsc close   r retries Error after closing",
                environments::name(plan),
                environments::status(plan),
                environments::context(plan),
                if matches!(plan.state(), EnvironmentState::Error) {
                    plan.diagnostic().text().to_owned()
                } else {
                    "Only Ready environments have a reviewable plan.".to_owned()
                }
            ));
            return None;
        }
        Some(self.open_at(index, 0, None))
    }

    fn open_at(&mut self, index: usize, line: usize, notice: Option<String>) -> EnvironmentInput {
        self.notice = notice;
        self.selection.raw = Some(index);
        self.reviews[index].jump_to_line(line, usize::MAX);
        EnvironmentInput::Review(index, Box::new(Action::ReviewSearchChanged(String::new())))
    }

    fn open_selected_matrix_row(&mut self, state: &EnvironmentSession) -> Option<EnvironmentInput> {
        match self.matrix.selected_item(self.selection.column)? {
            MatrixSelectedItem::SameChanges => None,
            MatrixSelectedItem::Resource {
                addresses,
                cell,
                grouped,
            } => self.open_selected_resource(state, &addresses, cell.as_ref(), grouped),
        }
    }

    fn open_selected_resource(
        &mut self,
        state: &EnvironmentSession,
        addresses: &[String],
        cell: Option<&MatrixCell>,
        grouped: bool,
    ) -> Option<EnvironmentInput> {
        let index = self.selection.column;
        let Some(plan) = state.plans().get(index) else {
            self.notice = Some("The selected environment has no plan.".to_owned());
            return None;
        };
        if cell.is_none()
            && matches!(
                plan.state(),
                EnvironmentState::Pending | EnvironmentState::Running
            )
        {
            self.notice = Some(format!(
                "{} has not finished plan acquisition.",
                environments::name(plan)
            ));
            return None;
        }
        if let Some(cell) = cell {
            match &cell.state {
                CellState::Unavailable => {
                    self.notice = Some(format!(
                        "{} has not finished plan acquisition.",
                        environments::name(plan)
                    ));
                    return None;
                }
                CellState::Missing => {
                    self.notice = Some(format!(
                        "{} has no resource in this row.",
                        environments::name(plan)
                    ));
                    return None;
                }
                CellState::Change { .. } | CellState::NoOp => {}
            }
            if cell
                .source
                .as_ref()
                .is_some_and(|source| source.environment != index)
            {
                self.notice = Some("The matrix source belongs to another environment.".to_owned());
                return None;
            }
            if !grouped
                && cell
                    .source
                    .as_ref()
                    .is_none_or(|source| source.line.is_none())
            {
                self.notice = Some("The selected row has no source block.".to_owned());
                return None;
            }
        }
        let Some(review) = plan.review() else {
            self.notice = Some(format!(
                "{} has no ready plan for this row.",
                environments::name(plan)
            ));
            return None;
        };
        let document = review.review().document();
        let matches = addresses
            .iter()
            .filter_map(|address| {
                document
                    .block_for_address(address)
                    .map(|block| (address.as_str(), block.lines().start))
            })
            .collect::<Vec<_>>();
        let Some((address, line)) = matches.first().copied() else {
            let row = addresses.first().map_or("the selected row", String::as_str);
            self.notice = Some(format!(
                "No source block for {row} in {}.",
                environments::name(plan)
            ));
            return None;
        };
        let notice = grouped.then(|| {
            format!(
                "Opening the first of {} matching resources: {address}.",
                matches.len()
            )
        });
        Some(self.open_at(index, line, notice))
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) -> Option<EnvironmentInput> {
        let is_help = matches!(self.dialog, Some(EnvironmentDialog::Help));
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => self.dialog = None,
            KeyCode::Up | KeyCode::Char('k') if is_help => self.dialog_scroll.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') if is_help => self.dialog_scroll.scroll_by(1),
            KeyCode::Up => self.dialog_scroll.scroll_by(-1),
            KeyCode::Down => self.dialog_scroll.scroll_by(1),
            KeyCode::PageUp => self.dialog_scroll.scroll_by(-4),
            KeyCode::PageDown => self.dialog_scroll.scroll_by(4),
            KeyCode::Left | KeyCode::Char('h') if is_help => {
                self.dialog_scroll.scroll_left();
            }
            KeyCode::Right | KeyCode::Char('l') if is_help => {
                self.dialog_scroll.scroll_right();
            }
            KeyCode::Char('q') => return self.quit(),
            _ => {}
        }
        None
    }

    fn show_dialog(&mut self, text: String) {
        self.dialog = Some(EnvironmentDialog::Message(text));
        self.dialog_scroll.reset();
    }

    fn help(&mut self) {
        self.dialog = Some(EnvironmentDialog::Help);
        self.dialog_scroll.reset();
    }

    const fn quit(&mut self) -> Option<EnvironmentInput> {
        self.confirming_quit = true;
        None
    }
}

fn adjacent_environment(active: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    active.saturating_add_signed(delta).min(count - 1)
}

#[cfg(test)]
mod tests;
