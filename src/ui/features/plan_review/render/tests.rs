use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::{Buffer, CellWidth},
    style::{Color, Modifier},
    text::Line,
    widgets::Widget,
};

use crate::app::{
    copy::{CopyResult, CopyTarget},
    execution::{
        Diagnostic, DiagnosticSeverity, DiagnosticSource, ExecutionContext, ExecutionState, Tool,
        VariableSources,
    },
    plan::{Plan, ResourceChangeKind, test_support::resource_change},
    review::{
        PlanBlock, PlanBlockKind, PlanDocument, PlanLineKind, PlanMetadata, PlanReview,
        test_support::{plan_document, plan_document_with_blocks},
    },
    session::{self, Action, SessionState},
};
use crate::ui::{
    features::plan_review::{
        ApplyConfirmationInput, ApplyConfirmationViewState, PlanReviewInput, PlanReviewMatch,
        key_to_input,
    },
    primitives::molecules::{dialog_scroll::DialogScroll, help_dialog},
    shell::context,
    test_support::{
        assert_dialog_scrolled_up, buffer_text, dialog_body_rows, render_to_buffer,
        write_buffer_captures,
    },
};

use super::{
    apply_confirmation::{CONFIRMATION_MAX_WIDTH, confirmation_input_scroll},
    content::{display_width, styled_plan_line, visible_lines},
    layout::PlanReviewLayout,
    overlay::plan_help_sections,
    review_footer::{footer_items, position_status},
    status::{horizontal_offset, search_query_line},
    *,
};

fn plan_line_and_matches<'a>(
    line: &'a str,
    query: &str,
    line_index: usize,
    selected: Option<&PlanReviewMatch>,
    kind: PlanLineKind,
) -> (Line<'a>, Vec<PlanReviewMatch>) {
    let mut matches = Vec::new();
    let styled = styled_plan_line(line, query, line_index, selected, kind, |found| {
        matches.push(found);
    });
    (styled, matches)
}

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (160, 60)];
const PLAN_AGE: Duration = Duration::from_mins(12);
static PLANNED_AT: LazyLock<Instant> = LazyLock::new(Instant::now);
const SEARCH_TERM: &str = "terraform_data";
const PLAN_TEXT: &str = r#"Terraform will perform the following actions:

  # terraform_data.api will be updated in-place
  ~ resource "terraform_data" "api" {
      id       = "api-20260920"
      ~ input  = "before" -> "after"
      # (4 unchanged attributes hidden)
    }

  # terraform_data.worker must be replaced
-/+ resource "terraform_data" "worker" {
      ~ input = "worker-before" -> "worker-after" # forces replacement
      - old_checksum = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef"
      + new_checksum = (known after apply)
    }

  # terraform_data.old will be destroyed
  - resource "terraform_data" "old" {
      id = "old-20260920"
    }

  # terraform_data.new will be created
  + resource "terraform_data" "new" {
      input = "new-value"
      note  = "A deliberately long synthetic value keeps horizontal scrolling visible"
    }

Changes to Outputs:
  + endpoint = (known after apply)
  ~ summary  = "old summary" -> "new summary with a deliberately long value for review"

Warning: Value for "pending" is not known until apply

Plan: 2 to add, 1 to change, 2 to destroy.

Synthetic review text continues below so the viewport and scrollbar remain meaningful.
The same long body is intentionally reused across every review state and terminal size.
No Terraform process, provider, state file, or cloud credential is used by this fixture.
The review surface preserves Terraform order, attributes, output values, and diagnostics.
Long lines remain unwrapped in the plan body; horizontal movement exposes the hidden suffix.
Vertical movement exposes later lines in this synthetic plan body.

End of synthetic plan body."#;

fn render(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &PlanReviewViewState,
    now: Instant,
) {
    super::render_with_quit_confirmation(frame, state, view, now, false);
}

fn review() -> PlanReview {
    review_with_applyable(true)
}

fn review_with_applyable(applyable: bool) -> PlanReview {
    review_with_options(applyable, true)
}

fn review_with_apply_allowed(applyable: bool, apply_allowed: bool) -> PlanReview {
    review_with_options(applyable, apply_allowed)
}

fn review_with_options(applyable: bool, apply_allowed: bool) -> PlanReview {
    PlanReview::new(
        PathBuf::from("/repo/environments/production/main"),
        "default".to_owned(),
        PlanDocument::with_blocks_and_line_kinds(
            PLAN_TEXT.to_owned(),
            vec![
                PlanBlock::new(0..2, PlanBlockKind::Common),
                PlanBlock::new(2..8, PlanBlockKind::Resource),
                PlanBlock::new(8..9, PlanBlockKind::Common),
                PlanBlock::new(9..15, PlanBlockKind::Resource),
                PlanBlock::new(15..16, PlanBlockKind::Common),
                PlanBlock::new(16..20, PlanBlockKind::Resource),
                PlanBlock::new(20..21, PlanBlockKind::Common),
                PlanBlock::new(21..26, PlanBlockKind::Resource),
                PlanBlock::new(26..28, PlanBlockKind::Common),
                PlanBlock::new(28..29, PlanBlockKind::Output),
                PlanBlock::new(29..30, PlanBlockKind::Output),
                PlanBlock::new(30..43, PlanBlockKind::Common),
            ],
            vec![
                PlanLineKind::Intro,
                PlanLineKind::Intro,
                PlanLineKind::ResourceHeader,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::ResourceHeader,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::ResourceHeader,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::ResourceHeader,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::OutputSection,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
            ],
        ),
        Plan {
            resource_changes: vec![
                resource_change("terraform_data.api", ResourceChangeKind::Update),
                resource_change("terraform_data.worker", ResourceChangeKind::Replace),
                resource_change("terraform_data.old", ResourceChangeKind::Delete),
                resource_change("terraform_data.new", ResourceChangeKind::Create),
            ],
            ..Plan::empty()
        },
        PlanMetadata::new(applyable),
        Vec::new(),
    )
    .with_apply_allowed(apply_allowed)
    .with_planned_at(*PLANNED_AT)
}

// Every confirmation fixture renders the same plan age, so snapshots stay stable.
fn confirmation_now() -> Instant {
    *PLANNED_AT + PLAN_AGE
}

fn searching_view() -> PlanReviewViewState {
    let mut view = PlanReviewViewState::default();
    view.apply_with_matches(PlanReviewInput::SearchStart, Rect::default(), 0, 0, "", &[]);
    view
}

// The plan body rows as text, prepared the way a frame prepares them.
fn content_text(review: &PlanReview, filtered_view: bool) -> Vec<String> {
    PlanContent::prepare(review, filtered_view, review.search_query())
        .lines(review.document(), 0..usize::MAX, None)
        .iter()
        .map(Line::to_string)
        .collect()
}

fn review_state(plan: PlanReview) -> ReviewSessionState {
    let now = Instant::now();
    let mut session = SessionState::new(ExecutionState::with_context(
        now,
        ExecutionContext::loading("/repo"),
    ));
    session::update(&mut session, Action::ReviewCompleted(plan), now);
    session
        .review()
        .expect("review should be available")
        .clone()
}

fn confirmation_state(plan: PlanReview) -> ReviewSessionState {
    let now = Instant::now();
    let mut session = SessionState::new(ExecutionState::with_context(
        now,
        ExecutionContext::loading("/repo"),
    ));
    session::update(&mut session, Action::ReviewCompleted(plan), now);
    session::update(&mut session, Action::OpenApplyConfirmation, now);
    session
        .apply_confirmation()
        .expect("confirmation should be available")
        .clone()
}

fn snapshot(name: &str, buffer: &Buffer) {
    insta::assert_snapshot!(name.to_string(), buffer_text(buffer));
    write_buffer_captures(name, buffer);
}

fn assert_text_prefix_uses_style(
    buffer: &Buffer,
    text: &str,
    styled_prefix: &str,
    foreground: Color,
    background: Color,
    modifier: Modifier,
) {
    assert_text_segment_uses_style(
        buffer,
        text,
        0,
        styled_prefix.chars().count(),
        foreground,
        background,
        modifier,
    );
}

fn assert_text_segment_uses_style(
    buffer: &Buffer,
    text: &str,
    segment_start: usize,
    segment_length: usize,
    foreground: Color,
    background: Color,
    modifier: Modifier,
) {
    assert_text_segment_uses_style_from(
        buffer,
        buffer.area().y,
        text,
        segment_start,
        segment_length,
        (foreground, background, modifier),
    );
}

fn assert_text_segment_uses_style_from(
    buffer: &Buffer,
    first_line: u16,
    text: &str,
    segment_start: usize,
    segment_length: usize,
    expected: (Color, Color, Modifier),
) {
    let area = buffer.area();
    for y in first_line.max(area.y)..area.bottom() {
        let symbols = (area.x..area.right())
            .map(|x| buffer.cell((x, y)).expect("search cell").symbol())
            .collect::<Vec<_>>();
        let Some(start) = (0..symbols.len()).find(|&start| {
            symbols[start..]
                .iter()
                .copied()
                .collect::<String>()
                .starts_with(text)
        }) else {
            continue;
        };
        for offset in segment_start..segment_start + segment_length {
            let cell = buffer
                .cell((
                    area.x + u16::try_from(start + offset).expect("search offset"),
                    y,
                ))
                .expect("search cell");
            assert_eq!(cell.fg, expected.0, "{text}");
            assert_eq!(cell.bg, expected.1, "{text}");
            assert_eq!(cell.modifier, expected.2, "{text}");
        }
        return;
    }
    panic!("text should be visible: {text}");
}

#[test]
fn renders_plan_review_normal_at_all_supported_sizes() {
    for &(width, height) in &SIZES {
        let state = review_state(review());
        let view = PlanReviewViewState::default();
        let buffer = render_to_buffer((width, height), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        snapshot(&format!("preview_{width}x{height}_normal"), &buffer);
    }
}

#[test]
fn renders_plan_apply_entry_at_all_supported_sizes() {
    for &(width, height) in &SIZES {
        let state = review_state(review().with_apply_entry(true));
        let view = PlanReviewViewState::default();
        let buffer = render_to_buffer((width, height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);
        let footer = text
            .lines()
            .find(|line| line.contains("s overview"))
            .expect("overview navigation should be visible");

        assert!(
            footer.starts_with("s overview"),
            "{width}x{height}\n{footer}"
        );
        assert!(footer.contains("a apply"), "{width}x{height}\n{footer}");
        assert!(
            !footer.contains("y copy plan"),
            "{width}x{height}\n{footer}"
        );
        if (width, height) == (80, 24) {
            snapshot("preview_80x24_apply-entry", &buffer);
        }
    }
}

#[test]
fn renders_an_empty_plan_snapshot() {
    let plan = PlanReview::new(
        PathBuf::from("/repo/environments/staging/empty"),
        "default".to_owned(),
        plan_document_with_blocks(String::new(), Vec::new()),
        Plan::empty(),
        PlanMetadata::new(false),
        Vec::new(),
    );
    let state = review_state(plan);
    let buffer = render_to_buffer((120, 40), |frame| {
        render(
            frame,
            &state,
            &PlanReviewViewState::default(),
            Instant::now(),
        );
    });

    snapshot("preview_120x40_empty", &buffer);
}

#[test]
fn renders_long_target_header_and_preserves_position_for_overlays() {
    let plan = review().with_apply_entry(true).with_context(
        ExecutionContext::loading("/repo/environments/production/very-long-target-name-for-review")
            .with_launch_root("/repo")
            .with_workspace("default")
            .with_tool_version(Tool::Terraform, "1.9.0")
            .with_variable_sources(VariableSources::new(
                vec![PathBuf::from("/repo/environments/production/common.tfvars")],
                vec![PathBuf::from("/repo/secrets/production.tfvars")],
                true,
                std::iter::once("TF_VAR_region".to_owned())
                    .chain((0..32).map(|index| format!("TF_VAR_{index:02}")))
                    .collect(),
            )),
    );
    let state = review_state(plan);
    let area = Rect::new(0, 0, 120, 40);
    let layout = layout(area, &PlanReviewViewState::default(), &state);
    let mut view = PlanReviewViewState::default();
    view.apply_with_matches(
        PlanReviewInput::Down,
        layout.body(),
        layout.max_vertical(),
        layout.max_horizontal(),
        "",
        layout.matches(),
    );
    let position = view.scroll();
    let normal = render_to_buffer((area.width, area.height), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    snapshot("preview_120x40_long-target", &normal);
    for (width, height) in [(80, 24), (160, 60)] {
        let buffer = render_to_buffer((width, height), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        let text = buffer_text(&buffer);
        assert!(text.contains("Target: "), "{width}x{height}\n{text}");
        assert!(text.contains("[PROD]"), "{width}x{height}\n{text}");
        assert!(
            text.contains("Workspace: default"),
            "{width}x{height}\n{text}"
        );
        assert!(
            text.contains("Tool: terraform 1.9.0"),
            "{width}x{height}\n{text}"
        );
        assert!(text.contains("Dir: ./"), "{width}x{height}\n{text}");
        snapshot(&format!("preview_{width}x{height}_long-target"), &buffer);
    }
    view.apply_with_matches(
        PlanReviewInput::OpenHelp,
        layout.body(),
        layout.max_vertical(),
        layout.max_horizontal(),
        "",
        layout.matches(),
    );
    let help = render_to_buffer((area.width, area.height), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    let help_text = buffer_text(&help);
    assert!(help_text.contains("Help"));
    assert_eq!(view.scroll(), position);

    view.close_overlay();
    view.apply_with_matches(
        PlanReviewInput::OpenContext,
        layout.body(),
        layout.max_vertical(),
        layout.max_horizontal(),
        "",
        layout.matches(),
    );
    let context = render_to_buffer((area.width, area.height), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    let context_text = buffer_text(&context);
    let compact_context = context_text.replace('\n', "");
    assert!(context_text.contains("very-long-target-name-for-review [PROD]"));
    assert!(context_text.contains("Workspace: default"));
    assert!(context_text.contains("terraform 1.9.0"));
    assert!(
        compact_context.contains("/repo/environments/production/very-long-target-name-for-review")
    );
    assert!(context_text.contains("Execution directory"));
    assert!(compact_context.contains("/repo/secrets/production.tfvars"));
    assert!(context_text.contains("TF_VAR_region"));
    view.overlay_scroll_mut().bottom();
    let scrolled_context = render_to_buffer((80, 24), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    assert!(buffer_text(&scrolled_context).contains("TF_VAR_31"));
    assert_eq!(view.scroll(), position);
}

#[test]
fn renders_plan_help_with_overview_navigation_and_scrollable_sections() {
    let state = review_state(review().with_apply_entry(true));
    let mut view = PlanReviewViewState::default();
    view.apply_with_matches(PlanReviewInput::OpenHelp, Rect::default(), 0, 0, "", &[]);

    for (width, height) in [(40, 16), (40, 24), (80, 24), (120, 40)] {
        let help = render_to_buffer((width, height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let help_text = buffer_text(&help);
        assert!(help_text.contains("Help"), "{width}x{height}: {help_text}");
        assert!(
            !help_text.contains("clear filter"),
            "{width}x{height}: {help_text}"
        );
        assert_eq!(
            help_text.matches("close").count(),
            1,
            "{width}x{height}: {help_text}"
        );
        if width >= 80 {
            assert!(
                help_text.lines().any(|line| {
                    let words: Vec<_> = line.split_whitespace().collect();
                    words.contains(&"s") && words.contains(&"overview")
                }),
                "{width}x{height}: {help_text}"
            );
            assert!(
                help_text.contains("copy the full plan"),
                "{width}x{height}: {help_text}"
            );
            assert!(
                help_text.contains("apply the full plan"),
                "{width}x{height}: {help_text}"
            );
        }
        if (width, height) == (120, 40) {
            assert!(
                help.cell((0, 0))
                    .expect("dimmed background")
                    .modifier
                    .contains(Modifier::DIM)
            );
        }
        snapshot(&format!("preview_{width}x{height}_help"), &help);
    }

    view.overlay_scroll_mut().bottom();
    let bottom = render_to_buffer((80, 24), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    let bottom_text = buffer_text(&bottom);
    assert!(bottom_text.contains("Exit"));
    assert!(bottom_text.contains("quit"));
    assert_eq!(bottom_text.matches("close").count(), 1);
    snapshot("preview_80x24_help_bottom", &bottom);

    let small_bottom = render_to_buffer((40, 16), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    let small_bottom_text = buffer_text(&small_bottom);
    assert!(small_bottom_text.contains("Exit"));
    assert!(small_bottom_text.contains("quit"));
    assert_eq!(small_bottom_text.matches("close").count(), 1);
    snapshot("preview_40x16_help_bottom", &small_bottom);
}

#[test]
fn confirmed_filter_help_explains_how_to_clear_the_filter() {
    let mut plan = review();
    plan.set_search_query("worker".to_owned());
    let state = review_state(plan);
    let mut view = PlanReviewViewState::default();
    view.apply_with_matches(PlanReviewInput::OpenHelp, Rect::default(), 0, 0, "", &[]);

    let help = render_to_buffer((120, 40), |frame| {
        render(frame, &state, &view, Instant::now());
    });
    let text = buffer_text(&help);

    assert!(text.contains("next or previous match"));
    assert!(text.contains("press Esc again to clear filter"));
}

#[test]
fn confirmation_opened_from_the_overview_detail_draws_the_same_background() {
    let now = Instant::now();
    let mut session = SessionState::new(ExecutionState::with_context(
        now,
        ExecutionContext::loading("/repo"),
    ));
    session::update(&mut session, Action::ReviewCompleted(review()), now);
    session::update(&mut session, Action::OpenOverview, now);
    session::update(
        &mut session,
        Action::CopyCompleted {
            target: CopyTarget::Plan,
            result: CopyResult::Written,
        },
        now,
    );
    session::update(&mut session, Action::OpenReviewFromOverview, now);
    session::update(
        &mut session,
        Action::ReviewSearchChanged("worker".to_owned()),
        now,
    );
    session::update(&mut session, Action::OpenApplyConfirmation, now);
    let from_overview = session
        .apply_confirmation()
        .expect("confirmation should open from the overview detail");

    let mut filtered = review();
    filtered.set_search_query("worker".to_owned());
    let direct = confirmation_state(filtered);
    for size in SIZES {
        let view = ApplyConfirmationViewState::default();
        assert_eq!(
            render_to_buffer(size, |frame| {
                render_apply_confirmation(
                    frame,
                    from_overview,
                    &PlanReviewViewState::default(),
                    &view,
                    confirmation_now(),
                );
            }),
            render_to_buffer(size, |frame| {
                render_apply_confirmation(
                    frame,
                    &direct,
                    &PlanReviewViewState::default(),
                    &view,
                    confirmation_now(),
                );
            }),
            "{size:?}"
        );
    }
}

#[test]
fn overview_detail_layout_matches_the_raw_review_opened_from_the_overview() {
    let now = Instant::now();
    let mut plan = review_with_content(80, 120);
    plan.set_search_query("line".to_owned());
    let mut session = SessionState::Review(Box::new(session::test_support::overview_session(plan)));
    session::update(
        &mut session,
        Action::CopyCompleted {
            target: CopyTarget::Plan,
            result: CopyResult::Written,
        },
        now,
    );
    let overview = session.overview().expect("overview should be visible");
    let area = Rect::new(0, 0, 80, 24);
    let predicted =
        overview_detail_layout(area, &PlanReviewViewState::default(), overview.review());

    session::update(&mut session, Action::OpenReviewFromOverview, now);
    let raw = layout(
        area,
        &PlanReviewViewState::default(),
        session.review().expect("raw review should be visible"),
    );
    assert_eq!(predicted.body(), raw.body());
    assert_eq!(predicted.max_vertical(), raw.max_vertical());
    assert_eq!(predicted.max_horizontal(), raw.max_horizontal());
    assert_eq!(predicted.matches(), raw.matches());
}

#[test]
fn renders_apply_help_and_context_with_only_confirmation_actions() {
    let plan = review().with_context(
        ExecutionContext::loading("/repo/environments/production/main")
            .with_launch_root("/repo")
            .with_workspace("default")
            .with_tool_version(Tool::Terraform, "1.9.0"),
    );
    let state = confirmation_state(plan);
    for (width, height) in [(40, 16), (40, 24), (80, 24)] {
        let mut view = ApplyConfirmationViewState::default();
        assert_eq!(
            view.apply(ApplyConfirmationInput::OpenHelp, "main", 0),
            None
        );
        let help = render_to_buffer((width, height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &view,
                confirmation_now(),
            );
        });
        let help_text = buffer_text(&help);
        assert!(
            help_text.contains("Apply help"),
            "{width}x{height}: {help_text}"
        );
        assert_eq!(
            help_text.matches("close").count(),
            1,
            "{width}x{height}: {help_text}"
        );
        if width >= 80 {
            assert!(
                help_text.contains("confirm apply"),
                "{width}x{height}: {help_text}"
            );
            assert!(
                help_text.contains("show execution context"),
                "{width}x{height}: {help_text}"
            );
        }
        snapshot(&format!("apply_confirmation_help_{width}x{height}"), &help);

        let renderable =
            apply_confirmation_layout(Rect::new(0, 0, width, height), &state, confirmation_now())
                .renderable();
        if (width, height) == (40, 16) {
            assert!(
                !renderable,
                "{width}x{height} should exercise Help over an unrenderable confirmation"
            );
        } else {
            assert!(
                renderable,
                "{width}x{height} should exercise Help over a rendered confirmation"
            );
        }

        if width == 40 {
            view.overlay_scroll_mut().bottom();
            let bottom = render_to_buffer((width, height), |frame| {
                render_apply_confirmation(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    &view,
                    confirmation_now(),
                );
            });
            let bottom_text = buffer_text(&bottom);
            assert!(
                bottom_text.contains("confirm"),
                "{width}x{height}: {bottom_text}"
            );
            assert_eq!(
                bottom_text.matches("close").count(),
                1,
                "{width}x{height}: {bottom_text}"
            );
            snapshot(
                &format!("apply_confirmation_help_{width}x{height}_bottom"),
                &bottom,
            );
        }
    }

    let mut view = ApplyConfirmationViewState::default();
    assert_eq!(
        view.apply(ApplyConfirmationInput::OpenContext, "main", 0),
        None
    );
    let context = render_to_buffer((120, 40), |frame| {
        render_apply_confirmation(
            frame,
            &state,
            &PlanReviewViewState::default(),
            &view,
            confirmation_now(),
        );
    });
    assert!(buffer_text(&context).contains("Execution directory"));
    assert!(buffer_text(&context).contains("/repo/environments/production/main"));
}

#[test]
fn renders_plan_review_quit_confirmation_in_the_footer() {
    let state = review_state(review());
    let buffer = render_to_buffer((80, 24), |frame| {
        render_with_quit_confirmation(
            frame,
            &state,
            &PlanReviewViewState::default(),
            Instant::now(),
            true,
        );
    });

    snapshot("preview_80x24_quit-confirmation", &buffer);
}

#[test]
fn renders_normal_plan_height_variants_at_small_and_large_sizes() {
    struct HeightCase {
        name: &'static str,
        line_count: u16,
    }

    for height_case in [
        HeightCase {
            name: "short",
            line_count: 3,
        },
        HeightCase {
            name: "medium",
            line_count: 25,
        },
        HeightCase {
            name: "long",
            line_count: 60,
        },
    ] {
        for &(width, height) in &[(80, 24), (160, 60)] {
            let state = review_state(review_with_content(height_case.line_count, 48));
            let area = Rect::new(0, 0, width, height);
            let layout = layout(area, &PlanReviewViewState::default(), &state);
            let buffer = render_to_buffer((width, height), |frame| {
                render(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    Instant::now(),
                );
            });

            let panel_height = layout.shell.footer().bottom() - layout.shell.header().y;
            let top_margin = layout.shell.header().y;
            let bottom_margin = height.saturating_sub(layout.shell.footer().bottom());
            assert_eq!(
                top_margin,
                (height - panel_height) / 2,
                "case: {} {width}x{height}",
                height_case.name
            );
            assert!(
                top_margin.abs_diff(bottom_margin) <= 1,
                "case: {} {width}x{height}",
                height_case.name
            );
            match (height_case.name, width) {
                ("short", _) | ("medium", 160) => assert!(!layout.vertical_scrollbar()),
                ("medium", 80) | ("long", _) => assert!(layout.vertical_scrollbar()),
                _ => unreachable!(),
            }
            snapshot(
                &format!("preview_{width}x{height}_normal-{}", height_case.name),
                &buffer,
            );
        }
    }
}

#[test]
fn renders_plan_review_search_at_all_supported_sizes() {
    for &(width, height) in &SIZES {
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let state = review_state(plan);
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            Rect::new(0, 0, width, height),
            0,
            0,
            SEARCH_TERM,
            &[],
        );
        let buffer = render_to_buffer((width, height), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        snapshot(&format!("preview_{width}x{height}_search"), &buffer);
    }
}

#[test]
fn renders_apply_confirmation_at_all_supported_sizes() {
    for &(width, height) in &SIZES {
        let state = confirmation_state(review());
        let view = ApplyConfirmationViewState::default();
        let buffer = render_to_buffer((width, height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &view,
                confirmation_now(),
            );
        });

        snapshot(
            &format!("preview_{width}x{height}_apply-confirmation"),
            &buffer,
        );
    }
}

#[test]
fn renders_rejected_apply_confirmation_vrt() {
    struct RejectedCase {
        name: &'static str,
        input: &'static str,
        message: &'static str,
    }

    for case in [
        RejectedCase {
            name: "yes_instead_of_target",
            input: "yes",
            message: "Type \"main\", not \"yes\".",
        },
        RejectedCase {
            name: "misspelled_target",
            input: "mian",
            message: "Does not match \"main\".",
        },
    ] {
        for &(width, height) in &SIZES {
            let state = confirmation_state(review());
            let mut view = ApplyConfirmationViewState::default();
            for character in case.input.chars() {
                view.apply(ApplyConfirmationInput::Character(character), "main", 0);
            }
            view.apply(ApplyConfirmationInput::Confirm, "main", 0);
            let buffer = render_to_buffer((width, height), |frame| {
                render_apply_confirmation(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    &view,
                    confirmation_now(),
                );
            });

            assert!(
                buffer_text(&buffer).contains(case.message),
                "case: {}",
                case.name
            );
            if (width, height) == (80, 24) {
                snapshot(
                    &format!("apply_confirmation_rejected_{}_{width}x{height}", case.name),
                    &buffer,
                );
            }
        }
    }
}

#[test]
fn apply_confirmation_footer_enables_apply_only_for_the_expected_input() {
    let state = confirmation_state(review());
    let mut view = ApplyConfirmationViewState::default();
    let render = |view: &ApplyConfirmationViewState| {
        render_to_buffer((80, 24), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                view,
                confirmation_now(),
            );
        })
    };

    let disabled = render(&view);
    for character in "main".chars() {
        view.apply(ApplyConfirmationInput::Character(character), "main", 0);
    }
    let enabled = render(&view);

    for (buffer, foreground) in [
        (&disabled, Color::Rgb(0x6c, 0x70, 0x78)),
        (&enabled, Color::Rgb(0xe9, 0xdb, 0xdb)),
    ] {
        assert_text_prefix_uses_style(
            buffer,
            "Enter apply",
            "Enter",
            foreground,
            Color::Reset,
            Modifier::empty(),
        );
    }
}

fn review_with_content(line_count: u16, line_width: u16) -> PlanReview {
    let line = "x".repeat(usize::from(line_width));
    let text = (0..line_count)
        .map(|_| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    PlanReview::new(
        PathBuf::from("/repo"),
        "default".to_owned(),
        plan_document_with_blocks(
            text,
            vec![PlanBlock::new(
                0..usize::from(line_count),
                PlanBlockKind::Common,
            )],
        ),
        Plan::empty(),
        PlanMetadata::new(true),
        Vec::new(),
    )
}

fn press(
    view: &mut PlanReviewViewState,
    state: &ReviewSessionState,
    layout: &PlanReviewLayout,
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    let query = state.review().search_query();
    let input = key_to_input(KeyEvent::new(code, modifiers), false, !query.is_empty())
        .expect("navigation key should map to an input");
    view.apply_with_matches(
        input,
        layout.body(),
        layout.max_vertical(),
        layout.max_horizontal(),
        query,
        layout.matches(),
    );
}

// Reads each body row as text, skipping the cell that a full-width grapheme covers.
fn body_rows(buffer: &Buffer, layout: &PlanReviewLayout) -> Vec<String> {
    let body = layout.body();
    (body.y..body.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut x = body.x;
            while x < body.right() {
                let symbol = buffer.cell((x, y)).expect("body cell").symbol();
                row.push_str(symbol);
                x += symbol.cell_width().max(1);
            }
            row
        })
        .collect()
}

mod layout {
    use super::*;

    fn assert_text_color(buffer: &Buffer, text: &str, color: Color) {
        let area = buffer.area();
        for y in area.y..area.bottom() {
            let symbols = (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("plan cell").symbol())
                .collect::<Vec<_>>();
            let Some(start) = (0..symbols.len()).find(|&start| {
                symbols[start..]
                    .iter()
                    .copied()
                    .collect::<String>()
                    .starts_with(text)
            }) else {
                continue;
            };
            for offset in 0..text.chars().count() {
                let cell = buffer
                    .cell((
                        area.x + u16::try_from(start + offset).expect("plan offset"),
                        y,
                    ))
                    .expect("plan cell");
                assert_eq!(cell.fg, color, "{text}");
            }
            return;
        }
        panic!("text should be visible: {text}");
    }

    fn filter_height_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            plan_document_with_blocks(
                "api line 1\napi line 2\nworker line 1\nworker line 2\ncommon line\n".to_owned(),
                vec![
                    PlanBlock::new(0..2, PlanBlockKind::Resource),
                    PlanBlock::new(2..4, PlanBlockKind::Resource),
                    PlanBlock::new(4..5, PlanBlockKind::Common),
                ],
            ),
            Plan {
                resource_changes: vec![
                    resource_change("api", ResourceChangeKind::Update),
                    resource_change("worker", ResourceChangeKind::Update),
                ],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    fn common_only_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            plan_document_with_blocks(
                "common line 1\ncommon line 2\n".to_owned(),
                vec![PlanBlock::new(0..2, PlanBlockKind::Common)],
            ),
            Plan::empty(),
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    fn review_buffer_at(
        area: Rect,
        state: &ReviewSessionState,
        vertical: usize,
        horizontal: usize,
    ) -> (PlanReviewLayout, Buffer) {
        let layout = layout(area, &PlanReviewViewState::default(), state);
        let mut view = PlanReviewViewState::default();
        for _ in 0..vertical {
            view.apply_with_matches(
                PlanReviewInput::Down,
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                "",
                &[],
            );
        }
        for _ in 0..horizontal {
            view.apply_with_matches(
                PlanReviewInput::Right,
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                "",
                &[],
            );
        }
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, state, &view, Instant::now());
        });
        (layout, buffer)
    }

    pub(super) fn assert_scrollbar_positions(
        buffer: &Buffer,
        layout: &PlanReviewLayout,
        vertical: usize,
        horizontal: usize,
    ) {
        let body = layout.body();
        if layout.vertical_scrollbar() {
            let height = body.height + u16::from(layout.horizontal_scrollbar());
            let symbols = (body.y..body.y + height)
                .map(|y| {
                    buffer
                        .cell((body.x + body.width, y))
                        .expect("vertical cell")
                        .symbol()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            assert_eq!(symbols.first().map(String::as_str), Some("▲"));
            assert_thumb_endpoints(
                &symbols[1..symbols.len() - 1],
                "┃",
                vertical,
                layout.max_vertical(),
            );
        }
        if layout.horizontal_scrollbar() {
            let width = body.width + u16::from(layout.vertical_scrollbar());
            let symbols = (body.x..body.x + width)
                .map(|x| {
                    buffer
                        .cell((x, body.y + body.height))
                        .expect("horizontal cell")
                        .symbol()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            assert_eq!(symbols.first().map(String::as_str), Some("◀︎"));
            assert_eq!(symbols.last().map(String::as_str), Some("▶︎"));
            assert_thumb_endpoints(
                &symbols[1..symbols.len() - 1],
                "═",
                horizontal,
                layout.max_horizontal(),
            );
        }
    }

    fn assert_thumb_endpoints(
        track: &[String],
        thumb_symbol: &str,
        position: usize,
        max_position: usize,
    ) {
        let thumb_start = track
            .iter()
            .position(|symbol| symbol == thumb_symbol)
            .expect("scrollbar should contain a thumb");
        let thumb_end = track
            .iter()
            .rposition(|symbol| symbol == thumb_symbol)
            .expect("scrollbar should contain a thumb");
        if position == 0 {
            assert_eq!(thumb_start, 0);
        } else {
            assert!(thumb_start > 0);
        }
        if position == max_position {
            assert_eq!(thumb_end, track.len() - 1);
        } else {
            assert!(thumb_end < track.len() - 1);
        }
    }

    #[test]
    fn quit_confirmation_preserves_the_plan_body_and_scroll_limits() {
        let state = review_state(review());
        let area = Rect::new(0, 0, 50, 24);
        let normal = layout(area, &PlanReviewViewState::default(), &state);
        let waiting =
            layout_with_quit_confirmation(area, &PlanReviewViewState::default(), &state, true);

        assert_eq!(waiting.body(), normal.body());
        assert_eq!(waiting.max_vertical(), normal.max_vertical());
        assert_eq!(waiting.max_horizontal(), normal.max_horizontal());
    }

    #[test]
    fn filter_height_uses_the_unfiltered_plan_as_its_baseline() {
        let mut plan = filter_height_review();
        let state = review_state(plan.clone());
        let area = Rect::new(0, 0, 80, 24);
        let normal = layout(area, &PlanReviewViewState::default(), &state);

        plan.set_search_query("api".to_owned());
        let first_filter_state = review_state(plan.clone());
        plan.set_search_query("missing".to_owned());
        let second_filter_state = review_state(plan);
        let first_filter = layout(area, &PlanReviewViewState::default(), &first_filter_state);
        let second_filter = layout(area, &PlanReviewViewState::default(), &second_filter_state);
        let searching = layout(area, &searching_view(), &state);

        assert_eq!(
            first_filter.shell.header().y,
            second_filter.shell.header().y
        );
        assert_eq!(
            first_filter.shell.footer().bottom(),
            second_filter.shell.footer().bottom()
        );
        assert_eq!(first_filter.shell.content(), normal.shell.content());
        assert_eq!(first_filter.body(), normal.body());
        assert_eq!(first_filter.status(), normal.status());
        assert_eq!(first_filter.separator(), normal.separator());
        assert_eq!(
            first_filter.shell.footer().bottom(),
            normal.shell.footer().bottom()
        );
        assert_eq!(searching.shell.content(), normal.shell.content());
        assert_eq!(searching.body(), normal.body());
        assert_eq!(searching.status(), normal.status());
        assert_eq!(searching.separator(), normal.separator());
    }

    #[test]
    fn filter_input_keeps_a_common_only_plan_height_stable() {
        let mut plan = common_only_review();
        let area = Rect::new(0, 0, 80, 24);
        let empty_filter = layout(area, &searching_view(), &review_state(plan.clone()));

        plan.set_search_query("missing".to_owned());
        let typed_filter = layout(area, &searching_view(), &review_state(plan));

        assert_eq!(typed_filter.shell.header().y, empty_filter.shell.header().y);
        assert_eq!(
            typed_filter.shell.footer().bottom(),
            empty_filter.shell.footer().bottom()
        );
    }

    #[test]
    fn production_review_render_draws_shell_scrollbars_and_plan_colors() {
        let state = review_state(review());
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 80, 24);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        assert_eq!(
            layout.shell.content().bottom(),
            layout.shell.footer_separator().y
        );
        assert_eq!(
            layout.shell.footer_separator().bottom(),
            layout.shell.footer().y
        );
        assert_eq!(layout.shell.footer_separator().height, 1);
        assert!(layout.shell.content().height >= 2);
        assert!(buffer_text(&buffer).contains("q quit"));
        let text = buffer_text(&buffer);
        assert!(!text.contains("Terraform will perform the following actions:"));
        assert!(layout.vertical_scrollbar());
        assert!(layout.horizontal_scrollbar());
        let body = layout.body();
        let vertical_x = body.x.saturating_add(body.width);
        let horizontal_y = body.y.saturating_add(body.height);
        let horizontal_end_x = vertical_x;
        assert_eq!(buffer[(vertical_x, body.y)].symbol(), "▲");
        assert_eq!(
            buffer[(vertical_x, body.y)].fg,
            Color::Rgb(0x50, 0x52, 0x5e)
        );
        assert_eq!(buffer[(body.x, horizontal_y)].symbol(), "◀︎");
        assert_eq!(
            buffer[(body.x, horizontal_y)].fg,
            Color::Rgb(0x50, 0x52, 0x5e)
        );
        assert_eq!(buffer[(horizontal_end_x, horizontal_y)].symbol(), "▶︎");
        assert_eq!(
            buffer[(horizontal_end_x, horizontal_y)].fg,
            Color::Rgb(0xc0, 0xb8, 0xb0)
        );
        assert_text_color(
            &buffer,
            "~ resource \"terraform_data\" \"api\"",
            Color::Rgb(0xeb, 0xcb, 0x8b),
        );
        assert_text_color(
            &buffer,
            "# terraform_data.api will be updated in-place",
            Color::Rgb(0xe9, 0xdb, 0xdb),
        );
        assert_text_color(
            &buffer,
            "# (4 unchanged attributes hidden)",
            Color::Rgb(0xc0, 0xb8, 0xb8),
        );
        assert_text_color(&buffer, "- old_checksum", Color::Rgb(0xbf, 0x61, 0x6a));
        assert_text_color(&buffer, "+ new_checksum", Color::Rgb(0xa3, 0xbe, 0x8c));
    }

    #[test]
    fn production_review_scrollbars_reach_offsets_after_resize_and_single_overflow() {
        let state = review_state(review());
        let mut previous_body = None;
        for area in [Rect::new(0, 0, 80, 24), Rect::new(0, 0, 88, 24)] {
            let layout = layout(area, &PlanReviewViewState::default(), &state);
            assert!(layout.vertical_scrollbar());
            assert!(layout.horizontal_scrollbar());
            assert!(layout.max_vertical() > 1);
            assert!(layout.max_horizontal() > 1);
            assert_ne!(previous_body, Some(layout.body()));
            previous_body = Some(layout.body());

            for (vertical, horizontal) in [
                (0, 0),
                (layout.max_vertical() / 2, layout.max_horizontal() / 2),
                (layout.max_vertical(), layout.max_horizontal()),
            ] {
                let (layout, buffer) = review_buffer_at(area, &state, vertical, horizontal);
                assert_scrollbar_positions(&buffer, &layout, vertical, horizontal);
            }
        }

        let area = Rect::new(0, 0, 80, 24);
        let base_layout = layout(area, &PlanReviewViewState::default(), &state);
        let vertical_state = review_state(review_with_content(
            base_layout.body().height.saturating_add(2),
            base_layout.body().width,
        ));
        let (vertical_layout, vertical_buffer) = review_buffer_at(area, &vertical_state, 1, 0);
        assert_eq!(vertical_layout.max_vertical(), 1);
        assert!(!vertical_layout.horizontal_scrollbar());
        assert_scrollbar_positions(&vertical_buffer, &vertical_layout, 1, 0);

        let horizontal_state = review_state(review_with_content(
            base_layout.body().height.saturating_sub(1),
            base_layout.body().width.saturating_add(2),
        ));
        let (horizontal_layout, horizontal_buffer) =
            review_buffer_at(area, &horizontal_state, 0, 1);
        assert_eq!(horizontal_layout.max_horizontal(), 1);
        assert!(!horizontal_layout.vertical_scrollbar());
        assert_scrollbar_positions(&horizontal_buffer, &horizontal_layout, 0, 1);
    }

    #[test]
    fn normal_body_keeps_the_final_summary_without_its_trailing_blank() {
        let review = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                "body\nPlan: 1 to add, 0 to change, 0 to destroy.\n".to_owned(),
                vec![PlanBlock::new(0..3, PlanBlockKind::Common)],
                vec![
                    PlanLineKind::Body,
                    PlanLineKind::Summary,
                    PlanLineKind::Body,
                ],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        );
        assert_eq!(
            content_text(&review, false),
            ["body", "Plan: 1 to add, 0 to change, 0 to destroy."]
        );
    }

    #[test]
    fn normal_body_keeps_unknown_plan_text_and_following_lines() {
        let review = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("Plan: application text\nfollowing body text\n".to_owned()),
            Plan::empty(),
            PlanMetadata::new(false),
            Vec::new(),
        );
        assert_eq!(
            content_text(&review, false),
            ["Plan: application text", "following body text"]
        );
    }
}

mod filter {
    use super::*;

    fn zero_match_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/repo/environments/production/main"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                "Warning: synthetic diagnostic\nCommon context stays visible\n  # terraform_data.api will be created\n  + resource \"terraform_data\" \"api\" {\n  + endpoint = (known after apply)\nPlan: 1 to add, 0 to change, 0 to destroy.\n"
                    .to_owned(),
                vec![
                    PlanBlock::new(0..2, PlanBlockKind::Common),
                    PlanBlock::new(2..4, PlanBlockKind::Resource),
                    PlanBlock::new(4..5, PlanBlockKind::Output),
                    PlanBlock::new(5..7, PlanBlockKind::Common),
                ],
                vec![
                    PlanLineKind::Body,
                    PlanLineKind::Body,
                    PlanLineKind::ResourceHeader,
                    PlanLineKind::Body,
                    PlanLineKind::Body,
                    PlanLineKind::Summary,
                    PlanLineKind::Body,
                ],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            vec![Diagnostic {
                severity: DiagnosticSeverity::Warning,
                summary: "Synthetic diagnostic".to_owned(),
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }],
        )
    }

    pub(super) fn search_match_style_counts(buffer: &Buffer, query: &str) -> (usize, usize) {
        let mut normal = 0;
        let mut selected = 0;
        let query_width = query.chars().count();
        let area = buffer.area();
        for y in area.y..area.bottom() {
            let symbols = (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("match cell").symbol())
                .collect::<Vec<_>>();
            for start in 0..symbols.len().saturating_sub(query_width.saturating_sub(1)) {
                if !symbols[start..]
                    .iter()
                    .copied()
                    .collect::<String>()
                    .starts_with(query)
                {
                    continue;
                }
                let cell = buffer
                    .cell((area.x + u16::try_from(start).expect("match offset"), y))
                    .expect("match cell");
                if cell.bg == Color::Rgb(0xf4, 0x9e, 0x4c) {
                    normal += 1;
                }
                if cell.bg == Color::Rgb(0xff, 0xd0, 0x8a) {
                    assert_eq!(cell.modifier, Modifier::BOLD | Modifier::UNDERLINED);
                    selected += 1;
                }
            }
        }
        (normal, selected)
    }

    fn search_prompt(view: &PlanReviewViewState, width: u16) -> Option<(Line<'static>, u16)> {
        let (line, (cursor_start, cursor_end)) = search_query_line(view)?;
        Some((
            line.clone(),
            horizontal_offset(cursor_start, cursor_end, line.width(), width),
        ))
    }

    #[test]
    fn production_search_render_draws_search_input_and_match_color() {
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let state = review_state(plan);
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            Rect::new(0, 0, 80, 24),
            0,
            0,
            SEARCH_TERM,
            &[],
        );
        let buffer = render_to_buffer((80, 24), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        assert!(buffer_text(&buffer).contains("/terraform_data"));
        assert_text_segment_uses_style(
            &buffer,
            "/terraform_data",
            0,
            1,
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Color::Reset,
            Modifier::empty(),
        );
        assert_text_segment_uses_style(
            &buffer,
            "/terraform_data",
            1,
            SEARCH_TERM.chars().count(),
            Color::Rgb(0xe9, 0xdb, 0xdb),
            Color::Reset,
            Modifier::empty(),
        );
        assert_text_segment_uses_style(
            &buffer,
            "/terraform_data",
            1 + SEARCH_TERM.chars().count(),
            1,
            Color::Rgb(0x11, 0x14, 0x19),
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Modifier::empty(),
        );
        assert_text_prefix_uses_style(
            &buffer,
            "terraform_data.api",
            SEARCH_TERM,
            Color::Rgb(0x11, 0x14, 0x19),
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Modifier::BOLD,
        );
        let confirmed_buffer = render_to_buffer((80, 24), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        assert_text_segment_uses_style(
            &confirmed_buffer,
            "/terraform_data",
            0,
            "/terraform_data".chars().count(),
            Color::Rgb(0xc0, 0xb8, 0xb8),
            Color::Reset,
            Modifier::empty(),
        );
        assert_text_prefix_uses_style(
            &confirmed_buffer,
            "8 matches",
            "8 matches",
            Color::Rgb(0xc0, 0xb8, 0xb8),
            Color::Reset,
            Modifier::empty(),
        );
    }

    #[test]
    fn production_search_cursor_styles_full_width_and_zwj_graphemes_without_inserting_a_bar() {
        let state = review_state(review());
        let mut view = PlanReviewViewState::default();
        let body = Rect::new(0, 0, 120, 40);
        view.apply_with_matches(PlanReviewInput::SearchStart, body, 0, 0, "", &[]);
        for character in "全e\u{301}👩\u{200d}💻".chars() {
            view.apply_with_matches(PlanReviewInput::SearchChar(character), body, 0, 0, "", &[]);
        }
        view.apply_with_matches(PlanReviewInput::SearchLeft, body, 0, 0, "", &[]);

        let buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        write_buffer_captures("ux06-filter-grapheme-cursor", &buffer);
        let text = buffer_text(&buffer);
        assert!(text.contains("/全"));
        assert!(text.contains("e\u{301}"));
        assert!(text.contains("👩\u{200d}💻"));
        let search_row = (buffer.area().y..buffer.area().bottom())
            .map(|y| {
                (buffer.area().x..buffer.area().right())
                    .map(|x| buffer.cell((x, y)).expect("search row cell").symbol())
                    .collect::<String>()
            })
            .find(|row| row.contains("/全"))
            .expect("search row should be visible");
        assert!(!search_row.contains("matches"));
        assert!(!text.contains("No matches"));

        let mut found = false;
        for y in buffer.area().y..buffer.area().bottom() {
            for x in buffer.area().x..buffer.area().right() {
                let cell = buffer.cell((x, y)).expect("grapheme cursor cell");
                if cell.symbol() == "👩\u{200d}💻" {
                    assert_eq!(cell.fg, Color::Rgb(0x11, 0x14, 0x19));
                    assert_eq!(cell.bg, Color::Rgb(0xf4, 0x9e, 0x4c));
                    found = true;
                }
            }
        }
        assert!(found, "ZWJ grapheme should be rendered as the cursor");

        view.apply_with_matches(PlanReviewInput::SearchEnd, body, 0, 0, "", &[]);
        let end_buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        assert!(
            (end_buffer.area().y..end_buffer.area().bottom()).any(|y| {
                (end_buffer.area().x..end_buffer.area().right()).any(|x| {
                    let cell = end_buffer.cell((x, y)).expect("end cursor cell");
                    cell.symbol() == " "
                        && cell.fg == Color::Rgb(0x11, 0x14, 0x19)
                        && cell.bg == Color::Rgb(0xf4, 0x9e, 0x4c)
                })
            }),
            "end cursor should style a blank cell",
        );
    }

    #[test]
    fn production_partial_zwj_match_tracks_the_rendered_span_columns() {
        let line = format!("{}👩\u{200d}💻", "a".repeat(20));
        let query = "💻";
        let (_, matches) = plan_line_and_matches(&line, query, 0, None, PlanLineKind::Body);
        assert_eq!(matches, [PlanReviewMatch::new(0, 22, 24)]);

        let (selected_line, _) =
            plan_line_and_matches(&line, query, 0, matches.first(), PlanLineKind::Body);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
        Paragraph::new(vec![selected_line]).render(*buffer.area(), &mut buffer);
        let cell = buffer
            .cell((22, 0))
            .expect("selected partial grapheme cell");
        assert_eq!(cell.fg, Color::Rgb(0x11, 0x14, 0x19));
        assert_eq!(cell.bg, Color::Rgb(0xff, 0xd0, 0x8a));
        assert_eq!(cell.modifier, Modifier::BOLD | Modifier::UNDERLINED);
    }

    #[test]
    fn production_filter_selects_one_match_and_moves_with_footer_priority() {
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let state = review_state(plan);
        let area = Rect::new(0, 0, 80, 24);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        assert!(layout.matches().len() >= 2);
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        assert_eq!(view.selected(), Some(0));
        let first = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let (normal, selected) = search_match_style_counts(&first, SEARCH_TERM);
        assert_eq!(selected, 1, "normal={normal}");
        assert_eq!(normal + selected, 6);
        let footer = buffer_text(&first);
        assert!(footer.contains("Esc clear"));
        assert!(footer.contains("/ edit"));
        assert!(!footer.contains("a apply"));
        assert!(!footer.contains("y yank"));
        assert!(footer.contains("q quit"));

        view.apply_with_matches(
            PlanReviewInput::SearchNext,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        assert_eq!(view.selected(), Some(1));
        let second = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        assert_eq!(search_match_style_counts(&second, SEARCH_TERM).1, 1);

        assert_eq!(
            view.apply_with_matches(
                PlanReviewInput::SearchCancel,
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                SEARCH_TERM,
                layout.matches(),
            ),
            Some(String::new())
        );
        assert_eq!(view.selected(), None);
        assert_eq!(view.scroll(), (0, 0));
    }

    #[test]
    fn selected_filter_match_does_not_block_manual_scrolling() {
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let state = review_state(plan);
        let area = Rect::new(0, 0, 80, 24);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        view.apply_with_matches(
            PlanReviewInput::Bottom,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            SEARCH_TERM,
            layout.matches(),
        );
        assert!(layout.max_vertical() > 0);
        assert_eq!(view.selected(), Some(0));
        assert_eq!(view.scroll().0, layout.max_vertical());

        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);

        assert!(text.contains("End of synthetic plan body."), "{text}");
        assert_eq!(search_match_style_counts(&buffer, SEARCH_TERM).1, 0);
    }

    #[test]
    fn confirmed_filter_footer_only_offers_match_navigation_when_needed() {
        struct Case {
            name: &'static str,
            query: &'static str,
            expected_matches: usize,
        }

        for case in [
            Case {
                name: "zero_matches",
                query: "not-present",
                expected_matches: 0,
            },
            Case {
                name: "one_match",
                query: "endpoint",
                expected_matches: 1,
            },
            Case {
                name: "multiple_matches",
                query: SEARCH_TERM,
                expected_matches: 8,
            },
        ] {
            let mut plan = if case.expected_matches == 0 {
                zero_match_review()
            } else {
                review()
            };
            plan.set_search_query(case.query.to_owned());
            let state = review_state(plan);
            let layout = layout(
                Rect::new(0, 0, 120, 40),
                &PlanReviewViewState::default(),
                &state,
            );
            assert_eq!(
                layout.matches().len(),
                case.expected_matches,
                "case: {}",
                case.name
            );
            let expected_label = match case.expected_matches {
                0 => "No matches".to_owned(),
                1 => "1 match".to_owned(),
                count => format!("{count} matches"),
            };
            assert_eq!(
                layout
                    .footer_status
                    .as_ref()
                    .map(|status| status.0.starts_with(&expected_label)),
                Some(true),
                "case: {}",
                case.name
            );
            let buffer = render_to_buffer((120, 40), |frame| {
                render(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    Instant::now(),
                );
            });
            let text = buffer_text(&buffer);
            assert!(text.contains("clear"), "case: {}", case.name);
            assert!(text.contains("/ edit"), "case: {}", case.name);
            assert!(text.contains("y copy all"), "case: {}", case.name);
            assert!(text.contains("? help"), "case: {}", case.name);
            assert!(text.contains("a apply all"), "case: {}", case.name);
            assert!(text.contains("q quit"), "case: {}", case.name);
        }
    }

    #[test]
    fn confirmed_filter_narrow_footer_keeps_required_actions_before_match_navigation() {
        for (width, apply_allowed) in [(24, false), (40, true)] {
            for (query, plan) in [
                ("not-present", zero_match_review()),
                ("endpoint", review()),
                (SEARCH_TERM, review()),
            ] {
                let mut plan = plan.with_apply_allowed(apply_allowed);
                plan.set_search_query(query.to_owned());
                let state = review_state(plan);
                let buffer = render_to_buffer((width, 24), |frame| {
                    render(
                        frame,
                        &state,
                        &PlanReviewViewState::default(),
                        Instant::now(),
                    );
                });
                let text = buffer_text(&buffer);
                assert!(text.contains("Esc clear"), "{width}: {query}");
                assert!(text.contains("/ edit"), "{width}: {query}");
                assert!(text.contains("copy all"), "{width}: {query}");
                assert!(text.contains("? help"), "{width}: {query}\n{text}");
            }
        }
    }

    #[test]
    fn production_filter_states_show_search_hits_in_the_footer() {
        let mut input_view = PlanReviewViewState::default();
        input_view.apply_with_matches(
            PlanReviewInput::SearchStart,
            Rect::new(0, 0, 120, 40),
            0,
            0,
            "",
            &[],
        );
        let input_buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &review_state(review()), &input_view, Instant::now());
        });
        let input_text = buffer_text(&input_buffer);
        write_buffer_captures("ux02-filter-input", &input_buffer);
        assert!(!input_text.contains("Plan | Filter"));
        assert!(input_text.contains("Filter: /"));
        assert!(!input_text.contains(" matches"));
        assert!(!input_text.contains("Filter changes display only"));
        assert!(!input_text.contains("Matching changes"));

        let mut confirmed = review();
        confirmed.set_search_query("worker".to_owned());
        let confirmed_state = review_state(confirmed);
        let confirmed_buffer = render_to_buffer((120, 40), |frame| {
            render(
                frame,
                &confirmed_state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        let confirmed_text = buffer_text(&confirmed_buffer);
        write_buffer_captures("ux02-filter-confirmed", &confirmed_buffer);
        assert!(!confirmed_text.contains("Plan | Filter"));
        assert!(confirmed_text.contains("Filter: /worker"));
        assert!(confirmed_text.contains("4 matches"));
        assert!(!confirmed_text.contains("Filter changes display only"));
        assert!(!confirmed_text.contains("Matching changes"));
        assert!(!confirmed_text.contains("terraform_data.api will be updated"));

        let cleared_buffer = render_to_buffer((120, 40), |frame| {
            render(
                frame,
                &review_state(review()),
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        let cleared_text = buffer_text(&cleared_buffer);
        assert!(!cleared_text.contains("┌Plan"));
        assert!(!cleared_text.contains("Plan | Filter"));
        assert!(!cleared_text.contains(" matches"));
        assert!(!cleared_text.contains("Scope: full plan"));
    }

    #[test]
    fn filter_input_keeps_the_footer_count_and_plan_body_fixed_while_typing() {
        let area = Rect::new(0, 0, 120, 40);
        let normal_state = review_state(review());
        let normal_layout = layout(area, &PlanReviewViewState::default(), &normal_state);
        let mut positions = Vec::new();

        for query in ["a", "worker", "a-very-long-filter-query"] {
            let mut plan = review();
            plan.set_search_query(query.to_owned());
            let state = review_state(plan);
            let mut view = PlanReviewViewState::default();
            view.apply_with_matches(PlanReviewInput::SearchStart, area, 0, 0, query, &[]);
            let layout = layout(area, &view, &state);
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render(frame, &state, &view, Instant::now());
            });
            let label = layout
                .footer_status
                .as_ref()
                .expect("filter count should be visible")
                .0
                .as_str();
            let footer = layout.shell.footer();
            let x = footer.right() - u16::try_from(label.len()).expect("footer count width");
            let y = footer.y + u16::try_from(layout.shell.footer_lines().len() - 1).unwrap();
            for (offset, character) in label.chars().enumerate() {
                assert_eq!(
                    buffer
                        .cell((x + u16::try_from(offset).unwrap(), y))
                        .expect("footer count cell")
                        .symbol(),
                    character.to_string(),
                    "query: {query}"
                );
            }
            assert_eq!(layout.body().y, normal_layout.body().y, "query: {query}");
            positions.push((x + u16::try_from(label.len()).unwrap(), y));
        }

        assert!(positions.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn filter_footer_count_compacts_or_disappears_when_controls_need_room() {
        let mut plan = review();
        plan.set_search_query("worker".to_owned());
        let state = review_state(plan);

        for (width, expected) in [(24, None), (80, Some("4 matches"))] {
            let buffer = render_to_buffer((width, 24), |frame| {
                render(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    Instant::now(),
                );
            });
            let text = buffer_text(&buffer);
            if let Some(expected) = expected {
                assert!(text.contains(expected), "width: {width}");
            } else {
                assert!(!text.contains(" matches"), "width: {width}");
                assert!(!text.contains(" hits"), "width: {width}");
            }
            assert!(text.contains("Esc clear"), "width: {width}");
        }
    }

    #[test]
    fn filter_toggle_keeps_the_body_origin_at_small_and_large_terminal_sizes() {
        let normal = review_state(review());
        let mut plan = review();
        plan.set_search_query("worker".to_owned());
        let filtered = review_state(plan);

        for (width, height) in [(48, 24), (80, 24), (120, 40), (160, 60)] {
            let area = Rect::new(0, 0, width, height);
            let normal_layout = layout(area, &PlanReviewViewState::default(), &normal);
            let filtered_layout = layout(area, &PlanReviewViewState::default(), &filtered);
            assert_eq!(
                normal_layout.body().y,
                filtered_layout.body().y,
                "terminal: {width}x{height}"
            );
            assert_eq!(
                normal_layout.shell.footer().y,
                filtered_layout.shell.footer().y,
                "terminal: {width}x{height}"
            );
        }
    }

    #[test]
    fn production_filter_keeps_common_text_matches_when_no_changes_match() {
        let mut plan = zero_match_review();
        plan.set_search_query("Common".to_owned());
        let state = review_state(plan);
        let buffer = render_to_buffer((120, 40), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        let text = buffer_text(&buffer);
        write_buffer_captures("ux02-filter-zero-match", &buffer);

        assert!(text.contains("No matching changes."));
        assert!(text.contains("1 match"));
        assert!(text.contains("Warning: Synthetic diagnostic"));
        assert!(text.contains("Common context stays visible"));
        assert!(!text.contains("Plan total (full plan):"));
        assert!(!text.contains("terraform_data.api will be created"));
        assert!(!text.contains("endpoint = (known after apply)"));
    }

    #[test]
    fn production_confirmed_filter_shows_the_query_prefix_without_expanding_the_title() {
        let mut plan = review();
        plan.set_search_query("long-query-".repeat(20));
        let state = review_state(plan);
        let buffer = render_to_buffer((80, 24), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        let text = buffer_text(&buffer);
        write_buffer_captures("ux02-filter-long-query", &buffer);

        assert!(!text.contains("┌Plan | Filter"));
        assert!(text.contains("/long-query-long-query-"));
        assert!(!text.contains("Plan | Filter: long-query"));
    }

    #[test]
    fn production_filter_layout_uses_a_single_status_row_and_separator() {
        let mut plan = review();
        plan.set_search_query("worker".to_owned());
        let state = review_state(plan);
        let area = Rect::new(0, 0, 80, 20);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let status = layout.status();
        let separator = layout.separator();
        assert_eq!(status.height, 1);
        assert_eq!(separator.y, status.y + status.height);
        assert_eq!(layout.body().y, separator.y + separator.height);
        assert!(layout.body().height > 0);

        let mut view = PlanReviewViewState::default();
        for _ in 0..layout.max_vertical() {
            view.apply_with_matches(
                PlanReviewInput::Down,
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                "worker",
                &[],
            );
        }
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);
        write_buffer_captures("ux02-filter-narrow", &buffer);
        assert!(text.contains("4 matches"));
        assert!(!text.contains("Filter changes display only"));
        assert!(!text.contains("Matching changes"));

        let tiny_buffer = render_to_buffer((24, 6), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });
        write_buffer_captures("ux02-filter-terminal-too-small", &tiny_buffer);
        assert!(buffer_text(&tiny_buffer).contains("Terminal too small"));
    }

    #[test]
    fn production_filter_resize_notice_keeps_escape_cancel_available() {
        let area = Rect::new(0, 0, 24, 6);
        let state = review_state(review());
        let mut view = PlanReviewViewState::default();
        let initial_layout = layout(area, &PlanReviewViewState::default(), &state);
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            initial_layout.body(),
            initial_layout.max_vertical(),
            initial_layout.max_horizontal(),
            state.review().search_query(),
            &[],
        );

        let searching_layout = layout(area, &view, &state);
        assert_eq!(searching_layout.body().height, 0);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        write_buffer_captures("ux02-filter-input-terminal-too-small", &buffer);
        assert!(buffer_text(&buffer).contains("press Esc"));
        assert!(buffer_text(&buffer).contains("cancel"));
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                view.searching(),
                false,
            ),
            Some(PlanReviewInput::SearchCancel)
        );

        let input = key_to_input(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            view.searching(),
            false,
        )
        .expect("Esc should cancel the filter");
        assert_eq!(
            view.apply_with_matches(
                input,
                searching_layout.body(),
                searching_layout.max_vertical(),
                searching_layout.max_horizontal(),
                state.review().search_query(),
                &[],
            ),
            Some(String::new())
        );
        assert!(!view.searching());
    }

    #[test]
    fn production_confirmed_filter_resize_notice_keeps_escape_clear_available() {
        let mut plan = review();
        plan.set_search_query("worker".to_owned());
        let state = review_state(plan);
        let buffer = render_to_buffer((24, 6), |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        });

        let text = buffer_text(&buffer);
        assert!(text.contains("press Esc"));
        assert!(text.contains("clear filter"));
    }

    #[test]
    fn search_prompt_keeps_the_cursor_visible() {
        let mut view = PlanReviewViewState::default();
        let body = Rect::new(0, 0, 10, 10);
        view.apply_with_matches(PlanReviewInput::SearchStart, body, 0, 0, "", &[]);
        for character in "abcdefgh".chars() {
            view.apply_with_matches(PlanReviewInput::SearchChar(character), body, 0, 0, "", &[]);
        }
        let Some((line, horizontal)) = search_prompt(&view, 6) else {
            panic!("search prompt should be visible");
        };
        assert_eq!(line.to_string(), "/abcdefgh ");
        assert_eq!(horizontal, 4);
    }

    #[test]
    fn search_prompt_keeps_a_wide_cursor_inside_the_input_width() {
        let mut view = PlanReviewViewState::default();
        let body = Rect::new(0, 0, 10, 10);
        view.apply_with_matches(PlanReviewInput::SearchStart, body, 0, 0, "", &[]);
        for character in "aaaaaaaaaaaaaaaaaaaa😀".chars() {
            view.apply_with_matches(PlanReviewInput::SearchChar(character), body, 0, 0, "", &[]);
        }
        view.apply_with_matches(PlanReviewInput::SearchLeft, body, 0, 0, "", &[]);

        let Some((line, horizontal)) = search_prompt(&view, 22) else {
            panic!("search prompt should be visible");
        };
        assert_eq!(line.width(), 23);
        assert_eq!(horizontal, 1);
    }

    #[test]
    fn filtered_body_keeps_the_plan_summary() {
        let mut review = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                "Plan: 1 to add, 0 to change, 0 to destroy.\n".to_owned(),
                vec![PlanBlock::new(0..2, PlanBlockKind::Common)],
                vec![PlanLineKind::Summary, PlanLineKind::Body],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        );
        review.set_search_query("api".to_owned());

        let lines = content_text(&review, true);
        assert!(lines.iter().all(|line| line != "Plan total (full plan):"));
        assert!(
            lines
                .iter()
                .any(|line| line == "Plan: 1 to add, 0 to change, 0 to destroy.")
        );
    }
}

mod confirmation {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn relative_directory_uses_the_launch_root_and_shows_dot_for_the_root() {
        assert_eq!(
            context::relative_directory(Path::new("/repo"), Some(Path::new("/repo"))),
            "."
        );
        assert_eq!(
            context::relative_directory(Path::new("/repo/infra"), Some(Path::new("/repo")),),
            "./infra"
        );
        assert_eq!(
            context::relative_directory(Path::new("/other"), Some(Path::new("/repo"))),
            "/other"
        );
    }

    fn update_review(root: &str, workspace: &str) -> PlanReview {
        PlanReview::new(
            PathBuf::from(root),
            workspace.to_owned(),
            plan_document("Plan: 0 to add, 0 to change, 0 to destroy.\n".to_owned()),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Update,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    #[test]
    fn production_apply_confirmation_uses_body_input_and_accent_cursor() {
        let state = confirmation_state(review());
        let mut view = ApplyConfirmationViewState::default();
        for character in "yes".chars() {
            view.apply(ApplyConfirmationInput::Character(character), "yes", 0);
        }
        let buffer = render_to_buffer((120, 40), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &view,
                confirmation_now(),
            );
        });

        assert_text_prefix_uses_style(
            &buffer,
            "yes|",
            "yes",
            Color::Rgb(0xe9, 0xdb, 0xdb),
            Color::Reset,
            Modifier::empty(),
        );
        assert!(buffer_text(&buffer).contains("> yes|"));
        assert_text_segment_uses_style(
            &buffer,
            "yes|",
            3,
            1,
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Color::Reset,
            Modifier::empty(),
        );
    }

    #[test]
    fn production_confirmation_layout_keeps_the_footer_adjacent_to_a_compact_frame() {
        for &(width, height) in &SIZES {
            let layout = apply_confirmation_layout(
                Rect::new(0, 0, width, height),
                &confirmation_state(review()),
                confirmation_now(),
            );

            assert!(layout.renderable());
            assert_eq!(
                layout.frame().width,
                width.saturating_sub(2).min(CONFIRMATION_MAX_WIDTH)
            );
            assert_eq!(layout.footer().y, layout.frame().bottom());
            assert_eq!(layout.footer().x, layout.frame().x);
            assert_eq!(layout.inner().width, layout.frame().width - 4);
            assert_eq!(layout.inner().height, layout.frame().height - 4);
            assert_eq!(layout.input().height, 1);
            assert!(layout.frame().height < height);
        }
    }

    #[test]
    fn production_confirmation_wraps_target_and_preserves_scope_and_workspace() {
        let plan = update_review(
            "/repo/environments/production/東京/with-a-very-long-target-name-that-must-wrap",
            "staging",
        );
        let state = confirmation_state(plan);
        let area = Rect::new(0, 0, 48, 30);
        let layout = apply_confirmation_layout(area, &state, confirmation_now());
        assert!(layout.renderable());
        assert!(layout.frame().height > 12);
        let too_short = Rect::new(0, 0, area.width, 12);
        assert!(!apply_confirmation_layout(too_short, &state, confirmation_now()).renderable());
        let too_short_buffer = render_to_buffer((too_short.width, too_short.height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });
        assert!(buffer_text(&too_short_buffer).contains("Terminal too small"));

        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });
        let text = buffer_text(&buffer);
        let flat = text.replace('\n', "");
        let compact = flat
            .chars()
            .filter(|character| !character.is_whitespace() && *character != '│')
            .collect::<String>();
        assert!(text.contains("Target:"));
        assert!(text.contains("Target: staging"));
        assert!(compact.contains("Directory:"));
        assert!(compact.contains("/repo"));
        assert!(compact.contains("environments/production"));
        assert!(compact.contains("東京"));
        assert!(text.contains("Workspace: staging"));
        assert!(!text.contains("This plan includes resource deletion."));
        assert_eq!(layout.footer().y, layout.frame().bottom());
    }

    #[test]
    fn production_confirmation_requires_a_complete_footer_and_keeps_notice_below_header() {
        let state = confirmation_state(review());
        let narrow = Rect::new(0, 0, 24, 30);
        assert!(!apply_confirmation_layout(narrow, &state, confirmation_now()).renderable());

        let area = Rect::new(0, 0, 48, 12);
        let layout = apply_confirmation_layout(area, &state, confirmation_now());
        assert!(!layout.renderable());
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });
        let text = buffer_text(&buffer);
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[usize::from(layout.header().y)].contains("main [PROD]"));
        assert!(!lines[usize::from(layout.header().y)].contains("Terminal too small"));
        assert!(lines[usize::from(layout.notice().y)].contains("Terminal too small"));
    }

    #[test]
    fn production_confirmation_uses_role_styles_for_labels_values_scope_and_warning() {
        let state = confirmation_state(review());
        let buffer = render_to_buffer((120, 40), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });
        let dialog_y = (buffer.area().y..buffer.area().bottom())
            .find(|&y| {
                (buffer.area().x..buffer.area().right())
                    .map(|x| buffer.cell((x, y)).expect("dialog cell").symbol())
                    .collect::<String>()
                    .contains("Apply this reviewed plan?")
            })
            .expect("confirmation dialog title");
        assert_text_segment_uses_style_from(
            &buffer,
            dialog_y,
            "Target: main [PROD]",
            0,
            "Target: ".chars().count(),
            (
                Color::Rgb(0xc0, 0xb8, 0xb8),
                Color::Reset,
                Modifier::empty(),
            ),
        );
        assert_text_segment_uses_style_from(
            &buffer,
            dialog_y,
            "Target: main [PROD]",
            "Target: ".chars().count(),
            "main [PROD]".chars().count(),
            (
                Color::Rgb(0xe9, 0xdb, 0xdb),
                Color::Reset,
                Modifier::empty(),
            ),
        );
        assert_text_segment_uses_style_from(
            &buffer,
            dialog_y,
            "Workspace: default",
            0,
            "Workspace: ".chars().count(),
            (
                Color::Rgb(0xc0, 0xb8, 0xb8),
                Color::Reset,
                Modifier::empty(),
            ),
        );
    }

    #[test]
    fn production_confirmation_scrolls_long_input_to_the_cursor() {
        let state = confirmation_state(review());
        let mut view = ApplyConfirmationViewState::default();
        for character in "this-is-a-long-invalid-confirmation-input"
            .repeat(3)
            .chars()
        {
            view.apply(ApplyConfirmationInput::Character(character), "yes", 0);
        }
        let layout = apply_confirmation_layout(Rect::new(0, 0, 80, 24), &state, confirmation_now());
        assert!(confirmation_input_scroll(&view, layout.input().width) > 0);

        let buffer = render_to_buffer((80, 24), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &view,
                confirmation_now(),
            );
        });
        assert!(buffer_text(&buffer).contains("input|"));
    }

    fn assert_line_segments_use_styles(buffer: &Buffer, line: &str, segments: &[(&str, Style)]) {
        for &(segment, style) in segments {
            let start = line
                .find(segment)
                .expect("segment should be part of the line");
            assert_text_segment_uses_style(
                buffer,
                line,
                line[..start].chars().count(),
                segment.chars().count(),
                style.fg.unwrap_or(Color::Reset),
                style.bg.unwrap_or(Color::Reset),
                style.add_modifier,
            );
        }
    }

    #[test]
    fn apply_confirmation_colors_each_planned_change_count_by_kind() {
        let state = confirmation_state(review());
        let buffer = render_to_buffer((120, 40), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });

        assert_line_segments_use_styles(
            &buffer,
            "Plan: +1 add  ~1 update  1 replace  -1 destroy",
            &[
                ("Plan: ", theme::secondary_style()),
                ("+1 add", theme::success_style()),
                ("  ", theme::secondary_style()),
                ("~1 update", theme::warning_style()),
                ("1 replace", theme::overview_total_replace_style()),
                ("-1 destroy", theme::error_style()),
            ],
        );
        for (label, address, style) in [
            ("Destroy:", "  terraform_data.old", theme::error_style()),
            (
                "Replace:",
                "  terraform_data.worker",
                theme::overview_total_replace_style(),
            ),
        ] {
            assert_line_segments_use_styles(&buffer, label, &[(label, style)]);
            assert_line_segments_use_styles(&buffer, address, &[(address, style)]);
        }
    }

    fn create_only_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            plan_document_with_blocks(
                "+ resource \"terraform_data\" \"new\" {}".to_owned(),
                vec![PlanBlock::new(0..1, PlanBlockKind::Resource)],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.new",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    #[test]
    fn apply_confirmation_keeps_zero_change_counts_in_the_secondary_color() {
        let state = confirmation_state(create_only_review());
        let buffer = render_to_buffer((80, 24), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });

        assert_line_segments_use_styles(
            &buffer,
            "Plan: +1 add  ~0 update  0 replace  -0 destroy",
            &[
                ("+1 add", theme::success_style()),
                ("~0 update", theme::secondary_style()),
                ("0 replace", theme::secondary_style()),
                ("-0 destroy", theme::secondary_style()),
            ],
        );
    }

    #[test]
    fn apply_confirmation_shows_the_plan_age_in_whole_minutes() {
        let planned_at = Instant::now();
        let state = confirmation_state(review().with_planned_at(planned_at));
        for (elapsed, expected) in [
            (0, "Planned: <1m ago"),
            (59, "Planned: <1m ago"),
            (60, "Planned: 1m ago"),
            (59 * 60, "Planned: 59m ago"),
            (60 * 60 - 1, "Planned: 59m ago"),
            (60 * 60, "Planned: 1h 0m ago"),
            (65 * 60, "Planned: 1h 5m ago"),
        ] {
            let buffer = render_to_buffer((120, 40), |frame| {
                render_apply_confirmation(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    &ApplyConfirmationViewState::default(),
                    planned_at + Duration::from_secs(elapsed),
                );
            });
            let text = buffer_text(&buffer);

            assert!(
                text.contains(&format!("│ {expected}  ")),
                "{elapsed}s\n{text}"
            );
            let tool = text.find("│ Tool: ").expect("tool line");
            let planned = text.find("│ Planned: ").expect("planned line");
            let counts = text.find("│ Plan: +1 add").expect("change counts");
            assert!(tool < planned && planned < counts, "{elapsed}s\n{text}");
            assert_line_segments_use_styles(
                &buffer,
                expected,
                &[
                    ("Planned: ", theme::secondary_style()),
                    (
                        expected.trim_start_matches("Planned: "),
                        theme::body_style(),
                    ),
                ],
            );
        }
    }

    #[test]
    fn apply_confirmation_drops_the_plan_age_before_the_dialog_stops_fitting() {
        let state = confirmation_state(review());
        for (width, height, age_shown) in [
            (40, 24, false),
            (48, 24, false),
            (40, 29, true),
            (48, 29, true),
            (80, 24, true),
        ] {
            let layout = apply_confirmation_layout(
                Rect::new(0, 0, width, height),
                &state,
                confirmation_now(),
            );
            let text = buffer_text(&render_to_buffer((width, height), |frame| {
                render_apply_confirmation(
                    frame,
                    &state,
                    &PlanReviewViewState::default(),
                    &ApplyConfirmationViewState::default(),
                    confirmation_now(),
                );
            }));

            assert!(layout.renderable(), "{width}x{height}\n{text}");
            assert!(text.contains("> |"), "{width}x{height}\n{text}");
            assert_eq!(
                text.contains("Planned: 12m ago"),
                age_shown,
                "{width}x{height}\n{text}"
            );
        }
    }

    #[test]
    fn apply_confirmation_omits_the_plan_age_when_the_plan_time_is_unknown() {
        let state = confirmation_state(create_only_review());
        let buffer = render_to_buffer((120, 40), |frame| {
            render_apply_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });

        assert!(!buffer_text(&buffer).contains("Planned:"));
        assert_eq!(
            apply_confirmation_redraw_at(&state, confirmation_now()),
            None
        );
    }

    #[test]
    fn apply_confirmation_redraws_when_the_plan_age_reaches_the_next_minute() {
        let planned_at = Instant::now();
        let state = confirmation_state(review().with_planned_at(planned_at));
        let minutes = |minutes| planned_at + Duration::from_mins(minutes);

        for (now, expected) in [
            (planned_at, minutes(1)),
            (planned_at + Duration::from_secs(59), minutes(1)),
            (minutes(1), minutes(2)),
            (minutes(59) + Duration::from_secs(30), minutes(60)),
        ] {
            assert_eq!(apply_confirmation_redraw_at(&state, now), Some(expected));
        }
    }
}

mod overlay {
    use super::*;

    const SCROLL_CASES: [(&str, i16, usize); 2] = [("up", -1, 1), ("page_up", -8, 8)];

    fn long_context_review() -> PlanReview {
        review().with_context(
            ExecutionContext::loading("/repo/environments/production").with_variable_sources(
                VariableSources::new(
                    Vec::new(),
                    Vec::new(),
                    false,
                    (0..32).map(|index| format!("TF_VAR_{index:02}")).collect(),
                ),
            ),
        )
    }

    #[test]
    fn review_overlays_scroll_up_from_the_end_by_one_line_and_one_page() {
        let state = review_state(long_context_review());
        for (overlay, input, title) in [
            ("help", PlanReviewInput::OpenHelp, "Help"),
            ("context", PlanReviewInput::OpenContext, "Context"),
        ] {
            for (name, delta, lines) in SCROLL_CASES {
                let rows = |view: &PlanReviewViewState| {
                    dialog_body_rows(
                        &render_to_buffer((40, 15), |frame| {
                            render(frame, &state, view, Instant::now());
                        }),
                        title,
                    )
                };
                let mut view = PlanReviewViewState::default();
                view.apply_with_matches(input, Rect::default(), 0, 0, "", &[]);
                view.overlay_scroll_mut().bottom();
                let end = rows(&view);

                view.overlay_scroll_mut().scroll_by(delta);
                let scrolled = rows(&view);

                assert_dialog_scrolled_up(&format!("{overlay} {name}"), &end, &scrolled, lines);
            }
        }
    }

    #[test]
    fn confirmation_overlays_scroll_up_from_the_end_by_one_line_and_one_page() {
        let state = confirmation_state(long_context_review());
        for (overlay, input, title) in [
            ("help", ApplyConfirmationInput::OpenHelp, "Apply help"),
            ("context", ApplyConfirmationInput::OpenContext, "Context"),
        ] {
            // The apply help is shorter than a page beyond its viewport, so only line moves apply.
            let cases = if overlay == "help" {
                &SCROLL_CASES[..1]
            } else {
                &SCROLL_CASES[..]
            };
            for &(name, delta, lines) in cases {
                let rows = |view: &ApplyConfirmationViewState| {
                    dialog_body_rows(
                        &render_to_buffer((40, 16), |frame| {
                            render_apply_confirmation(
                                frame,
                                &state,
                                &PlanReviewViewState::default(),
                                view,
                                confirmation_now(),
                            );
                        }),
                        title,
                    )
                };
                let mut view = ApplyConfirmationViewState::default();
                view.apply(input, "yes", 0);
                view.overlay_scroll_mut().bottom();
                let end = rows(&view);

                view.overlay_scroll_mut().scroll_by(delta);
                let scrolled = rows(&view);

                assert_dialog_scrolled_up(&format!("{overlay} {name}"), &end, &scrolled, lines);
            }
        }
    }

    fn diagnostic_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/repo/environments/production/main"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                "Plan: 1 to add, 0 to change, 0 to destroy.\n".to_owned(),
                vec![PlanBlock::new(0..2, PlanBlockKind::Common)],
                vec![PlanLineKind::Summary, PlanLineKind::Body],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            vec![
                Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    summary: "Invalid configuration".to_owned(),
                    detail: Some("error detail line 1\nerror detail line 2".to_owned()),
                    address: None,
                    position: None,
                    source: DiagnosticSource::Terraform,
                },
                Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    summary: "Deprecated configuration".to_owned(),
                    detail: Some("warning detail line 1\nwarning detail line 2".to_owned()),
                    address: None,
                    position: None,
                    source: DiagnosticSource::Terraform,
                },
            ],
        )
    }

    fn assert_area_unchanged(before: &Buffer, after: &Buffer, area: Rect) {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(
                    before.cell((x, y)).expect("before cell"),
                    after.cell((x, y)).expect("after cell"),
                    "cell changed at ({x}, {y})"
                );
            }
        }
    }

    fn assert_flash_body_cells(before: &Buffer, after: &Buffer, body: Rect) {
        let mut flashed_cells = 0;
        for y in body.y..body.bottom() {
            if y == body.y {
                continue;
            }
            let last_content = (body.x..body.right()).rev().find(|&x| {
                let cell = before.cell((x, y)).expect("before plan cell");
                !cell.symbol().is_empty() && !cell.symbol().chars().all(char::is_whitespace)
            });
            let Some(last_content) = last_content else {
                for x in body.x..body.right() {
                    assert_eq!(
                        before.cell((x, y)).expect("before blank cell"),
                        after.cell((x, y)).expect("after blank cell"),
                        "empty row changed at ({x}, {y})"
                    );
                }
                continue;
            };
            for x in body.x..body.right() {
                let before_cell = before.cell((x, y)).expect("before plan cell");
                let after_cell = after.cell((x, y)).expect("after plan cell");

                assert_eq!(before_cell.symbol(), after_cell.symbol());
                if x > last_content || before_cell.symbol().is_empty() {
                    assert_eq!(before_cell, after_cell, "blank cell changed at ({x}, {y})");
                } else {
                    assert_eq!(after_cell.fg, Color::Rgb(0x11, 0x14, 0x19));
                    assert_eq!(after_cell.bg, Color::Rgb(0xf4, 0x9e, 0x4c));
                    flashed_cells += 1;
                }
            }
        }
        assert!(flashed_cells > 0);
    }

    fn assert_area_restored_after_flash(before: &Buffer, after: &Buffer, body: Rect) {
        for y in body.y.saturating_add(1)..body.bottom() {
            for x in body.x..body.right() {
                assert_eq!(
                    before.cell((x, y)).expect("before plan cell"),
                    after.cell((x, y)).expect("after plan cell"),
                    "plan body should restore after flash at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn quit_confirmation_replaces_the_plan_footer_and_has_a_narrow_notice() {
        let state = review_state(review_with_applyable(false));
        let buffer = render_to_buffer((80, 24), |frame| {
            render_with_quit_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
                true,
            );
        });
        let text = buffer_text(&buffer);
        assert!(text.contains("Quit Terraleph?   [Enter] Quit   [Esc] Cancel"));
        assert!(!text.contains("q quit"));

        let narrow = render_to_buffer((32, 9), |frame| {
            render_with_quit_confirmation(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
                true,
            );
        });
        assert!(buffer_text(&narrow).contains("Quit? [Enter] quit [Esc] cancel"));
    }

    #[test]
    fn narrow_quit_confirmation_keeps_its_prompt_while_a_copy_notice_is_active() {
        let now = Instant::now();
        let mut session = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/repo"),
        ));
        session::update(
            &mut session,
            Action::ReviewCompleted(review_with_applyable(false)),
            now,
        );
        session::update(
            &mut session,
            Action::CopyCompleted {
                target: CopyTarget::Plan,
                result: CopyResult::SentToTerminal,
            },
            now,
        );
        let state = session.review().expect("review should be visible");

        let text = buffer_text(&render_to_buffer((40, 12), |frame| {
            render_with_quit_confirmation(frame, state, &PlanReviewViewState::default(), now, true);
        }));

        assert!(text.contains("Quit? [Enter] quit [Esc] cancel"), "{text}");
        assert!(!text.contains("Sent to terminal clipboard."), "{text}");
    }

    #[test]
    fn copy_flash_styles_plan_cells_without_overwriting_the_review_shell() {
        let (before, flash, flash_at_100ms, after, layout) = copy_flash_buffers();

        assert_eq!(buffer_text(&flash), buffer_text(&flash_at_100ms));
        assert!(buffer_text(&flash).contains("Copied."));
        assert_text_prefix_uses_style(
            &flash,
            "terraform_data.api",
            "terraform_data",
            Color::Rgb(0x11, 0x14, 0x19),
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Modifier::empty(),
        );
        assert_text_prefix_uses_style(
            &flash_at_100ms,
            "terraform_data.api",
            "terraform_data",
            Color::Rgb(0x11, 0x14, 0x19),
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Modifier::empty(),
        );
        assert_area_restored_after_flash(&before, &after, layout.body());

        assert_area_unchanged(&before, &flash, layout.shell.header());
        assert_text_prefix_uses_style(
            &flash,
            "Copied.",
            "Copied.",
            Color::Rgb(0xf4, 0x9e, 0x4c),
            Color::Reset,
            Modifier::empty(),
        );
        assert_area_unchanged(&before, &flash, layout.status());
        assert_area_unchanged(&before, &flash, layout.separator());
        assert_area_unchanged(
            &before,
            &flash,
            Rect::new(
                layout.body().x + layout.body().width,
                layout.body().y,
                u16::from(layout.vertical_scrollbar()),
                layout.body().height,
            ),
        );
        assert_area_unchanged(
            &before,
            &flash,
            Rect::new(
                layout.body().x,
                layout.body().y + layout.body().height,
                layout.body().width + u16::from(layout.vertical_scrollbar()),
                u16::from(layout.horizontal_scrollbar()),
            ),
        );
        assert_flash_body_cells(&before, &flash, layout.body());
    }

    fn copy_flash_buffers() -> (Buffer, Buffer, Buffer, Buffer, PlanReviewLayout) {
        let area = Rect::new(0, 0, 80, 24);
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let state = review_state(plan);
        let scroll_layout = layout(area, &PlanReviewViewState::default(), &state);
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::Right,
            area,
            scroll_layout.max_vertical(),
            scroll_layout.max_horizontal(),
            SEARCH_TERM,
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            area,
            scroll_layout.max_vertical(),
            scroll_layout.max_horizontal(),
            SEARCH_TERM,
            &[],
        );
        assert_eq!(view.scroll(), (0, 1));

        let started_at = Instant::now();
        let before = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, started_at);
        });
        let mut session = SessionState::new(ExecutionState::with_context(
            started_at,
            ExecutionContext::loading("/repo"),
        ));
        session::update(
            &mut session,
            Action::ReviewCompleted(state.review().clone()),
            started_at,
        );
        let layout = layout(
            area,
            &searching_view(),
            session.review().expect("review should be visible"),
        );
        session::update(
            &mut session,
            Action::CopyCompleted {
                target: CopyTarget::Plan,
                result: CopyResult::Written,
            },
            started_at,
        );

        let flash = render_to_buffer((area.width, area.height), |frame| {
            render(
                frame,
                session.review().expect("review should be visible"),
                &view,
                started_at,
            );
        });
        let flash_at_100ms = render_to_buffer((area.width, area.height), |frame| {
            render(
                frame,
                session.review().expect("review should be visible"),
                &view,
                started_at + std::time::Duration::from_millis(100),
            );
        });
        let after = render_to_buffer((area.width, area.height), |frame| {
            render(
                frame,
                session.review().expect("review should be visible"),
                &view,
                started_at + std::time::Duration::from_millis(201),
            );
        });
        (before, flash, flash_at_100ms, after, layout)
    }

    #[test]
    fn production_review_render_orders_diagnostics_before_plan_and_styles_severity() {
        let state = review_state(diagnostic_review());
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 120, 40);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);
        let lines = text.lines().collect::<Vec<_>>();
        let position = |marker: &str| {
            lines
                .iter()
                .position(|line| line.contains(marker))
                .unwrap_or_else(|| panic!("text should be visible: {marker}"))
        };

        assert!(position("Error: Invalid configuration") < position("error detail line 1"));
        assert!(position("error detail line 2") < position("Warning: Deprecated configuration"));
        assert!(position("Warning: Deprecated configuration") < position("warning detail line 1"));
        assert!(position("Changes  +1 add") < position("Error: Invalid configuration"));
        assert_text_prefix_uses_style(
            &buffer,
            "Error: Invalid configuration",
            "Error",
            Color::Rgb(0xbf, 0x61, 0x6a),
            Color::Reset,
            Modifier::BOLD,
        );
        assert_text_prefix_uses_style(
            &buffer,
            "Warning: Deprecated configuration",
            "Warning",
            Color::Rgb(0xeb, 0xcb, 0x8b),
            Color::Reset,
            Modifier::BOLD,
        );
    }

    #[test]
    fn non_applyable_review_footer_keeps_viewing_actions_without_apply() {
        let state = review_state(review_with_applyable(false));
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 120, 40);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let footer = layout.shell.footer();
        let mut footer_text = String::new();
        for y in footer.y..footer.bottom() {
            for x in footer.x..footer.right() {
                footer_text.push_str(buffer.cell((x, y)).expect("footer cell").symbol());
            }
        }

        assert!(!footer_text.contains("a apply"), "{footer_text}");
        assert!(footer_text.contains("/ filter"), "{footer_text}");
        assert!(footer_text.contains("s overview"), "{footer_text}");
        assert!(!footer_text.contains("y copy plan"), "{footer_text}");
        assert!(footer_text.contains("q quit"), "{footer_text}");
    }

    #[test]
    fn narrow_normal_footer_keeps_required_actions_and_position_together() {
        let state = review_state(review_with_applyable(false));
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 24, 24);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let footer = layout.shell.footer();
        let footer_lines = (footer.y..footer.bottom())
            .map(|y| {
                (footer.x..footer.right())
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .expect("footer cell")
                            .symbol()
                            .to_owned()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let position = layout
            .footer_status
            .as_ref()
            .expect("plan position")
            .0
            .as_str();

        assert!(footer_lines.iter().any(|line| line.contains("/ filter")));
        assert!(footer_lines.iter().any(|line| line.contains("? help")));
        assert!(
            footer_lines
                .iter()
                .any(|line| line.contains("q quit") && line.contains(position)),
            "{footer_lines:?}"
        );
        assert!(!footer_lines.iter().any(|line| line.contains("a apply")));
    }

    #[test]
    fn narrow_environment_footer_keeps_help_and_quit() {
        let state = review_state(review_with_applyable(true));
        for width in 24..=28 {
            let mut view = PlanReviewViewState::default();
            let text = buffer_text(&render_to_buffer((width, 24), |frame| {
                render_environment(frame, frame.area(), &state, &mut view, Instant::now());
            }));

            assert!(text.contains("q quit"), "width {width}:\n{text}");
            assert!(text.contains("? help"), "width {width}:\n{text}");
        }
    }

    #[test]
    fn single_environment_footer_shows_overview_when_it_fits() {
        let wide = footer::layout_prioritized(
            footer_items(
                false,
                true,
                0,
                false,
                ReviewNavigation::Standalone,
                footer::available_width(80, Some("Line 1/43")),
            ),
            footer::available_width(80, Some("Line 1/43")),
        );
        let wide_text = wide
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(wide_text.contains("s overview"), "{wide_text}");
        assert!(wide_text.starts_with("s overview"), "{wide_text}");
        assert!(!wide_text.contains("y copy plan"), "{wide_text}");

        let narrow = footer::layout_prioritized(
            footer_items(
                false,
                true,
                0,
                false,
                ReviewNavigation::Standalone,
                footer::available_width(24, Some("L1/43")),
            ),
            footer::available_width(24, Some("L1/43")),
        );
        let narrow_text = narrow
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(!narrow_text.contains("s overview"), "{narrow_text}");
        assert!(narrow_text.contains("/ filter"), "{narrow_text}");
        assert!(narrow_text.contains("a apply"), "{narrow_text}");
        assert!(narrow_text.contains("? help"), "{narrow_text}");
        assert!(narrow_text.contains("q quit"), "{narrow_text}");
    }

    #[test]
    fn standalone_footer_prioritizes_overview_at_38_columns() {
        let state = review_state(review_with_applyable(true).with_apply_entry(true));
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 38, 24);
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let footer = layout.shell.footer();
        let footer_lines = (footer.y..footer.bottom())
            .map(|y| {
                (footer.x..footer.right())
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .expect("footer cell")
                            .symbol()
                            .to_owned()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let position = layout
            .footer_status
            .as_ref()
            .expect("plan position")
            .0
            .as_str();

        assert!(
            footer_lines.iter().any(|line| line.contains("/ filter")),
            "{footer_lines:?}"
        );
        assert!(
            footer_lines.iter().any(|line| line.contains("a apply")),
            "{footer_lines:?}"
        );
        assert!(
            footer_lines.iter().any(|line| line.contains("? help")),
            "{footer_lines:?}"
        );
        assert!(footer_lines[0].contains("s overview"), "{footer_lines:?}");
        assert!(
            footer_lines.iter().any(|line| line.contains("q quit")),
            "{footer_lines:?}"
        );
        assert!(footer_lines.iter().any(|line| line.contains(position)));
        assert!(!footer_lines.iter().any(|line| line.contains("y copy plan")));
    }

    #[test]
    fn environment_help_shows_bracket_navigation_at_supported_widths() {
        let sections = plan_help_sections(&review(), ReviewNavigation::Environments, false);
        for size in [(40, 16), (40, 24), (80, 24), (120, 40), (160, 60)] {
            let text = buffer_text(&render_to_buffer(size, |frame| {
                help_dialog::render(
                    frame,
                    frame.area(),
                    "Help",
                    &sections,
                    &DialogScroll::default(),
                );
            }));
            let compact = text
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();

            assert!(compact.contains("[/]"), "{size:?}: {text}");
            assert!(compact.contains("next"), "{size:?}: {text}");
            assert!(compact.contains("previous"), "{size:?}: {text}");
            if size.0 >= 80 {
                assert!(compact.contains("environment"), "{size:?}: {text}");
            }
        }
    }

    #[test]
    fn position_status_names_the_source_line_at_wide_and_narrow_widths() {
        assert_eq!(position_status(10, 47, 80), "Line 11/47");
        assert_eq!(position_status(10, 47, 40), "L11/47");
    }

    fn footer_text(state: &ReviewSessionState) -> String {
        let view = PlanReviewViewState::default();
        let area = Rect::new(0, 0, 120, 40);
        let layout = layout(area, &PlanReviewViewState::default(), state);
        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render(frame, state, &view, Instant::now());
        });
        let footer = layout.shell.footer();
        let mut footer_text = String::new();
        for y in footer.y..footer.bottom() {
            for x in footer.x..footer.right() {
                footer_text.push_str(buffer.cell((x, y)).expect("footer cell").symbol());
            }
        }
        footer_text
    }

    #[test]
    fn plan_entry_footer_and_help_offer_apply_for_an_applyable_saved_plan() {
        let plan = review_with_applyable(true);
        assert!(!plan.apply_entry());
        let footer = footer_text(&review_state(plan.clone()));
        assert!(footer.contains("a apply"), "{footer}");

        let sections = plan_help_sections(&plan, ReviewNavigation::Standalone, false);
        let help = buffer_text(&render_to_buffer((120, 40), |frame| {
            help_dialog::render(
                frame,
                frame.area(),
                "Help",
                &sections,
                &DialogScroll::default(),
            );
        }));
        assert!(help.contains("apply the full plan"), "{help}");
    }

    #[test]
    fn footer_hides_apply_when_the_review_cannot_apply() {
        let footer = footer_text(&review_state(review_with_apply_allowed(true, false)));
        assert!(!footer.contains("a apply"), "{footer}");
    }
}

mod large_plan {
    use ratatui::text::Span;
    use rstest::rstest;

    use super::filter::search_match_style_counts;
    use super::layout::assert_scrollbar_positions;
    use super::*;

    // Both sizes exceed u16::MAX, which used to cap the scroll offsets and limits.
    const LARGE_LINE_COUNT: usize = 70_000;
    const LARGE_COLUMN_COUNT: usize = 70_000;
    const AREA: Rect = Rect::new(0, 0, 80, 24);
    const NEEDLE: &str = "needle";

    fn attribute_line(line: usize) -> String {
        format!("      + attribute_{line:05} = \"synthetic\"")
    }

    // Full-width characters make the column count differ from the character count.
    fn wide_payload_line() -> String {
        format!(
            "      + payload = \"{}end-of-payload\"",
            "あ".repeat(LARGE_COLUMN_COUNT / 2)
        )
    }

    fn large_review(lines: &[String], query: &str) -> ReviewSessionState {
        let mut plan = PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            plan_document(lines.join("\n")),
            Plan::empty(),
            PlanMetadata::new(true),
            Vec::new(),
        );
        plan.set_search_query(query.to_owned());
        review_state(plan)
    }

    fn render_view(state: &ReviewSessionState, view: &PlanReviewViewState) -> Buffer {
        render_to_buffer((AREA.width, AREA.height), |frame| {
            render(frame, state, view, Instant::now());
        })
    }

    #[rstest]
    #[case::end(KeyCode::End, KeyModifiers::NONE)]
    #[case::alt_greater(KeyCode::Char('>'), KeyModifiers::ALT)]
    #[case::page_down(KeyCode::PageDown, KeyModifiers::NONE)]
    fn bottom_keys_reach_the_last_line_beyond_u16(
        #[case] code: KeyCode,
        #[case] modifiers: KeyModifiers,
    ) {
        let state = large_review(
            &(1..=LARGE_LINE_COUNT)
                .map(attribute_line)
                .collect::<Vec<_>>(),
            "",
        );
        let layout = layout(AREA, &PlanReviewViewState::default(), &state);
        let height = usize::from(layout.body().height);
        assert_eq!(layout.max_vertical(), LARGE_LINE_COUNT - height);
        assert!(layout.max_vertical() > usize::from(u16::MAX));
        let presses = if code == KeyCode::PageDown {
            layout.max_vertical().div_ceil(height)
        } else {
            1
        };
        let mut view = PlanReviewViewState::default();
        for _ in 0..presses {
            press(&mut view, &state, &layout, code, modifiers);
        }
        assert_eq!(view.scroll(), (layout.max_vertical(), 0));

        let buffer = render_view(&state, &view);
        let rows = body_rows(&buffer, &layout);
        assert_eq!(
            rows.first().map(|row| row.trim_end()),
            Some(attribute_line(LARGE_LINE_COUNT - height + 1).as_str())
        );
        assert_eq!(
            rows.last().map(|row| row.trim_end()),
            Some(attribute_line(LARGE_LINE_COUNT).as_str())
        );
        let text = buffer_text(&buffer);
        let position = format!("Line {}/{LARGE_LINE_COUNT}", LARGE_LINE_COUNT - height + 1);
        assert!(text.contains(&position), "{text}");
        assert_scrollbar_positions(&buffer, &layout, layout.max_vertical(), 0);
    }

    #[test]
    fn right_edge_shows_the_end_of_a_line_wider_than_u16() {
        let line = wide_payload_line();
        let state = large_review(&[attribute_line(1), line.clone()], "");
        let layout = layout(AREA, &PlanReviewViewState::default(), &state);
        let width = Line::from(line.as_str()).width();
        assert!(width > LARGE_COLUMN_COUNT);
        assert_eq!(
            layout.max_horizontal(),
            width - usize::from(layout.body().width)
        );

        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(view.scroll(), (0, layout.max_horizontal()));

        let buffer = render_view(&state, &view);
        let rows = body_rows(&buffer, &layout);
        assert!(rows[1].ends_with("あend-of-payload\""), "{}", rows[1]);
        let body = layout.body();
        let last_cell = buffer
            .cell((body.right() - 1, body.y + 1))
            .expect("last payload cell");
        assert_eq!(last_cell.fg, Color::Rgb(0xa3, 0xbe, 0x8c));
        assert_scrollbar_positions(&buffer, &layout, 0, layout.max_horizontal());
    }

    #[test]
    fn next_and_previous_reach_matches_near_the_end_and_highlight_them() {
        let mut lines = (1..=LARGE_LINE_COUNT)
            .map(attribute_line)
            .collect::<Vec<_>>();
        lines[0] = format!("      + {NEEDLE}_head = \"synthetic\"");
        lines[LARGE_LINE_COUNT - 10] = format!("      + {NEEDLE}_tail = \"synthetic\"");
        lines[LARGE_LINE_COUNT - 1] = format!("{}{NEEDLE}\"", wide_payload_line());
        let state = large_review(&lines, NEEDLE);
        let layout = layout(AREA, &PlanReviewViewState::default(), &state);
        let [_, tail, last] = layout.matches() else {
            panic!("expected three matches: {:?}", layout.matches());
        };
        assert!(tail.line() > usize::from(u16::MAX));
        assert!(last.start() > usize::from(u16::MAX));

        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('N'),
            KeyModifiers::SHIFT,
        );
        assert_eq!(view.selected(), Some(2));
        assert_eq!(
            view.scroll(),
            (layout.max_vertical(), layout.max_horizontal() - 1)
        );
        let buffer = render_view(&state, &view);
        assert_eq!(search_match_style_counts(&buffer, NEEDLE), (0, 1));
        assert!(
            body_rows(&buffer, &layout)
                .last()
                .is_some_and(|row| row.ends_with(NEEDLE)),
        );

        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('N'),
            KeyModifiers::SHIFT,
        );
        assert_eq!(view.selected(), Some(1));
        assert_eq!(view.scroll().1, tail.start());
        let buffer = render_view(&state, &view);
        assert_eq!(search_match_style_counts(&buffer, NEEDLE), (0, 1));
        assert!(buffer_text(&buffer).contains(&format!("{NEEDLE}_tail")));

        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        );
        assert_eq!(view.selected(), Some(2));
        let buffer = render_view(&state, &view);
        assert_eq!(search_match_style_counts(&buffer, NEEDLE), (0, 1));
    }

    #[test]
    fn halfwidth_sound_marks_count_as_drawn_cells_for_the_right_edge_and_matches() {
        const PREFIX: &str = "      + kana = \"";
        // Each pair draws in two cells although unicode-width counts the sound mark as zero.
        let kana = "ｶﾞｷﾟ".repeat(40);
        let line = format!("{PREFIX}{kana}{NEEDLE}\"");
        let drawn_width = PREFIX.len() + 4 * 40 + NEEDLE.len() + 1;
        assert!(Line::from(line.as_str()).width() < drawn_width);

        let state = large_review(std::slice::from_ref(&line), "");
        let raw = layout(AREA, &PlanReviewViewState::default(), &state);
        assert_eq!(
            raw.max_horizontal(),
            drawn_width - usize::from(raw.body().width)
        );
        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &raw,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        );
        let buffer = render_view(&state, &view);
        let rows = body_rows(&buffer, &raw);
        assert!(rows[0].ends_with(&format!("ｷﾟ{NEEDLE}\"")), "{}", rows[0]);

        let state = large_review(&[line], NEEDLE);
        let filtered = layout(AREA, &PlanReviewViewState::default(), &state);
        let [matched] = filtered.matches() else {
            panic!("expected one match: {:?}", filtered.matches());
        };
        assert_eq!(
            (matched.start(), matched.end()),
            (drawn_width - NEEDLE.len() - 1, drawn_width - 1)
        );
        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &filtered,
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        );
        let buffer = render_view(&state, &view);
        assert_eq!(search_match_style_counts(&buffer, NEEDLE), (0, 1));
        assert!(
            body_rows(&buffer, &filtered)
                .iter()
                .any(|row| row.ends_with(&format!("ｷﾟ{NEEDLE}"))),
        );
    }

    #[test]
    fn visible_lines_cut_wide_and_combining_graphemes_at_cell_boundaries() {
        let style = theme::warning_style();
        let lines = [Line::from(vec![
            Span::raw("aあ"),
            Span::styled("e\u{301}b", style),
        ])];
        let window = |horizontal, width| {
            let visible = visible_lines(&lines, horizontal, width);
            assert_eq!(visible.len(), 1);
            visible[0].clone()
        };

        assert_eq!(window(0, 2).to_string(), "a");
        assert_eq!(window(1, 3).to_string(), "あe\u{301}");
        // The right half of a cut wide grapheme stays blank so the columns remain aligned.
        assert_eq!(window(2, 3).to_string(), " e\u{301}b");
        assert_eq!(window(3, 2).to_string(), "e\u{301}b");
        let styled = window(3, 1);
        assert_eq!(styled.to_string(), "e\u{301}");
        assert!(styled.spans.iter().all(|span| span.style == style));
        assert!(window(5, 3).spans.is_empty());
    }
}

mod content_cache {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn cached(view: &PlanReviewViewState) -> Option<Rc<PlanContent>> {
        view.content_cache().cached_for_test()
    }

    fn prepared(view: &PlanReviewViewState, state: &ReviewSessionState) -> Rc<PlanContent> {
        Rc::clone(layout(AREA, view, state).content())
    }

    fn press(view: &mut PlanReviewViewState, state: &ReviewSessionState, input: PlanReviewInput) {
        let layout = layout(AREA, view, state);
        view.apply_with_matches(
            input,
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            state.review().search_query(),
            layout.matches(),
        );
    }

    fn draw(state: &ReviewSessionState, view: &PlanReviewViewState, size: (u16, u16)) {
        render_to_buffer(size, |frame| render(frame, state, view, Instant::now()));
    }

    // Clones the review as the session does when it moves between screens.
    fn with_query(state: &ReviewSessionState, query: &str) -> ReviewSessionState {
        let mut plan = state.review().clone();
        plan.set_search_query(query.to_owned());
        review_state(plan)
    }

    #[test]
    fn frames_and_keys_share_the_body_the_first_frame_prepared() {
        let state = with_query(&review_state(review()), SEARCH_TERM);
        let mut view = PlanReviewViewState::default();
        assert!(cached(&view).is_none());

        draw(&state, &view, (80, 24));
        let body = cached(&view).expect("the frame should keep its body on the view");
        for input in [
            PlanReviewInput::Down,
            PlanReviewInput::Right,
            PlanReviewInput::PageDown,
            PlanReviewInput::SearchNext,
            PlanReviewInput::SearchNext,
            PlanReviewInput::SearchPrevious,
            PlanReviewInput::Bottom,
        ] {
            press(&mut view, &state, input);
            draw(&state, &view, (80, 24));
            draw(&state, &view, (120, 40));
        }
        draw(&with_query(&state, SEARCH_TERM), &view, (80, 24));

        assert_eq!(view.selected(), Some(0));
        assert_ne!(view.scroll(), (0, 0));
        assert!(Rc::ptr_eq(
            &body,
            &cached(&view).expect("the body should stay cached")
        ));
    }

    #[test]
    fn the_apply_confirmation_draws_the_review_body_unscrolled_without_preparing_it_again() {
        let state = with_query(&review_state(review()), SEARCH_TERM);
        let confirmation = confirmation_state(state.review().clone());
        let draw_confirmation = |review_view: &PlanReviewViewState| {
            render_to_buffer((80, 24), |frame| {
                render_apply_confirmation(
                    frame,
                    &confirmation,
                    review_view,
                    &ApplyConfirmationViewState::default(),
                    confirmation_now(),
                );
            })
        };
        let unscrolled = draw_confirmation(&PlanReviewViewState::default());

        let mut view = PlanReviewViewState::default();
        draw_confirmation(&view);
        let body = cached(&view).expect("the confirmation should keep its body on the review view");
        let fresh = fresh_view(&confirmation, &view);
        assert!(Rc::ptr_eq(
            &body,
            &cached(&fresh).expect("the fresh view should share the body")
        ));
        assert!(Rc::ptr_eq(&body, &view_content(&confirmation, &fresh)));

        draw(&state, &view, (80, 24));
        for input in [
            PlanReviewInput::Down,
            PlanReviewInput::Right,
            PlanReviewInput::SearchNext,
        ] {
            press(&mut view, &state, input);
        }
        assert_ne!(view.scroll(), (0, 0));
        assert!(view.selected().is_some());
        for _ in 0..3 {
            assert_eq!(draw_confirmation(&view), unscrolled);
        }
        assert!(Rc::ptr_eq(
            &body,
            &cached(&view).expect("the body should stay cached")
        ));
    }

    #[test]
    fn the_body_is_prepared_again_only_when_its_inputs_change() {
        let state = review_state(review());
        let mut view = PlanReviewViewState::default();
        let mut previous = prepared(&view, &state);
        let mut assert_prepared_again =
            |view: &PlanReviewViewState, state: &ReviewSessionState, change: &str| {
                let body = prepared(view, state);
                assert!(!Rc::ptr_eq(&previous, &body), "{change}");
                assert!(Rc::ptr_eq(&body, &prepared(view, state)), "{change}");
                previous = body;
            };

        press(&mut view, &state, PlanReviewInput::SearchStart);
        assert_prepared_again(&view, &state, "opening the filter");
        let filtered = with_query(&state, "worker");
        assert_prepared_again(&view, &filtered, "changing the filter query");
        let diagnosed =
            review_state(filtered.review().clone().with_diagnostics(vec![Diagnostic {
                severity: DiagnosticSeverity::Warning,
                summary: "Synthetic diagnostic".to_owned(),
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }]));
        assert_prepared_again(&view, &diagnosed, "changing the diagnostics");
        let mut replanned = review().with_diagnostics(diagnosed.review().diagnostics().to_vec());
        replanned.set_search_query("worker".to_owned());
        assert_prepared_again(
            &view,
            &review_state(replanned),
            "a new plan with the same text",
        );
    }

    #[test]
    fn a_window_matches_the_full_body_and_highlights_only_the_selected_match() {
        let mut plan = review();
        plan.set_search_query(SEARCH_TERM.to_owned());
        let content = PlanContent::prepare(&plan, false, SEARCH_TERM);
        let selected = content.matches()[1];
        let unselected = content.lines(plan.document(), 0..usize::MAX, None);
        let full = content.lines(plan.document(), 0..usize::MAX, Some(&selected));
        assert_eq!(full.len(), content.metrics().line_count);

        for start in 0..=full.len() {
            let end = (start + 3).min(full.len());
            assert_eq!(
                content.lines(plan.document(), start..start + 3, Some(&selected)),
                full[start..end],
                "{start}"
            );
        }
        let selected_spans = |lines: &[Line<'_>]| {
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style == theme::selected_search_match_style())
                .count()
        };
        assert_eq!(selected_spans(&full), 1);
        assert_eq!(selected_spans(&full[selected.line()..=selected.line()]), 1);
        for (row, (line, unselected)) in full.iter().zip(&unselected).enumerate() {
            if row != selected.line() {
                assert_eq!(line, unselected, "{row}");
            }
        }
    }
}

mod line_styles {
    use std::ops::Range;

    use ratatui::style::Style;
    use rstest::rstest;

    use super::*;

    const AREA: (u16, u16) = (120, 60);
    const BODY: Color = Color::Rgb(0xe9, 0xdb, 0xdb);
    const SECONDARY: Color = Color::Rgb(0xc0, 0xb8, 0xb8);
    const ADD: Color = Color::Rgb(0xa3, 0xbe, 0x8c);
    const DESTROY: Color = Color::Rgb(0xbf, 0x61, 0x6a);
    const UPDATE: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
    const MATCH: (Color, Color, Modifier) = (
        Color::Rgb(0x11, 0x14, 0x19),
        Color::Rgb(0xf4, 0x9e, 0x4c),
        Modifier::BOLD,
    );
    // Heredoc lines follow Terraform 1.16 output: a created value keeps its text two columns right
    // of the marker column, and an updated value puts its markers there.
    const STYLED_PLAN: [&str; 41] = [
        "  # terraform_data.created will be created",
        "  + resource \"terraform_data\" \"created\" {",
        "      + id     = (known after apply)",
        "      + secret = (sensitive value)",
        "      + note   = \"(known after apply) -> \\\"(sensitive value)\\\"\"",
        "      + input  = <<-EOT",
        "            - dash",
        "            + plus",
        "            (sensitive value) -> (known after apply)",
        "        EOT",
        "    }",
        "",
        "  # terraform_data.updated will be updated in-place",
        "  ~ resource \"terraform_data\" \"updated\" {",
        "      ~ input  = <<-EOT",
        "            - item one",
        "          - + item two",
        "          + + item 2",
        "            plain",
        "        EOT",
        "        # (1 unchanged attribute hidden)",
        "    }",
        "",
        "  # terraform_data.destroyed will be destroyed",
        "  - resource \"terraform_data\" \"destroyed\" {",
        "      - input  = <<-EOT",
        "            - gone",
        "        EOT -> null",
        "    }",
        "",
        "  # terraform_data.replaced must be replaced",
        "-/+ resource \"terraform_data\" \"replaced\" {",
        "      ~ input = \"before\" -> \"after\" # forces replacement",
        "      ~ id    = \"replaced\" -> (known after apply)",
        "    }",
        "",
        "  # terraform_data.swapped must be replaced",
        "+/- resource \"terraform_data\" \"swapped\" {",
        "    }",
        "",
        "Plan: 2 to add, 1 to change, 3 to destroy.",
    ];

    fn styled_review(query: &str) -> ReviewSessionState {
        let line_kinds = (0..STYLED_PLAN.len())
            .map(|line| match line {
                0 | 12 | 23 | 30 | 36 => PlanLineKind::ResourceHeader,
                6..=8 | 15..=18 | 26 => PlanLineKind::HeredocBody { marker_column: 10 },
                20 => PlanLineKind::Note,
                40 => PlanLineKind::Summary,
                _ => PlanLineKind::Body,
            })
            .collect();
        let mut plan = PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                STYLED_PLAN.join("\n"),
                vec![
                    PlanBlock::new(0..12, PlanBlockKind::Resource),
                    PlanBlock::new(12..23, PlanBlockKind::Resource),
                    PlanBlock::new(23..30, PlanBlockKind::Resource),
                    PlanBlock::new(30..36, PlanBlockKind::Resource),
                    PlanBlock::new(36..40, PlanBlockKind::Resource),
                    PlanBlock::new(40..41, PlanBlockKind::Common),
                ],
                line_kinds,
            ),
            Plan::empty(),
            PlanMetadata::new(true),
            Vec::new(),
        );
        plan.set_search_query(query.to_owned());
        review_state(plan)
    }

    fn render_styled(query: &str) -> Buffer {
        let state = styled_review(query);
        render_to_buffer(AREA, |frame| {
            render(
                frame,
                &state,
                &PlanReviewViewState::default(),
                Instant::now(),
            );
        })
    }

    fn assert_segment(
        buffer: &Buffer,
        text: &str,
        segment: Range<usize>,
        style: (Color, Modifier),
    ) {
        assert_text_segment_uses_style(
            buffer,
            text,
            segment.start,
            segment.len(),
            style.0,
            Color::Reset,
            style.1,
        );
    }

    fn assert_line(buffer: &Buffer, text: &str, style: (Color, Modifier)) {
        assert_segment(buffer, text, 0..text.chars().count(), style);
    }

    #[test]
    fn plan_lines_take_the_style_of_their_change_header_note_or_heredoc_marker() {
        let buffer = render_styled("");

        for (text, style) in [
            (
                "# terraform_data.updated will be updated in-place",
                (BODY, Modifier::BOLD),
            ),
            (
                "# terraform_data.replaced must be replaced",
                (BODY, Modifier::BOLD),
            ),
            (
                "# (1 unchanged attribute hidden)",
                (SECONDARY, Modifier::empty()),
            ),
            (
                "-/+ resource \"terraform_data\" \"replaced\" {",
                (Color::Magenta, Modifier::empty()),
            ),
            (
                "+/- resource \"terraform_data\" \"swapped\" {",
                (Color::Magenta, Modifier::empty()),
            ),
            ("+ input  = <<-EOT", (ADD, Modifier::empty())),
            ("- dash", (BODY, Modifier::empty())),
            ("+ plus", (BODY, Modifier::empty())),
            ("~ input  = <<-EOT", (UPDATE, Modifier::empty())),
            ("- item one", (BODY, Modifier::empty())),
            ("- + item two", (DESTROY, Modifier::empty())),
            ("+ + item 2", (ADD, Modifier::empty())),
            ("plain", (BODY, Modifier::empty())),
            ("- input  = <<-EOT", (DESTROY, Modifier::empty())),
            ("- gone", (BODY, Modifier::empty())),
            (
                "Plan: 2 to add, 1 to change, 3 to destroy.",
                (BODY, Modifier::empty()),
            ),
        ] {
            assert_line(&buffer, text, style);
        }
        assert_segment(&buffer, "EOT -> null", 0..3, (BODY, Modifier::empty()));
    }

    #[test]
    fn filtered_heredoc_lines_keep_their_markers_under_the_match_highlight() {
        let buffer = render_styled("item");
        let text = buffer_text(&buffer);
        assert!(!text.contains("- dash"), "{text}");
        assert!(!text.contains("- gone"), "{text}");

        assert_line(
            &buffer,
            "# terraform_data.updated will be updated in-place",
            (BODY, Modifier::BOLD),
        );
        assert_segment(&buffer, "- item one", 0..2, (BODY, Modifier::empty()));
        assert_segment(&buffer, "- + item two", 0..4, (DESTROY, Modifier::empty()));
        assert_segment(&buffer, "- + item two", 8..12, (DESTROY, Modifier::empty()));
        assert_segment(&buffer, "+ + item 2", 0..4, (ADD, Modifier::empty()));
        for text in ["- item one", "- + item two", "+ + item 2"] {
            let start = text.find("item").expect("the query should be in the line");
            assert_text_segment_uses_style(&buffer, text, start, 4, MATCH.0, MATCH.1, MATCH.2);
        }
    }

    #[test]
    fn hidden_values_fade_and_change_arrows_stand_out_only_in_plan_syntax() {
        let buffer = render_styled("");

        let id = "+ id     = (known after apply)";
        assert_segment(&buffer, id, 0..11, (ADD, Modifier::empty()));
        assert_segment(&buffer, id, 11..30, (ADD, Modifier::DIM));
        let secret = "+ secret = (sensitive value)";
        assert_segment(&buffer, secret, 0..11, (ADD, Modifier::empty()));
        assert_segment(&buffer, secret, 11..28, (ADD, Modifier::DIM));
        let replaced = "~ id    = \"replaced\" -> (known after apply)";
        assert_segment(&buffer, replaced, 0..21, (UPDATE, Modifier::empty()));
        assert_segment(&buffer, replaced, 21..23, (UPDATE, Modifier::BOLD));
        assert_segment(&buffer, replaced, 23..24, (UPDATE, Modifier::empty()));
        assert_segment(&buffer, replaced, 24..43, (UPDATE, Modifier::DIM));
        let forced = "~ input = \"before\" -> \"after\" # forces replacement";
        assert_segment(&buffer, forced, 0..19, (UPDATE, Modifier::empty()));
        assert_segment(&buffer, forced, 19..21, (UPDATE, Modifier::BOLD));
        assert_segment(
            &buffer,
            forced,
            21..forced.len(),
            (UPDATE, Modifier::empty()),
        );
        assert_segment(&buffer, "EOT -> null", 0..4, (BODY, Modifier::empty()));
        assert_segment(&buffer, "EOT -> null", 4..6, (BODY, Modifier::BOLD));
        assert_segment(&buffer, "EOT -> null", 6..11, (BODY, Modifier::empty()));

        assert_line(
            &buffer,
            "+ note   = \"(known after apply) -> \\\"(sensitive value)\\\"\"",
            (ADD, Modifier::empty()),
        );
        assert_line(
            &buffer,
            "(sensitive value) -> (known after apply)",
            (BODY, Modifier::empty()),
        );
    }

    #[test]
    fn search_matches_win_over_hidden_value_and_arrow_emphasis() {
        let buffer = render_styled("known");

        let id = "+ id     = (known after apply)";
        assert_segment(&buffer, id, 11..12, (ADD, Modifier::DIM));
        assert_text_segment_uses_style(&buffer, id, 12, 5, MATCH.0, MATCH.1, MATCH.2);
        assert_segment(&buffer, id, 17..30, (ADD, Modifier::DIM));
        let replaced = "~ id    = \"replaced\" -> (known after apply)";
        assert_segment(&buffer, replaced, 21..23, (UPDATE, Modifier::BOLD));
        assert_segment(&buffer, replaced, 24..25, (UPDATE, Modifier::DIM));
        assert_text_segment_uses_style(&buffer, replaced, 25, 5, MATCH.0, MATCH.1, MATCH.2);
        assert_segment(&buffer, replaced, 30..43, (UPDATE, Modifier::DIM));

        let buffer = render_styled("-> (known");
        assert_text_segment_uses_style(&buffer, replaced, 21, 9, MATCH.0, MATCH.1, MATCH.2);
        assert_segment(&buffer, replaced, 30..43, (UPDATE, Modifier::DIM));
    }

    #[test]
    fn heredoc_markers_count_only_alone_in_the_marker_column() {
        let kind = PlanLineKind::HeredocBody { marker_column: 10 };
        for (line, expected) in [
            ("          - removed", theme::plan_marker_style(Some('-'))),
            ("          ~", theme::plan_marker_style(Some('~'))),
            ("            - text", theme::body_style()),
            ("          -text", theme::body_style()),
            ("        x - text", theme::body_style()),
            ("  -", theme::body_style()),
            ("        ああ", theme::body_style()),
        ] {
            let (styled, _) = plan_line_and_matches(line, "", 0, None, kind);
            assert!(
                styled.spans.iter().all(|span| span.style == expected),
                "{line:?}: {styled:?}"
            );
        }
    }

    #[test]
    fn list_element_heredoc_lines_color_only_markers_in_the_element_marker_column() {
        let kind = PlanLineKind::HeredocBody { marker_column: 14 };
        for (line, expected) in [
            ("                - dash", theme::body_style()),
            (
                "              - item one",
                theme::plan_marker_style(Some('-')),
            ),
            (
                "              + item two",
                theme::plan_marker_style(Some('+')),
            ),
            ("                plain", theme::body_style()),
            (
                "  # terraform_data.lookalike will be created",
                theme::body_style(),
            ),
            (
                "                a -> (known after apply)",
                theme::body_style(),
            ),
        ] {
            let (styled, _) = plan_line_and_matches(line, "", 0, None, kind);
            assert!(
                styled.spans.iter().all(|span| span.style == expected),
                "{line:?}: {styled:?}"
            );
        }

        let saturated = PlanLineKind::HeredocBody {
            marker_column: u16::MAX,
        };
        let line = format!("{}- text", " ".repeat(usize::from(u16::MAX)));
        let (styled, _) = plan_line_and_matches(&line, "", 0, None, saturated);
        assert!(
            styled
                .spans
                .iter()
                .all(|span| span.style == theme::body_style())
        );
    }

    #[test]
    fn prepared_widths_and_matches_equal_the_styled_rows() {
        // Printable ASCII rows are measured from their raw text and the other rows are styled, so
        // both ways of measuring a row must agree with the rows a frame styles. Tabs and carriage
        // returns are ASCII but take the cells of their shown spelling, so the widest row has them.
        const LINES: [&str; 5] = [
            "      ~ id    = \"a -> b\" -> (known after apply)",
            "      ~ description = \"ああ\" ->\u{301} (sensitive value)",
            "      + tags  = { \"key\" = \"value\" }",
            "      + script = \"\tcd a\t&& make\"\t\t\t\t# build\r",
            "",
        ];
        // Every query matches the one resource block, so no notice comes before the rows.
        for query in ["", "a", "->", "ああ", "make", "build\r"] {
            let plan = PlanReview::new(
                PathBuf::from("/repo"),
                "default".to_owned(),
                PlanDocument::with_blocks_and_line_kinds(
                    LINES.join("\n"),
                    vec![PlanBlock::new(0..LINES.len(), PlanBlockKind::Resource)],
                    vec![PlanLineKind::Body; LINES.len()],
                ),
                Plan::empty(),
                PlanMetadata::new(true),
                Vec::new(),
            );
            let content = PlanContent::prepare(&plan, false, query);
            let styled = LINES
                .iter()
                .take(4)
                .enumerate()
                .map(|(row, line)| {
                    plan_line_and_matches(line, query, row, None, PlanLineKind::Body)
                })
                .collect::<Vec<_>>();

            assert_eq!(content.metrics().line_count, 4, "{query:?}");
            // `\tcd a\t&& make"` ends at column 40, four tabs reach 72, and `# build^M` adds 9.
            assert_eq!(content.metrics().max_width, 81, "{query:?}");
            assert_eq!(
                content.metrics().max_width,
                styled
                    .iter()
                    .map(|(line, _)| display_width(line))
                    .max()
                    .unwrap_or(0),
                "{query:?}"
            );
            assert_eq!(
                content.matches(),
                styled
                    .into_iter()
                    .flat_map(|(_, matches)| matches)
                    .collect::<Vec<_>>(),
                "{query:?}"
            );
        }
    }

    #[rstest]
    #[case::marker_before_a_tab(
        "          + \tindented\r",
        "          +     indented^M",
        theme::plan_marker_style(Some('+'))
    )]
    #[case::tab_right_after_the_marker("          -\t", "          -     ", theme::body_style())]
    #[case::tab_before_the_marker_column_is_heredoc_text(
        "\t  ~ text",
        "          ~ text",
        theme::body_style()
    )]
    fn heredoc_markers_are_found_in_the_plan_text_before_tabs_are_expanded(
        #[case] line: &str,
        #[case] shown: &str,
        #[case] expected: Style,
    ) {
        let kind = PlanLineKind::HeredocBody { marker_column: 10 };
        let (styled, _) = plan_line_and_matches(line, "", 0, None, kind);
        assert_eq!(styled.to_string(), shown);
        assert!(
            styled.spans.iter().all(|span| span.style == expected),
            "{styled:?}"
        );
    }
}

mod control_characters {
    use std::ops::Range;

    use rstest::rstest;

    use super::*;
    use crate::app::copy::plan_effect;

    const AREA: Rect = Rect::new(0, 0, 80, 24);
    const MATCH_BACKGROUNDS: [Color; 2] =
        [Color::Rgb(0xf4, 0x9e, 0x4c), Color::Rgb(0xff, 0xd0, 0x8a)];
    const PLAN: [&str; 11] = [
        "  # terraform_data.script will be created",
        "  + resource \"terraform_data\" \"script\" {",
        "      + input = <<-EOT",
        "            build:",
        "            \tgo build ./...\t# compile the service",
        "            crlf line\r",
        "            \u{1b}[1mbold\u{1b}[0m",
        "        EOT",
        "    }",
        "",
        "Plan: 1 to add, 0 to change, 0 to destroy.",
    ];
    const SHOWN: [&str; 11] = [
        "  # terraform_data.script will be created",
        "  + resource \"terraform_data\" \"script\" {",
        "      + input = <<-EOT",
        "            build:",
        "                go build ./...  # compile the service",
        "            crlf line^M",
        "            ^[[1mbold^[[0m",
        "        EOT",
        "    }",
        "",
        "Plan: 1 to add, 0 to change, 0 to destroy.",
    ];
    const WIDEST: usize = 53;

    fn script_review(query: &str) -> ReviewSessionState {
        let line_kinds = (0..PLAN.len())
            .map(|line| match line {
                0 => PlanLineKind::ResourceHeader,
                3..=6 => PlanLineKind::HeredocBody { marker_column: 10 },
                10 => PlanLineKind::Summary,
                _ => PlanLineKind::Body,
            })
            .collect();
        let mut plan = PlanReview::new(
            PathBuf::from("/repo"),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(
                PLAN.join("\n"),
                vec![
                    PlanBlock::new(0..10, PlanBlockKind::Resource),
                    PlanBlock::new(10..11, PlanBlockKind::Common),
                ],
                line_kinds,
            ),
            Plan::empty(),
            PlanMetadata::new(true),
            Vec::new(),
        );
        plan.set_search_query(query.to_owned());
        review_state(plan)
    }

    fn draw(state: &ReviewSessionState, view: &PlanReviewViewState, area: Rect) -> Buffer {
        render_to_buffer((area.width, area.height), |frame| {
            render(frame, state, view, Instant::now());
        })
    }

    fn highlighted_columns(buffer: &Buffer, layout: &PlanReviewLayout, row: usize) -> Vec<usize> {
        let body = layout.body();
        let y = body.y + u16::try_from(row).expect("body row");
        (body.x..body.right())
            .filter(|x| MATCH_BACKGROUNDS.contains(&buffer[(*x, y)].bg))
            .map(|x| usize::from(x - body.x))
            .collect()
    }

    #[test]
    fn tabs_and_control_characters_take_the_cells_a_terminal_shows() {
        let state = script_review("");
        let view = PlanReviewViewState::default();
        let layout = layout(AREA, &view, &state);
        let rows = body_rows(&draw(&state, &view, AREA), &layout);

        assert_eq!(
            rows.iter()
                .take(SHOWN.len())
                .map(|row| row.trim_end())
                .collect::<Vec<_>>(),
            SHOWN
        );
        assert_eq!(layout.content().metrics().max_width, WIDEST);
        assert_eq!(
            SHOWN.iter().map(|line| Line::from(*line).width()).max(),
            Some(WIDEST)
        );
    }

    #[test]
    fn horizontal_scrolling_reaches_the_end_of_a_line_widened_by_tabs() {
        let area = Rect::new(0, 0, 40, 16);
        let state = script_review("");
        let layout = layout(area, &PlanReviewViewState::default(), &state);
        let width = usize::from(layout.body().width);
        assert_eq!(layout.max_horizontal(), WIDEST - width);

        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(view.scroll(), (0, WIDEST - width));
        let rows = body_rows(&draw(&state, &view, area), &layout);
        assert_eq!(rows[4], SHOWN[4][WIDEST - width..]);

        // A window starting inside a tab keeps the rest of its spaces.
        let content = layout.content();
        let lines = content.lines(state.review().document(), 0..SHOWN.len(), None);
        let window = visible_lines(&lines[4..5], 14, 12);
        assert_eq!(window[0].to_string(), "  go build .");
    }

    #[rstest]
    #[case::after_tabs("compile", 4, 34..41)]
    #[case::after_escapes("bold", 6, 17..21)]
    fn matches_highlight_the_shown_columns(
        #[case] query: &str,
        #[case] row: usize,
        #[case] columns: Range<usize>,
    ) {
        let state = script_review(query);
        let layout = layout(AREA, &PlanReviewViewState::default(), &state);
        let [matched] = layout.matches() else {
            panic!("expected one match: {:?}", layout.matches());
        };
        assert_eq!(
            (matched.line(), matched.start(), matched.end()),
            (row, columns.start, columns.end)
        );
        assert_eq!(&SHOWN[row][columns.clone()], query);

        let mut view = PlanReviewViewState::default();
        press(
            &mut view,
            &state,
            &layout,
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        );
        assert_eq!(view.selected(), Some(0));
        let buffer = draw(&state, &view, AREA);
        assert_eq!(
            highlighted_columns(&buffer, &layout, row),
            columns.collect::<Vec<_>>()
        );
    }

    #[test]
    fn filtering_and_copying_keep_the_plan_text() {
        // The shown spelling of a carriage return is not in the plan text.
        let state = script_review("^M");
        let spelled = layout(AREA, &PlanReviewViewState::default(), &state);
        assert!(spelled.matches().is_empty());
        assert!(
            buffer_text(&draw(&state, &PlanReviewViewState::default(), AREA))
                .contains("No matching changes.")
        );

        let state = script_review("line\r");
        let raw = layout(AREA, &PlanReviewViewState::default(), &state);
        let [matched] = raw.matches() else {
            panic!("expected one match: {:?}", raw.matches());
        };
        assert_eq!(
            (matched.line(), matched.start(), matched.end()),
            (5, 17, 23)
        );

        let copied = plan_effect(state.review());
        assert_eq!(copied.text(), PLAN.join("\n"));
    }
}
