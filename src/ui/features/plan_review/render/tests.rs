use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier, Style},
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
    content::{plan_line_and_matches, review_lines},
    layout::PlanReviewLayout,
    overlay::plan_help_sections,
    review_footer::{footer_items, position_status},
    status::{horizontal_offset, search_query_line},
    *,
};

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
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Note,
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
    let layout = layout(area, false, &state);
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
    view.overlay_bottom();
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

    view.overlay_bottom();
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
    session::update(
        &mut session,
        Action::OpenReviewFromOverview { address: None },
        now,
    );
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
                render_apply_confirmation(frame, from_overview, &view, confirmation_now());
            }),
            render_to_buffer(size, |frame| {
                render_apply_confirmation(frame, &direct, &view, confirmation_now());
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
    let predicted = overview_detail_layout(area, overview.review());

    session::update(
        &mut session,
        Action::OpenReviewFromOverview { address: None },
        now,
    );
    let raw = layout(
        area,
        false,
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
            render_apply_confirmation(frame, &state, &view, confirmation_now());
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

        if (width, height) == (40, 16) {
            assert!(
                !apply_confirmation_layout(
                    Rect::new(0, 0, width, height),
                    &state,
                    confirmation_now()
                )
                .renderable(),
                "{width}x{height} should exercise Help over an unrenderable confirmation"
            );
        }

        if width == 40 {
            view.overlay_bottom();
            let bottom = render_to_buffer((width, height), |frame| {
                render_apply_confirmation(frame, &state, &view, confirmation_now());
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
        render_apply_confirmation(frame, &state, &view, confirmation_now());
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
            let layout = layout(area, false, &state);
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
            render_apply_confirmation(frame, &state, &view, confirmation_now());
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
                render_apply_confirmation(frame, &state, &view, confirmation_now());
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
            render_apply_confirmation(frame, &state, view, confirmation_now());
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
fn apply_confirmation_omits_the_plan_age_when_the_plan_time_is_unknown() {
    let state = confirmation_state(create_only_review());
    let buffer = render_to_buffer((120, 40), |frame| {
        render_apply_confirmation(
            frame,
            &state,
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
        vertical: u16,
        horizontal: u16,
    ) -> (PlanReviewLayout, Buffer) {
        let layout = layout(area, false, state);
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

    fn assert_scrollbar_positions(
        buffer: &Buffer,
        layout: &PlanReviewLayout,
        vertical: u16,
        horizontal: u16,
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
                usize::from(vertical),
                usize::from(layout.max_vertical()),
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
                usize::from(horizontal),
                usize::from(layout.max_horizontal()),
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
        let normal = layout(area, false, &state);
        let waiting = layout_with_quit_confirmation(area, false, &state, true);

        assert_eq!(waiting.body(), normal.body());
        assert_eq!(waiting.max_vertical(), normal.max_vertical());
        assert_eq!(waiting.max_horizontal(), normal.max_horizontal());
    }

    #[test]
    fn filter_height_uses_the_unfiltered_plan_as_its_baseline() {
        let mut plan = filter_height_review();
        let state = review_state(plan.clone());
        let area = Rect::new(0, 0, 80, 24);
        let normal = layout(area, false, &state);

        plan.set_search_query("api".to_owned());
        let first_filter_state = review_state(plan.clone());
        plan.set_search_query("missing".to_owned());
        let second_filter_state = review_state(plan);
        let first_filter = layout(area, false, &first_filter_state);
        let second_filter = layout(area, false, &second_filter_state);
        let searching = layout(area, true, &state);

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
        let empty_filter = layout(area, true, &review_state(plan.clone()));

        plan.set_search_query("missing".to_owned());
        let typed_filter = layout(area, true, &review_state(plan));

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
        let layout = layout(area, false, &state);
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
            let layout = layout(area, false, &state);
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
        let base_layout = layout(area, false, &state);
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
        let filtered = review.document().filter(review.search_query());
        let lines = review_lines(&review, &filtered, false, "").0;

        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
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
        let filtered = review.document().filter(review.search_query());
        let lines = review_lines(&review, &filtered, false, "").0;

        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
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
                    PlanLineKind::Note,
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

    fn search_match_style_counts(buffer: &Buffer, query: &str) -> (usize, usize) {
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
        let layout = layout(area, false, &state);
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
        let layout = layout(area, false, &state);
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
            let layout = layout(Rect::new(0, 0, 120, 40), false, &state);
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
        let normal_layout = layout(area, false, &normal_state);
        let mut positions = Vec::new();

        for query in ["a", "worker", "a-very-long-filter-query"] {
            let mut plan = review();
            plan.set_search_query(query.to_owned());
            let state = review_state(plan);
            let mut view = PlanReviewViewState::default();
            view.apply_with_matches(PlanReviewInput::SearchStart, area, 0, 0, query, &[]);
            let layout = layout(area, true, &state);
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
            let normal_layout = layout(area, false, &normal);
            let filtered_layout = layout(area, false, &filtered);
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
        let layout = layout(area, false, &state);
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
        let initial_layout = layout(area, false, &state);
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            initial_layout.body(),
            initial_layout.max_vertical(),
            initial_layout.max_horizontal(),
            state.review().search_query(),
            &[],
        );

        let searching_layout = layout(area, view.searching(), &state);
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

        let filtered = review.document().filter(review.search_query());
        let lines = review_lines(&review, &filtered, true, "api").0;
        assert!(
            lines
                .iter()
                .all(|line| line.to_string() != "Plan total (full plan):")
        );
        assert!(
            lines
                .iter()
                .any(|line| { line.to_string() == "Plan: 1 to add, 0 to change, 0 to destroy." })
        );
    }
}

mod confirmation {
    use super::*;

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
            render_apply_confirmation(frame, &state, &view, confirmation_now());
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
                &ApplyConfirmationViewState::default(),
                confirmation_now(),
            );
        });
        assert!(buffer_text(&too_short_buffer).contains("Terminal too small"));

        let buffer = render_to_buffer((area.width, area.height), |frame| {
            render_apply_confirmation(
                frame,
                &state,
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
            render_apply_confirmation(frame, &state, &view, confirmation_now());
        });
        assert!(buffer_text(&buffer).contains("input|"));
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
                view.overlay_bottom();
                let end = rows(&view);

                view.scroll_overlay(delta);
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
                            render_apply_confirmation(frame, &state, view, confirmation_now());
                        }),
                        title,
                    )
                };
                let mut view = ApplyConfirmationViewState::default();
                view.apply(input, "yes", 0);
                view.overlay_bottom();
                let end = rows(&view);

                view.scroll_overlay(delta);
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
        let scroll_layout = layout(area, false, &state);
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
            true,
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
        let layout = layout(area, false, &state);
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
        let layout = layout(area, false, &state);
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
        let layout = layout(area, false, &state);
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
        let layout = layout(area, false, state);
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
