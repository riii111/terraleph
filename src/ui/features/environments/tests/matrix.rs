use std::{collections::BTreeMap, time::Instant};

use rstest::rstest;

use super::*;
use crate::app::environments::overview::OverviewRowId;
use crate::app::{
    execution::{ExecutionContext, SensitiveValue},
    plan::{
        AttributeType, ConfigurationRelationStatus, PlanAction, PlanRelations, PlanValue,
        ProviderSchema, ProviderSchemas, RelationEndpoint, RelationEvidence, RelationSource,
        ResourceChange, ResourceChangeKind, ResourceMode, ResourceSchema, StateRelationStatus,
    },
    review::{PlanBlock, PlanBlockKind, PlanDocument},
};
use crate::ui::test_support::buffer_visual_snapshot;

fn session(names: &[&str]) -> EnvironmentSession {
    EnvironmentSession::new(
        names
            .iter()
            .map(|name| Environment {
                tool: Tool::Terraform,
                availability: EnvironmentAvailability::Available(EnvironmentIdentity {
                    directory: PathBuf::from(format!("/synthetic/{name}")),
                    workspace: "default".to_owned(),
                }),
            })
            .collect(),
        false,
    )
}

fn change(address: &str, kind: ResourceChangeKind) -> ResourceChange {
    let actions = match kind {
        ResourceChangeKind::Create => vec![PlanAction::Create],
        ResourceChangeKind::Delete => vec![PlanAction::Delete],
        ResourceChangeKind::Replace => vec![PlanAction::Create, PlanAction::Delete],
        ResourceChangeKind::NoOp => vec![PlanAction::NoOp],
        _ => vec![PlanAction::Update],
    };
    ResourceChange {
        address: address.to_owned(),
        provider: None,
        resource_type: Some("terraform_data".to_owned()),
        mode: ResourceMode::Managed,
        actions,
        kind,
        before: Some(PlanValue::Object(BTreeMap::from([(
            "input".to_owned(),
            PlanValue::String("old".to_owned()),
        )]))),
        after: Some(PlanValue::Object(BTreeMap::from([(
            "input".to_owned(),
            PlanValue::String("new".to_owned()),
        )]))),
        before_sensitive: None,
        after_sensitive: None,
        after_unknown: None,
        has_previous_address: false,
        has_importing: false,
    }
}

fn complete(state: &mut EnvironmentSession, changes: Vec<ResourceChange>) {
    complete_with_relations(state, changes, PlanRelations::not_collected());
}

fn complete_with_relations(
    state: &mut EnvironmentSession,
    changes: Vec<ResourceChange>,
    relations: PlanRelations,
) {
    complete_with_schemas(state, changes, relations, None);
}

fn complete_with_schemas(
    state: &mut EnvironmentSession,
    changes: Vec<ResourceChange>,
    relations: PlanRelations,
    provider_schemas: Option<ProviderSchemas>,
) {
    let mut lines = vec![
        "Terraform will perform the following actions:".to_owned(),
        String::new(),
    ];
    let mut blocks = vec![PlanBlock::new(0..2, PlanBlockKind::Common)];
    for change in changes
        .iter()
        .filter(|change| change.kind != ResourceChangeKind::NoOp)
    {
        let start = lines.len();
        lines.extend([
            format!("# {} will change", change.address),
            "~ input = old -> new".to_owned(),
            String::new(),
        ]);
        blocks.push(PlanBlock::with_addresses(
            start..lines.len(),
            PlanBlockKind::Resource,
            vec![change.address.clone()],
        ));
    }
    complete_with_plan_document_and_relations_and_schemas(
        state,
        changes,
        lines.join("\n"),
        blocks,
        Vec::new(),
        relations,
        provider_schemas,
    );
}

fn complete_with_plan_document_and_relations_and_schemas(
    state: &mut EnvironmentSession,
    changes: Vec<ResourceChange>,
    text: String,
    blocks: Vec<PlanBlock>,
    sensitive_values: Vec<SensitiveValue>,
    relations: PlanRelations,
    provider_schemas: Option<ProviderSchemas>,
) {
    let index = state.start_next().expect("pending environment");
    let directory = state.plans()[index].directory().to_owned();
    let applyable = !changes.is_empty();
    let review = PlanReview::new(
        directory.clone(),
        "default".to_owned(),
        PlanDocument::with_blocks_and_line_kinds(text, blocks, Vec::new()),
        Plan {
            resource_changes: changes,
            ..Plan::empty()
        },
        PlanMetadata::new(applyable).with_sensitive_values(sensitive_values),
        Vec::new(),
    )
    .with_relations(relations)
    .with_provider_schemas(provider_schemas)
    .with_context(ExecutionContext::loading(directory).with_workspace("default"))
    .with_apply_entry(false);
    state.complete(
        index,
        PlanResult::Ready {
            review: Box::new(review),
            changed: true,
        },
        Vec::new(),
    );
}

fn press(view: &mut EnvironmentView, state: &mut EnvironmentSession, code: KeyCode) {
    press_event(view, state, KeyEvent::new(code, KeyModifiers::NONE));
}

fn press_event(view: &mut EnvironmentView, state: &mut EnvironmentSession, key: KeyEvent) {
    press_event_at(view, state, key, Size::new(80, 24));
}

fn press_event_at(
    view: &mut EnvironmentView,
    state: &mut EnvironmentSession,
    key: KeyEvent,
    size: Size,
) {
    if let Some(input) = view.handle_key(key, size, state) {
        match input {
            EnvironmentInput::Review(index, action) => {
                state.update_review(index, *action, Instant::now());
            }
            EnvironmentInput::Retry(index) => {
                assert!(state.retry(index));
            }
            _ => panic!("unexpected exit"),
        }
    }
}

fn press_at(view: &mut EnvironmentView, state: &mut EnvironmentSession, code: KeyCode, size: Size) {
    press_event_at(view, state, KeyEvent::new(code, KeyModifiers::NONE), size);
}

fn relation_session() -> EnvironmentSession {
    let mut state = session(&["dev", "stg", "prod"]);
    for (environment, api_kind) in [
        ResourceChangeKind::Update,
        ResourceChangeKind::Create,
        ResourceChangeKind::Replace,
    ]
    .into_iter()
    .enumerate()
    {
        let mut changes = vec![
            change("terraform_data.api", api_kind),
            change("terraform_data.worker[0]", ResourceChangeKind::Update),
            change("terraform_data.worker[1]", ResourceChangeKind::Update),
        ];
        changes.extend((0..24).map(|index| {
            change(
                &format!(
                    "terraform_data.node_{environment}_{index:02}_{}",
                    "x".repeat(100)
                ),
                ResourceChangeKind::Update,
            )
        }));
        let relations = PlanRelations::from_saved_plan(
            ConfigurationRelationStatus::Available,
            vec![RelationEvidence::resolved(
                RelationEndpoint::Instance("terraform_data.api".to_owned()),
                RelationEndpoint::Instance("terraform_data.worker[0]".to_owned()),
                RelationSource::Configuration,
            )],
            false,
        );
        complete_with_relations(&mut state, changes, relations);
    }
    state
}

fn multi_demo_session() -> EnvironmentSession {
    let provider = "terraform.io/builtin/terraform".to_owned();
    let schemas = ProviderSchemas {
        providers: BTreeMap::from([(
            provider.clone(),
            ProviderSchema {
                resources: BTreeMap::from([(
                    "terraform_data".to_owned(),
                    ResourceSchema {
                        attributes: BTreeMap::from([
                            ("input".to_owned(), AttributeType::Dynamic),
                            ("output".to_owned(), AttributeType::Dynamic),
                        ]),
                        block_types: BTreeMap::new(),
                    },
                )]),
            },
        )]),
    };
    let mut state = session(&["dev", "stg", "prod"]);
    for (name, count) in [("dev", 2), ("stg", 2), ("prod", 4)] {
        let mut changes = vec![unknown_terraform_data_update(
            "terraform_data.api",
            &provider,
        )];
        changes.extend((0..count).map(|index| {
            unknown_terraform_data_update(&format!("terraform_data.server[{index}]"), &provider)
        }));
        if name == "dev" {
            let mut dev_only = change("terraform_data.dev_only", ResourceChangeKind::Create);
            dev_only.provider = Some(provider.clone());
            dev_only.before = Some(PlanValue::Null);
            dev_only.after = Some(PlanValue::Object(BTreeMap::from([(
                "input".to_owned(),
                PlanValue::String("new".to_owned()),
            )])));
            changes.push(dev_only);
        }

        let mut evidence = vec![RelationEvidence::resolved(
            RelationEndpoint::Instance("terraform_data.api".to_owned()),
            RelationEndpoint::Instance("terraform_data.server[0]".to_owned()),
            RelationSource::Configuration,
        )];
        if name == "dev" {
            evidence.push(RelationEvidence::resolved(
                RelationEndpoint::Block("terraform_data.dev_only".to_owned()),
                RelationEndpoint::Block("terraform_data.api".to_owned()),
                RelationSource::Configuration,
            ));
        }
        let relations =
            PlanRelations::from_saved_plan(ConfigurationRelationStatus::Available, evidence, false)
                .with_state(StateRelationStatus::Available, Vec::new());
        complete_with_schemas(&mut state, changes, relations, Some(schemas.clone()));
    }
    state
}

fn unknown_terraform_data_update(address: &str, provider: &str) -> ResourceChange {
    let mut change = change(address, ResourceChangeKind::Update);
    change.provider = Some(provider.to_owned());
    change.before = Some(PlanValue::Object(BTreeMap::from([
        ("input".to_owned(), PlanValue::String("old".to_owned())),
        ("output".to_owned(), PlanValue::String("old".to_owned())),
    ])));
    change.after = Some(PlanValue::Object(BTreeMap::from([
        ("input".to_owned(), PlanValue::String("new".to_owned())),
        ("output".to_owned(), PlanValue::Null),
    ])));
    change.after_unknown = Some(PlanValue::Object(BTreeMap::from([(
        "output".to_owned(),
        PlanValue::Bool(true),
    )])));
    change
}

mod raw_plan {
    use super::*;

    fn complete_with_plan_document(
        state: &mut EnvironmentSession,
        changes: Vec<ResourceChange>,
        text: String,
        blocks: Vec<PlanBlock>,
        sensitive_values: Vec<SensitiveValue>,
    ) {
        complete_with_plan_document_and_relations_and_schemas(
            state,
            changes,
            text,
            blocks,
            sensitive_values,
            PlanRelations::not_collected(),
            None,
        );
    }

    #[test]
    fn brackets_open_the_adjacent_full_plan_from_raw_plan() {
        let mut state = session(&["dev", "prod", "stg"]);
        for _ in 0..3 {
            complete(
                &mut state,
                vec![change("terraform_data.api", ResourceChangeKind::Update)],
            );
        }
        let mut view = EnvironmentView::default();

        press(&mut view, &mut state, KeyCode::Char(']'));
        assert_eq!(view.selection.column, 1);
        press(&mut view, &mut state, KeyCode::Char('['));
        assert_eq!(view.selection.column, 0);
        press(&mut view, &mut state, KeyCode::Char(']'));
        assert_eq!(view.selection.column, 1);

        press(&mut view, &mut state, KeyCode::Char('v'));
        assert_eq!(view.selection.raw, Some(1));
        assert!(
            render_text(&mut view, &state, (80, 24)).contains("# terraform_data.api will change")
        );

        press(&mut view, &mut state, KeyCode::Char(']'));
        assert_eq!(view.selection.raw, Some(2));
        assert!(
            render_text(&mut view, &state, (80, 24)).contains("# terraform_data.api will change")
        );

        press_event(
            &mut view,
            &mut state,
            KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
        );
        assert_eq!(view.selection.raw, Some(1));
        assert!(
            render_text(&mut view, &state, (80, 24)).contains("# terraform_data.api will change")
        );
    }

    fn resources(names: &[&str]) -> Vec<ResourceChange> {
        names
            .iter()
            .map(|name| {
                change(
                    &format!("terraform_data.{name}"),
                    ResourceChangeKind::Update,
                )
            })
            .collect()
    }

    const TALL_PLAN: [&str; 10] = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9"];

    #[test]
    fn brackets_open_the_next_plan_at_the_resource_that_is_on_top() {
        let mut state = session(&["dev", "stg", "prod"]);
        complete(&mut state, resources(&TALL_PLAN));
        complete(&mut state, resources(&TALL_PLAN));
        complete(&mut state, resources(&["r1"]));
        let mut view = EnvironmentView::default();
        let size = Size::new(80, 24);

        press_at(&mut view, &mut state, KeyCode::Char('v'), size);
        for _ in 0..8 {
            press_at(&mut view, &mut state, KeyCode::Down, size);
        }
        assert_eq!(view.reviews[0].scroll().0, 8);
        press_at(&mut view, &mut state, KeyCode::Char(']'), size);

        // Line 8 of the plan text is where the r2 block starts.
        assert_eq!(
            (view.selection.raw, view.reviews[1].scroll().0),
            (Some(1), 8)
        );

        press_at(&mut view, &mut state, KeyCode::Char(']'), size);

        assert_eq!(
            (view.selection.raw, view.reviews[2].scroll().0),
            (Some(2), 0)
        );
    }

    #[test]
    fn brackets_at_the_first_and_last_environment_keep_the_plan_where_it_is() {
        let mut state = session(&["dev", "prod"]);
        complete(&mut state, resources(&TALL_PLAN));
        complete(&mut state, resources(&TALL_PLAN));
        let mut view = EnvironmentView::default();
        let size = Size::new(80, 24);
        press_at(&mut view, &mut state, KeyCode::Char('v'), size);
        for _ in 0..4 {
            press_at(&mut view, &mut state, KeyCode::Down, size);
        }

        let input = view.handle_key(
            KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
            size,
            &state,
        );

        assert!(input.is_none());
        assert_eq!(
            (view.selection.raw, view.reviews[0].scroll().0),
            (Some(0), 4)
        );
    }

    fn plan_text_column(text: &str) -> usize {
        text.lines()
            .find_map(|line| {
                line.find("# terraform_data.r0")
                    .map(|byte| line[..byte].chars().count())
            })
            .expect("the plan text should be visible")
    }

    #[test]
    fn full_plan_keeps_the_environment_list_and_b_hides_it() {
        let mut state = session(&["dev", "stg"]);
        complete(&mut state, resources(&["r0"]));
        complete(&mut state, resources(&["r0"]));
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);
        press_at(&mut view, &mut state, KeyCode::Char('v'), size);

        let with_list = render_text(&mut view, &state, (120, 40));
        press_at(&mut view, &mut state, KeyCode::Char('b'), size);
        let without_list = render_text(&mut view, &state, (120, 40));

        assert!(with_list.contains("[1] Envs"), "{with_list}");
        assert!(with_list.contains("> [x] dev"), "{with_list}");
        assert!(plan_text_column(&with_list) >= 25, "{with_list}");
        assert!(!without_list.contains("[1] Envs"), "{without_list}");
        assert!(plan_text_column(&without_list) < 5, "{without_list}");
    }

    #[test]
    fn full_plan_marks_the_open_environment_in_the_list_when_brackets_switch() {
        let mut state = session(&["dev", "stg"]);
        complete(&mut state, resources(&["r0"]));
        complete(&mut state, resources(&["r0"]));
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);
        press_at(&mut view, &mut state, KeyCode::Char('v'), size);
        press_at(&mut view, &mut state, KeyCode::Char(']'), size);

        let text = render_text(&mut view, &state, (120, 40));

        assert!(text.contains("> [x] stg"), "{text}");
        assert!(text.contains("  [x] dev"), "{text}");
    }

    #[test]
    fn full_plan_header_leaves_the_tool_to_the_top_row() {
        let mut state = session(&["dev", "stg"]);
        complete(&mut state, resources(&["r0"]));
        complete(&mut state, resources(&["r0"]));
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);
        press_at(&mut view, &mut state, KeyCode::Char('v'), size);

        let text = render_text(&mut view, &state, (120, 40));

        assert!(text.contains("Target: dev"), "{text}");
        assert!(!text.contains("Tool:"), "{text}");
    }

    #[test]
    fn unavailable_environment_opens_its_state_dialog_and_raw_plan_after_completion() {
        let mut state = session(&["a", "b", "c"]);
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::NoOp)],
        );
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        press_at(&mut view, &mut state, KeyCode::Enter, size);
        assert_eq!(view.selection.raw, Some(1));
        assert!(
            render_text(&mut view, &state, (120, 40))
                .contains("Terraform will perform the following actions")
        );
        press_at(&mut view, &mut state, KeyCode::Esc, size);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        press_at(&mut view, &mut state, KeyCode::Enter, size);
        assert!(render_text(&mut view, &state, (120, 40)).contains("Pending"));
        assert!(view.selection.raw.is_none());
        press_at(&mut view, &mut state, KeyCode::Esc, size);
        complete(&mut state, Vec::new());
        press_at(&mut view, &mut state, KeyCode::Enter, size);
        let rendered = render_text(&mut view, &state, (120, 40));
        assert!(
            rendered.contains("Terraform will perform the following actions"),
            "{rendered}"
        );
        assert_eq!(view.selection.raw, Some(2));
    }

    #[test]
    fn matrix_enter_opens_the_selected_cell_and_never_falls_back_to_another_resource() {
        let mut state = session(&["dev", "prod"]);
        complete(
            &mut state,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.beta", ResourceChangeKind::Update)],
        );
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));

        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.alpha"
        ));
        press(&mut view, &mut state, KeyCode::Char(']'));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, None);
        assert!(render_text(&mut view, &state, (80, 24)).contains("prod has no resource"));

        press(&mut view, &mut state, KeyCode::Down);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.beta"
        ));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, Some(1));
        assert!(view.reviews[1].scroll().0 > 0);
        assert!(
            render_text(&mut view, &state, (80, 24)).contains("# terraform_data.beta will change")
        );
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.selection.raw, None);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.beta"
        ));
    }

    #[test]
    fn matrix_enter_stays_in_overview_when_the_selected_plan_is_not_ready() {
        let mut state = session(&["dev", "pending"]);
        complete(
            &mut state,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
        );
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Char(']'));
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, None);
        let rendered = render_text(&mut view, &state, (80, 24));
        assert!(
            rendered.contains("has not finished plan acquisition"),
            "{rendered}"
        );
    }

    #[test]
    fn matrix_enter_opens_the_source_block_and_missing_blocks_stay_in_overview() {
        let mut state = session(&["dev"]);
        complete(
            &mut state,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
        );
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, Some(0));
        let block_line = state.plans()[0]
            .review()
            .unwrap()
            .review()
            .document()
            .block_for_address("terraform_data.alpha")
            .unwrap()
            .lines()
            .start;
        assert_eq!(view.reviews[0].scroll().0, block_line);

        let mut missing = session(&["dev"]);
        complete_with_plan_document(
            &mut missing,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
            "Synthetic plan text without a resource block".to_owned(),
            vec![PlanBlock::new(0..1, PlanBlockKind::Common)],
            Vec::new(),
        );
        let mut missing_view = EnvironmentView::default();
        let _ = render_text(&mut missing_view, &missing, (80, 24));
        press(&mut missing_view, &mut missing, KeyCode::Enter);

        assert_eq!(missing_view.selection.raw, None);
        assert!(
            render_text(&mut missing_view, &missing, (80, 24))
                .contains("No source block for terraform_data.alpha in dev.")
        );
    }

    #[test]
    fn grouped_matrix_enter_opens_the_first_matching_resource_and_reports_the_count() {
        let mut state = session(&["dev", "prod"]);
        for _ in 0..2 {
            complete(
                &mut state,
                (0..2)
                    .map(|index| {
                        change(
                            &format!("terraform_data.server[{index}]"),
                            ResourceChangeKind::Update,
                        )
                    })
                    .collect(),
            );
        }
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Group(_), None))
        ));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, Some(0));
        let block_line = state.plans()[0]
            .review()
            .unwrap()
            .review()
            .document()
            .block_for_address("terraform_data.server[0]")
            .unwrap()
            .lines()
            .start;
        assert_eq!(view.reviews[0].scroll().0, block_line);
        assert!(
            render_text(&mut view, &state, (80, 24))
                .contains("# terraform_data.server[0] will change")
        );
        press(&mut view, &mut state, KeyCode::Esc);

        assert_eq!(view.selection.raw, None);
        assert!(
            render_text(&mut view, &state, (80, 24))
                .contains("Opening the first of 2 matching resources")
        );
    }

    #[test]
    fn grouped_matrix_enter_finds_a_later_member_when_the_first_has_no_source_block() {
        let mut state = session(&["dev", "prod"]);
        let changes = (0..2)
            .map(|index| {
                change(
                    &format!("terraform_data.server[{index}]"),
                    ResourceChangeKind::Update,
                )
            })
            .collect();
        let lines = [
            "Terraform will perform the following actions:".to_owned(),
            String::new(),
            "# terraform_data.server[1] will change".to_owned(),
            "~ input = old -> new".to_owned(),
            String::new(),
        ];
        let blocks = vec![
            PlanBlock::new(0..2, PlanBlockKind::Common),
            PlanBlock::with_addresses(
                2..lines.len(),
                PlanBlockKind::Resource,
                vec!["terraform_data.server[1]".to_owned()],
            ),
        ];
        complete_with_plan_document(&mut state, changes, lines.join("\n"), blocks, Vec::new());
        complete(
            &mut state,
            (0..2)
                .map(|index| {
                    change(
                        &format!("terraform_data.server[{index}]"),
                        ResourceChangeKind::Update,
                    )
                })
                .collect(),
        );

        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        assert!(matches!(
            view.matrix.selected_item(0),
            Some(MatrixSelectedItem::Resource {
                cell: Some(MatrixCell {
                    state: CellState::Change { .. },
                    ..
                }),
                grouped: true,
                ..
            })
        ));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, Some(0));
        let block_line = state.plans()[0]
            .review()
            .unwrap()
            .review()
            .document()
            .block_for_address("terraform_data.server[1]")
            .unwrap()
            .lines()
            .start;
        assert_eq!(view.reviews[0].scroll().0, block_line);
        press(&mut view, &mut state, KeyCode::Esc);
        assert!(
            render_text(&mut view, &state, (80, 24))
                .contains("Opening the first of 1 matching resources: terraform_data.server[1].")
        );
    }

    #[test]
    fn grouped_matrix_reports_one_match_for_an_asymmetric_group() {
        let mut state = session(&["dev", "prod"]);
        complete(
            &mut state,
            vec![change(
                "terraform_data.server[0]",
                ResourceChangeKind::Update,
            )],
        );
        complete(
            &mut state,
            (0..2)
                .map(|index| {
                    change(
                        &format!("terraform_data.server[{index}]"),
                        ResourceChangeKind::Update,
                    )
                })
                .collect(),
        );

        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Group(_), None))
        ));
        press(&mut view, &mut state, KeyCode::Enter);

        assert_eq!(view.selection.raw, Some(0));
        press(&mut view, &mut state, KeyCode::Esc);
        assert!(
            render_text(&mut view, &state, (80, 24))
                .contains("Opening the first of 1 matching resources: terraform_data.server[0].")
        );
    }

    #[test]
    fn raw_filter_escape_clears_the_query_before_returning_to_overview() {
        let mut state = session(&["a", "b"]);
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Update)],
        );
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('v'));
        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "api".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        press(&mut view, &mut state, KeyCode::Esc);

        assert_eq!(view.selection.raw, Some(0));
        assert!(
            state.plans()[0]
                .review()
                .unwrap()
                .review()
                .search_query()
                .is_empty()
        );
        press(&mut view, &mut state, KeyCode::Esc);
        assert!(view.selection.raw.is_none());
    }
}

mod relations {
    use super::*;

    #[rstest]
    #[case::memo(165, 50)]
    #[case::medium(120, 40)]
    #[case::small(80, 24)]
    #[case::narrow(40, 16)]
    fn relations_pane_shows_the_selected_environment_at_supported_sizes(
        #[case] width: u16,
        #[case] height: u16,
    ) {
        let state = relation_session();
        let mut view = EnvironmentView::default();
        let rendered = render_text(&mut view, &state, (width, height));

        assert!(
            rendered.contains("[3] Relations"),
            "{width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("whole env"),
            "{width}x{height}: {rendered}"
        );
        assert!(rendered.contains("A ──> B"), "{width}x{height}: {rendered}");
        if width >= 120 {
            assert!(
                rendered.contains("[1] Envs"),
                "{width}x{height}: {rendered}"
            );
        } else {
            assert!(
                !rendered.contains("[1] Envs"),
                "{width}x{height}: {rendered}"
            );
        }
    }

    #[test]
    fn row_unchanged_in_the_shown_environment_is_named_and_not_highlighted() {
        let mut state = session(&["dev", "prod"]);
        complete(
            &mut state,
            vec![
                change("terraform_data.api", ResourceChangeKind::Update),
                change("terraform_data.worker", ResourceChangeKind::Update),
            ],
        );
        complete(
            &mut state,
            vec![
                change("terraform_data.api", ResourceChangeKind::NoOp),
                change("terraform_data.worker", ResourceChangeKind::Update),
            ],
        );
        let size = Size::new(165, 50);
        let mut view = EnvironmentView::default();
        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::Home, size);
        let relations = |text: &str| {
            text.lines()
                .skip_while(|line| !line.contains("[3] Relations"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };

        let dev = render_text(&mut view, &state, (size.width, size.height));
        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        let prod = render_text(&mut view, &state, (size.width, size.height));

        assert!(dev.contains("> terraform_data.api"), "{dev}");
        assert!(
            relations(&dev)
                .iter()
                .any(|line| line.contains("> ~ terraform_data.api")),
            "{dev}"
        );
        assert!(
            prod.contains("prod · whole env · selected row unchanged in prod"),
            "{prod}"
        );
        assert!(
            relations(&prod).iter().all(|line| !line.contains("│> ")),
            "{prod}"
        );
    }

    #[test]
    fn multi_demo_member_missing_from_the_shown_environment_is_not_highlighted_there() {
        let mut state = multi_demo_session();
        let wide = Size::new(165, 50);
        let mut view = EnvironmentView::default();
        press_at(&mut view, &mut state, KeyCode::Char('2'), wide);
        press_at(&mut view, &mut state, KeyCode::End, wide);
        press_at(&mut view, &mut state, KeyCode::Char(' '), wide);
        press_at(&mut view, &mut state, KeyCode::Down, wide);
        press_at(&mut view, &mut state, KeyCode::Down, wide);
        press_at(&mut view, &mut state, KeyCode::Char(' '), wide);

        let group = render_text(&mut view, &state, (wide.width, wide.height));
        press_at(&mut view, &mut state, KeyCode::End, wide);
        let missing_member = render_text(&mut view, &state, (wide.width, wide.height));

        assert!(group.contains("> ~ terraform_data.server[*] ×2"), "{group}");
        assert!(!group.contains("selected row not in dev"), "{group}");
        assert!(
            missing_member.contains(">       terraform_data.server[3]"),
            "{missing_member}"
        );
        assert!(
            missing_member.contains("dev · whole env · selected row not in dev"),
            "{missing_member}"
        );
        assert!(
            !missing_member.contains("> ~ terraform_data.server[*]"),
            "{missing_member}"
        );
    }

    #[test]
    fn multi_demo_relations_show_environment_configuration_links() {
        let mut state = multi_demo_session();
        let wide = Size::new(165, 50);
        let mut relations = EnvironmentView::default();
        press_at(&mut relations, &mut state, KeyCode::Char('3'), wide);
        let dev = render_to_buffer((wide.width, wide.height), |frame| {
            relations.render(frame, &state);
        });
        let dev_text = buffer_text(&dev);
        assert!(dev_text.contains("terraform_data.server[*]"), "{dev_text}");
        assert!(dev_text.contains("terraform_data.dev_only"), "{dev_text}");
        assert!(dev_text.contains("└──> ~ terraform_data.api"), "{dev_text}");
        assert!(
            dev_text.contains("└┄┄> + terraform_data.dev_only !"),
            "{dev_text}"
        );
        assert!(!dev_text.contains("(config)"), "{dev_text}");
        insta::assert_snapshot!(
            "multi_demo_relations_dev_165x50",
            buffer_visual_snapshot(&dev)
        );

        let mut stg_view = EnvironmentView::default();
        press_at(&mut stg_view, &mut state, KeyCode::Char('1'), wide);
        press_at(&mut stg_view, &mut state, KeyCode::Down, wide);
        press_at(&mut stg_view, &mut state, KeyCode::Char('3'), wide);
        let stg = render_to_buffer((wide.width, wide.height), |frame| {
            stg_view.render(frame, &state);
        });
        let stg_text = buffer_text(&stg);
        assert!(stg_text.contains("stg · whole env"));
        assert!(stg_text.contains("└──> ~ terraform_data.api"), "{stg_text}");
        assert!(!stg_text.contains("┄┄>"), "{stg_text}");
        insta::assert_snapshot!(
            "multi_demo_relations_stg_165x50",
            buffer_visual_snapshot(&stg)
        );
    }

    #[test]
    fn relations_focus_scrolls_per_environment_and_ignores_matrix_only_keys() {
        let mut state = relation_session();
        let size = Size::new(120, 40);
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (size.width, size.height));

        press_at(&mut view, &mut state, KeyCode::Char('3'), size);
        assert_eq!(view.focus, EnvironmentPane::Relations);
        let selected = view.selected_relation_node(&state).cloned();
        assert!(selected.is_some());
        let selection = view
            .matrix
            .relation_selection()
            .map(|(row_id, child)| (row_id.clone(), child.map(str::to_owned)));

        let initial_vertical = view.relation_scrolls[0].vertical;
        press_at(&mut view, &mut state, KeyCode::Down, size);
        let down_scroll = view.relation_scrolls[0].vertical;
        assert_eq!(down_scroll, initial_vertical.saturating_add(1));
        press_at(&mut view, &mut state, KeyCode::Char('j'), size);
        assert_eq!(
            view.relation_scrolls[0].vertical,
            down_scroll.saturating_add(1)
        );
        press_at(&mut view, &mut state, KeyCode::Up, size);
        let up_scroll = view.relation_scrolls[0].vertical;
        assert_eq!(up_scroll, down_scroll);
        press_at(&mut view, &mut state, KeyCode::Char('k'), size);
        assert_eq!(view.relation_scrolls[0].vertical, initial_vertical);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        press_at(&mut view, &mut state, KeyCode::Right, size);
        let rendered = render_text(&mut view, &state, (size.width, size.height));
        assert!(rendered.contains("[3] Relations"));
        assert_eq!(
            view.relation_scrolls[0].vertical,
            initial_vertical.saturating_add(1)
        );
        assert!(view.relation_scrolls[0].horizontal > 0);
        assert_eq!(view.matrix.filter(), "");
        assert_eq!(
            view.matrix
                .relation_selection()
                .map(|(row_id, child)| (row_id.clone(), child.map(str::to_owned))),
            selection
        );
        assert_eq!(view.selected_relation_node(&state).cloned(), selected);

        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        let _ = render_text(&mut view, &state, (size.width, size.height));
        assert_eq!(view.selection.column, 1);
        assert_eq!(view.relation_scrolls[1].vertical, 0);
        assert_eq!(view.relation_scrolls[1].horizontal, 0);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        let _ = render_text(&mut view, &state, (size.width, size.height));
        assert!(view.relation_scrolls[1].vertical > 0);
        press_at(&mut view, &mut state, KeyCode::Char('['), size);
        assert_eq!(view.selection.column, 0);
        assert_eq!(
            view.relation_scrolls[0].vertical,
            initial_vertical.saturating_add(1)
        );
        assert_eq!(view.relation_scrolls[0].horizontal, 1);
    }

    #[test]
    fn excluded_selected_environment_keeps_its_graph_without_matrix_highlight() {
        let mut state = relation_session();
        let size = Size::new(120, 40);
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (size.width, size.height));

        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        press_at(&mut view, &mut state, KeyCode::Char('1'), size);
        press_at(&mut view, &mut state, KeyCode::Char(' '), size);
        let rendered = render_text(&mut view, &state, (size.width, size.height));

        assert!(rendered.contains("stg · not compared"), "{rendered}");
        assert!(rendered.contains("terraform_data.api"), "{rendered}");
        assert!(view.selected_relation_node(&state).is_none());

        press_at(&mut view, &mut state, KeyCode::Char('o'), size);
        let rendered = render_text(&mut view, &state, (size.width, size.height));
        assert!(rendered.contains("stg · whole env"), "{rendered}");
        assert!(view.selected_relation_node(&state).is_some());
    }

    #[test]
    fn relation_selection_highlights_only_matching_nodes_and_not_same_summary() {
        let mut state = relation_session();
        let size = Size::new(165, 50);
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (size.width, size.height));
        press_at(&mut view, &mut state, KeyCode::Char('3'), size);
        let buffer = render_to_buffer((size.width, size.height), |frame| {
            view.render(frame, &state);
        });
        assert!(view.selected_relation_node(&state).is_some());

        let relation_row = (0..size.height)
            .find(|row| {
                (0..size.width)
                    .map(|column| buffer.cell((column, *row)).unwrap().symbol())
                    .collect::<String>()
                    .contains("[3] Relations")
            })
            .expect("Relations pane title");
        let frame = buffer
            .cell((view.sidebar_width, relation_row))
            .expect("Relations pane top border");
        assert_eq!(frame.fg, Color::Cyan);
        assert_eq!(frame.bg, Color::Reset);
        assert_underlined_address(&buffer, "terraform_data.api");

        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::Char('a'), size);
        press_at(&mut view, &mut state, KeyCode::End, size);
        let _ = render_text(&mut view, &state, (size.width, size.height));
        assert_eq!(view.matrix.relation_selection(), None);
        assert!(view.selected_relation_node(&state).is_none());
    }

    fn assert_underlined_address(buffer: &ratatui::buffer::Buffer, address: &str) {
        let found = (0..buffer.area.height).any(|row| {
            let line = (0..buffer.area.width)
                .map(|column| buffer.cell((column, row)).unwrap().symbol())
                .collect::<String>();
            let Some(start) = line.find(address) else {
                return false;
            };
            let selected = line[..start].contains('>');
            let underlined = buffer
                .cell((u16::try_from(start).unwrap(), row))
                .is_some_and(|cell| cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
            selected && underlined
        });
        assert!(found, "{address} should be underlined on its selected node");
    }

    #[test]
    fn relation_status_recovers_after_environment_retry() {
        let mut state = session(&["ready", "error"]);
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Update)],
        );
        let error = state.start_next().unwrap();
        state.complete(
            error,
            PlanResult::Error("synthetic error".to_owned()),
            Vec::new(),
        );
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);

        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        assert!(render_text(&mut view, &state, (size.width, size.height)).contains("Plan failed"));
        press_at(&mut view, &mut state, KeyCode::Char('r'), size);
        assert!(render_text(&mut view, &state, (size.width, size.height)).contains("Plan pending"));
        complete_with_relations(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Create)],
            PlanRelations::not_collected(),
        );
        let rendered = render_text(&mut view, &state, (size.width, size.height));
        assert!(rendered.contains("Links unknown"), "{rendered}");
        assert!(!rendered.contains("No links shown"), "{rendered}");
        assert!(view.selected_relation_node(&state).is_some());
    }
}

mod layout {
    use super::*;

    #[test]
    fn compare_pane_ends_after_its_legend_and_leaves_the_rest_to_relations() {
        let state = multi_demo_session();
        let mut view = EnvironmentView::default();

        let text = render_text(&mut view, &state, (165, 50));

        let lines = text.lines().collect::<Vec<_>>();
        let legend = lines
            .iter()
            .position(|line| line.contains("blank: absent"))
            .expect("the matrix legend");
        assert!(
            lines[legend - 1].contains("instance counts differ"),
            "{text}"
        );
        assert!(lines[legend + 1].contains('└'), "{text}");
        assert!(lines[legend + 2].contains("[3] Relations"), "{text}");
    }

    #[rstest]
    #[case::terminal_150x48(Size::new(150, 48), false)]
    #[case::medium_100x30(Size::new(100, 30), true)]
    fn multi_demo_overview_visual_baseline(
        #[case] size: Size,
        #[case] snapshot_compare_steps: bool,
    ) {
        let mut state = multi_demo_session();
        let suffix = format!("{}x{}", size.width, size.height);
        let mut view = EnvironmentView::default();
        let snapshot = |view: &mut EnvironmentView, state: &EnvironmentSession| {
            buffer_visual_snapshot(&render_to_buffer((size.width, size.height), |frame| {
                view.render(frame, state);
            }))
        };

        insta::assert_snapshot!(format!("vrt_initial_{suffix}"), snapshot(&mut view, &state));

        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::End, size);
        press_at(&mut view, &mut state, KeyCode::Char(' '), size);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        press_at(&mut view, &mut state, KeyCode::Char(' '), size);
        if snapshot_compare_steps {
            insta::assert_snapshot!(
                format!("vrt_compare_group_open_{suffix}"),
                snapshot(&mut view, &state)
            );
        }

        press_at(&mut view, &mut state, KeyCode::End, size);
        if snapshot_compare_steps {
            insta::assert_snapshot!(
                format!("vrt_compare_member_missing_in_dev_{suffix}"),
                snapshot(&mut view, &state)
            );
        }

        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        press_at(&mut view, &mut state, KeyCode::Char(']'), size);
        press_at(&mut view, &mut state, KeyCode::Char('3'), size);
        insta::assert_snapshot!(
            format!("vrt_relations_prod_{suffix}"),
            snapshot(&mut view, &state)
        );
    }

    #[rstest]
    #[case::small(80, 24)]
    #[case::medium(120, 40)]
    #[case::large(165, 50)]
    #[case::narrow(40, 16)]
    fn three_environments_show_matrix_actions_across_supported_widths(
        #[case] width: u16,
        #[case] height: u16,
    ) {
        let mut state = session(&["dev", "prod", "stg"]);
        for (count, action) in [
            (10, ResourceChangeKind::Update),
            (100, ResourceChangeKind::Replace),
            (10, ResourceChangeKind::Delete),
        ] {
            let mut changes: Vec<_> = (0..count)
                .map(|index| {
                    change(
                        &format!("terraform_data.server[{index}]"),
                        ResourceChangeKind::Update,
                    )
                })
                .collect();
            changes.push(change("terraform_data.api", action));
            complete(&mut state, changes);
        }
        let mut view = EnvironmentView::default();
        if (width, height) == (40, 16) {
            handle_key_code(
                &mut view,
                KeyCode::Char('2'),
                Size::new(width, height),
                &state,
            );
            handle_key_code(
                &mut view,
                KeyCode::Char('f'),
                Size::new(width, height),
                &state,
            );
        }
        let buffer = render_to_buffer((width, height), |frame| view.render(frame, &state));

        let reversed_cells = buffer
            .content
            .iter()
            .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
            .count();
        assert_eq!(
            reversed_cells, 0,
            "the matrix selection never reverses the background"
        );
        let rendered = buffer_text(&buffer);
        assert!(rendered.contains("terraleph ▸ dev"));
        assert!(!rendered.contains("0 Overview"));
        if (width, height) == (40, 16) {
            handle_key_code(&mut view, KeyCode::End, Size::new(width, height), &state);
            let bottom = render_text(&mut view, &state, (width, height));
            assert!(bottom.contains("blank: absent"), "{bottom}");
            assert!(bottom.contains("?: plan unavailable"), "{bottom}");
            assert!(!bottom.contains("Total"), "{bottom}");
        } else {
            assert!(rendered.contains("blank: absent"));
            assert!(rendered.contains("?: plan unavailable"));
        }
        assert!(rendered.contains("> terraform_data.api"));
        assert!(rendered.contains("[2] Compare"));
        assert!(!rendered.contains("Total"));
        let matrix_header = rendered
            .lines()
            .position(|line| line.contains("Address"))
            .unwrap();
        let (selected_row_y, selected_row) = rendered
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("> terraform_data.api"))
            .expect("the first differing row is selected");
        let address_x = u16::try_from(
            ratatui::text::Line::from(
                &selected_row[..selected_row.find("terraform_data.api").unwrap()],
            )
            .width(),
        )
        .unwrap();
        let address_cell = buffer
            .cell((address_x, u16::try_from(selected_row_y).unwrap()))
            .unwrap();
        assert!(address_cell.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(address_cell.bg, Color::Reset);
        let header_line = rendered.lines().nth(matrix_header).unwrap();
        let dev_x = u16::try_from(
            ratatui::text::Line::from(&header_line[..header_line.find("dev").unwrap()]).width(),
        )
        .unwrap();
        let update_x = u16::try_from(
            ratatui::text::Line::from(&selected_row[..selected_row.find('~').unwrap()]).width(),
        )
        .unwrap();
        assert_eq!(
            dev_x, update_x,
            "selected rows preserve the environment columns"
        );
        assert!(
            !rendered
                .lines()
                .nth(matrix_header)
                .unwrap()
                .contains("> dev")
        );
        assert_eq!(
            buffer
                .cell((2, u16::try_from(matrix_header).unwrap()))
                .unwrap()
                .bg,
            Color::Reset
        );
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Reset);
        assert_eq!(
            buffer.cell((width - 1, height - 1)).unwrap().bg,
            Color::Reset
        );
        if width >= 120 {
            assert!(rendered.contains("* [1] Envs"));
            assert!(rendered.contains("Space toggle"));
        } else {
            assert!(!rendered.contains("Space expand selected"));
            assert!(!rendered.contains("[1] Envs"));
        }
        // 165x50 only adds blank space to the 120x40 layout, so it takes no snapshot.
        if (width, height) != (165, 50) {
            insta::assert_snapshot!(format!("three_environments_{width}x{height}"), rendered);
        }
    }

    #[test]
    fn short_terminal_keeps_major_environment_actions_without_movement_hints() {
        let state = session(&["dev", "prod", "stg"]);
        let mut view = EnvironmentView::default();
        let rendered = render_text(&mut view, &state, (40, 14));

        assert!(!rendered.contains("Enter open row"));
        for hint in ["[/] env", "? help", "q quit"] {
            assert!(rendered.contains(hint), "{hint}");
        }
        assert!(!rendered.contains("/ filter"));
        assert!(!rendered.contains("↑↓"));
        assert!(!rendered.contains("←→"));
        let lines = rendered.lines().collect::<Vec<_>>();
        assert!(
            !lines
                .iter()
                .any(|line| line.trim_start().starts_with("Total"))
        );
        assert!(!rendered.contains(&"─".repeat(40)));
    }

    #[test]
    fn narrow_matrix_keeps_why_visible_with_a_long_selected_environment_name() {
        let state = session(&["production-eu-west-1"]);
        let mut view = EnvironmentView::default();
        handle_key_code(&mut view, KeyCode::Char('f'), Size::new(40, 16), &state);
        let rendered = render_text(&mut view, &state, (40, 16));
        let header = rendered
            .lines()
            .find(|line| line.contains("Address"))
            .expect("matrix header");

        assert!(!header.contains("> "));
        assert!(header.contains("why"));
    }

    #[test]
    fn matrix_title_separates_comparison_filtering_from_ready_plans() {
        let mut state = session(&["dev", "prod", "stg"]);
        for _ in 0..2 {
            complete(
                &mut state,
                vec![change("terraform_data.api", ResourceChangeKind::Update)],
            );
        }
        let mut view = EnvironmentView::default();
        let all = render_text(&mut view, &state, (120, 40));
        assert!(all.contains("Ready 2/3"));
        assert!(!all.contains("Filtered"));

        press_at(
            &mut view,
            &mut state,
            KeyCode::Char(' '),
            Size::new(120, 40),
        );
        let filtered = render_text(&mut view, &state, (120, 40));
        assert!(filtered.contains("Filtered 2/3 envs"));
        assert!(filtered.contains("Ready 1/2"));

        press_at(
            &mut view,
            &mut state,
            KeyCode::Char('o'),
            Size::new(120, 40),
        );
        let single = render_text(&mut view, &state, (120, 40));
        assert!(single.contains("[2] Changes · dev"));
        assert!(single.contains("Filtered 1/3 envs"));
        let title = single
            .lines()
            .find(|line| line.contains("[2] Changes · dev"))
            .expect("matrix title");
        assert!(!title.contains("Ready"));
    }

    #[rstest]
    #[case::small(80, 24)]
    #[case::narrow(40, 16)]
    fn shared_workspace_names_keep_retry_directory_and_tool_visible(
        #[case] width: u16,
        #[case] height: u16,
    ) {
        let mut state = EnvironmentSession::new(
            ["dev", "prod"]
                .map(|name| Environment {
                    tool: Tool::OpenTofu,
                    availability: EnvironmentAvailability::Available(EnvironmentIdentity {
                        directory: PathBuf::from(format!("/synthetic/{name}")),
                        workspace: "staging".to_owned(),
                    }),
                })
                .into_iter()
                .collect(),
            false,
        );
        for _ in 0..2 {
            let index = state.start_next().unwrap();
            state.complete(
                index,
                PlanResult::Error("Synthetic acquisition error".to_owned()),
                Vec::new(),
            );
        }
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char(']'));
        let output = render_text(&mut view, &state, (width, height));

        assert!(!output.contains("/synthetic/prod"));
        assert!(output.contains("terraleph ▸ prod"));
        assert!(!output.contains("staging · tofu"));
        insta::assert_snapshot!(format!("shared_workspace_{width}x{height}"), output);
        press(&mut view, &mut state, KeyCode::Char('c'));
        let context = render_text(&mut view, &state, (width, height));
        assert!(context.contains("/synthetic/prod"));
        assert!(context.contains("tofu"));
        press(&mut view, &mut state, KeyCode::Esc);
        let input = handle_key_code(
            &mut view,
            KeyCode::Char('r'),
            Size::new(width, height),
            &state,
        );
        assert!(matches!(input, Some(EnvironmentInput::Retry(1))));
    }
}

mod row_groups {
    use super::*;

    #[test]
    fn multi_demo_compare_expansion_keeps_columns_anchored() {
        let mut state = multi_demo_session();
        let wide = Size::new(165, 50);
        let mut compare = EnvironmentView::default();
        press_at(&mut compare, &mut state, KeyCode::Char('2'), wide);
        press_at(&mut compare, &mut state, KeyCode::Char('f'), wide);
        press_at(&mut compare, &mut state, KeyCode::End, wide);

        let collapsed = render_to_buffer((wide.width, wide.height), |frame| {
            compare.render(frame, &state);
        });
        let collapsed_text = buffer_text(&collapsed);
        assert!(
            collapsed_text.contains("Same change across envs"),
            "{collapsed_text}"
        );
        insta::assert_snapshot!(
            "multi_demo_compare_same_closed_165x50",
            buffer_visual_snapshot(&collapsed)
        );

        press_at(&mut compare, &mut state, KeyCode::Char(' '), wide);
        let expanded_summary = render_to_buffer((wide.width, wide.height), |frame| {
            compare.render(frame, &state);
        });
        let expanded_summary_text = buffer_text(&expanded_summary);
        assert!(
            expanded_summary_text.contains("terraform_data.server[*]"),
            "{expanded_summary_text}"
        );
        assert!(expanded_summary_text.contains("[unknown values]"));
        assert!(
            expanded_summary_text.contains("~ 2         ~ 2         ~ 4"),
            "{expanded_summary_text}"
        );
        assert_eq!(
            matrix_header(&collapsed_text),
            matrix_header(&expanded_summary_text),
            "opening Same change must keep environment columns anchored"
        );
        insta::assert_snapshot!(
            "multi_demo_compare_same_open_165x50",
            buffer_visual_snapshot(&expanded_summary)
        );

        press_at(&mut compare, &mut state, KeyCode::Down, wide);
        press_at(&mut compare, &mut state, KeyCode::Down, wide);
        press_at(&mut compare, &mut state, KeyCode::Char(' '), wide);
        let expanded_group = render_to_buffer((wide.width, wide.height), |frame| {
            compare.render(frame, &state);
        });
        let expanded_group_text = buffer_text(&expanded_group);
        assert!(
            expanded_group_text.contains("terraform_data.server[0]"),
            "{expanded_group_text}"
        );
        assert_eq!(
            matrix_header(&expanded_summary_text),
            matrix_header(&expanded_group_text),
            "expanding a resource group must keep environment columns anchored"
        );
        assert_eq!(
            expanded_group_text.matches("Differs across envs").count(),
            1,
            "members missing from some environments stay under their group\n{expanded_group_text}"
        );
        let group_line = expanded_group_text
            .lines()
            .position(|line| line.contains("▾ terraform_data.server[*]"))
            .expect("the expanded group is visible");
        let member_rows = expanded_group_text.lines().skip(group_line + 1).take(4);
        assert!(
            member_rows
                .clone()
                .all(|line| line.contains("        terraform_data.server[")),
            "{expanded_group_text}"
        );
        assert!(
            member_rows
                .filter(|line| line.contains("only in prod"))
                .count()
                == 2,
            "{expanded_group_text}"
        );
        insta::assert_snapshot!(
            "multi_demo_compare_group_open_165x50",
            buffer_visual_snapshot(&expanded_group)
        );
    }

    #[test]
    fn multi_demo_compare_shows_unknown_summary_at_narrow_width() {
        let mut state = multi_demo_session();
        let mut narrow_view = EnvironmentView::default();
        let narrow = Size::new(40, 16);
        press_at(&mut narrow_view, &mut state, KeyCode::Char('2'), narrow);
        press_at(&mut narrow_view, &mut state, KeyCode::Char('f'), narrow);
        press_at(&mut narrow_view, &mut state, KeyCode::End, narrow);
        press_at(&mut narrow_view, &mut state, KeyCode::Char(' '), narrow);
        let narrow_buffer = render_to_buffer((narrow.width, narrow.height), |frame| {
            narrow_view.render(frame, &state);
        });
        assert!(buffer_text(&narrow_buffer).contains("[unknown values]"));
        insta::assert_snapshot!(
            "multi_demo_compare_narrow_40x16",
            buffer_visual_snapshot(&narrow_buffer)
        );
    }

    #[test]
    fn same_change_summary_and_group_rows_expand_independently() {
        let mut state = session(&["dev", "prod", "stg"]);
        for _ in 0..3 {
            complete(
                &mut state,
                (0..2)
                    .map(|index| {
                        change(
                            &format!("terraform_data.server[{index}]"),
                            ResourceChangeKind::Update,
                        )
                    })
                    .collect(),
            );
        }
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('2'));

        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Char('f'));
        press(&mut view, &mut state, KeyCode::End);
        let collapsed = render_text(&mut view, &state, (80, 24));
        assert!(collapsed.contains("Same change across envs: 1 pattern"));
        assert!(!collapsed.contains("terraform_data.server[*]"));
        assert!(collapsed.contains("Space expand selected"));
        press(&mut view, &mut state, KeyCode::Char('f'));
        assert_matrix_footer_actions(&mut view, &state);

        press(&mut view, &mut state, KeyCode::Char('f'));
        press(&mut view, &mut state, KeyCode::Char(' '));
        let expanded_same = render_text(&mut view, &state, (80, 24));
        assert!(expanded_same.contains("Same change across envs: 1 pattern"));
        assert!(expanded_same.contains("▸ terraform_data.server[*]"));
        assert!(expanded_same.contains("Space collapse selected"));

        press(&mut view, &mut state, KeyCode::Down);
        press(&mut view, &mut state, KeyCode::Char(' '));
        let expanded_group = render_text(&mut view, &state, (80, 24));
        assert!(expanded_group.contains("▾ terraform_data.server[*]"));
        assert!(expanded_group.contains("terraform_data.server[0]"));
        assert!(expanded_group.contains("Space collapse selected"));
        assert!(expanded_group.contains("q quit"));

        press(&mut view, &mut state, KeyCode::Home);
        press(&mut view, &mut state, KeyCode::Enter);
        let enter_on_summary = render_text(&mut view, &state, (80, 24));
        assert!(
            enter_on_summary.contains("terraform_data.server[*]"),
            "{enter_on_summary}"
        );
        press(&mut view, &mut state, KeyCode::Char(' '));
        let collapsed_same = render_text(&mut view, &state, (80, 24));
        assert!(!collapsed_same.contains("terraform_data.server[*]"));

        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "server[1]".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        let filtered = render_text(&mut view, &state, (80, 24));
        assert!(filtered.contains("terraform_data.server[1]"));
        assert!(!filtered.contains("terraform_data.server[*]"));
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Group(_), Some(address))) if address == "terraform_data.server[1]"
        ));
        press(&mut view, &mut state, KeyCode::Char(' '));
        assert_eq!(render_text(&mut view, &state, (80, 24)), filtered);
    }

    #[test]
    fn same_change_row_shows_space_only_while_the_matrix_can_expand_it() {
        let mut state = session(&["dev", "prod"]);
        for _ in 0..2 {
            complete(
                &mut state,
                vec![change("terraform_data.server", ResourceChangeKind::Update)],
            );
        }
        let mut view = EnvironmentView::default();
        let wide = Size::new(165, 50);
        let _ = render_text(&mut view, &state, (wide.width, wide.height));
        press_at(&mut view, &mut state, KeyCode::Char('2'), wide);
        let _ = render_text(&mut view, &state, (165, 50));
        press(&mut view, &mut state, KeyCode::End);

        let matrix = render_text(&mut view, &state, (165, 50));
        assert!(matrix.contains("> ▸ Same change across envs"), "{matrix}");
        assert!(matrix.contains("Space expand"), "{matrix}");

        press_at(&mut view, &mut state, KeyCode::Char('f'), wide);
        let maximized = render_text(&mut view, &state, (165, 50));
        assert!(maximized.contains("Space expand"), "{maximized}");
        press_at(&mut view, &mut state, KeyCode::Char('f'), wide);

        press_at(&mut view, &mut state, KeyCode::Char('1'), wide);
        let environments = render_text(&mut view, &state, (165, 50));
        assert!(
            environments.contains("> ▸ Same change across envs"),
            "{environments}"
        );
        assert!(!environments.contains("Space expand"), "{environments}");
        assert!(environments.contains("Space toggle"), "{environments}");

        press_at(&mut view, &mut state, KeyCode::Char('?'), wide);
        let environment_help = render_text(&mut view, &state, (165, 50));
        assert!(
            !environment_help.contains("Space toggle"),
            "{environment_help}"
        );
        press_at(&mut view, &mut state, KeyCode::Esc, wide);

        press_at(&mut view, &mut state, KeyCode::Char('c'), wide);
        let environment_context = render_text(&mut view, &state, (165, 50));
        assert!(
            !environment_context.contains("Space toggle"),
            "{environment_context}"
        );
        press_at(&mut view, &mut state, KeyCode::Esc, wide);

        press_at(&mut view, &mut state, KeyCode::Char('2'), wide);
        press_at(&mut view, &mut state, KeyCode::Char('b'), wide);
        let sidebar_closed = render_text(&mut view, &state, (165, 50));
        assert!(sidebar_closed.contains("Space expand"), "{sidebar_closed}");

        press_at(&mut view, &mut state, KeyCode::Char('c'), wide);
        let context = render_text(&mut view, &state, (165, 50));
        assert!(!context.contains("Space expand"), "{context}");
        press_at(&mut view, &mut state, KeyCode::Esc, wide);

        press_at(&mut view, &mut state, KeyCode::Char('3'), wide);
        let relations = render_text(&mut view, &state, (165, 50));
        assert!(
            relations.contains("> ▸ Same change across envs"),
            "{relations}"
        );
        assert!(!relations.contains("Space expand"), "{relations}");

        press_at(&mut view, &mut state, KeyCode::Char('2'), wide);
        press_at(&mut view, &mut state, KeyCode::Char('/'), wide);
        let searching = render_text(&mut view, &state, (165, 50));
        assert!(!searching.contains("Space expand"), "{searching}");

        press_at(&mut view, &mut state, KeyCode::Esc, wide);
        press_at(&mut view, &mut state, KeyCode::Char('?'), wide);
        let help = render_text(&mut view, &state, (165, 50));
        assert!(!help.contains("Space expand"), "{help}");

        press_at(&mut view, &mut state, KeyCode::Esc, wide);
        press_at(&mut view, &mut state, KeyCode::Char('q'), wide);
        let quit_confirmation = render_text(&mut view, &state, (165, 50));
        assert!(
            !quit_confirmation.contains("Space expand"),
            "{quit_confirmation}"
        );
    }

    #[test]
    fn unknown_same_change_summary_and_group_row_keep_the_annotation_visible() {
        let provider = "registry.example/provider".to_owned();
        let schemas = ProviderSchemas {
            providers: BTreeMap::from([(
                provider.clone(),
                ProviderSchema {
                    resources: BTreeMap::from([(
                        "terraform_data".to_owned(),
                        ResourceSchema {
                            attributes: BTreeMap::from([
                                ("input".to_owned(), AttributeType::String),
                                ("output".to_owned(), AttributeType::Dynamic),
                            ]),
                            block_types: BTreeMap::new(),
                        },
                    )]),
                },
            )]),
        };
        let mut state = session(&["dev", "stg", "prod"]);
        for count in [2, 2, 4] {
            let mut changes: Vec<_> = (0..count)
                .map(|index| {
                    let mut change = change(
                        &format!("terraform_data.server[{index}]"),
                        ResourceChangeKind::Update,
                    );
                    change.provider = Some(provider.clone());
                    change.before = Some(PlanValue::Object(BTreeMap::from([
                        ("input".to_owned(), PlanValue::String("old".to_owned())),
                        ("output".to_owned(), PlanValue::String("old".to_owned())),
                    ])));
                    change.after = Some(PlanValue::Object(BTreeMap::from([
                        ("input".to_owned(), PlanValue::String("new".to_owned())),
                        ("output".to_owned(), PlanValue::Null),
                    ])));
                    change.after_unknown = Some(PlanValue::Object(BTreeMap::from([(
                        "output".to_owned(),
                        PlanValue::Bool(true),
                    )])));
                    change
                })
                .collect();
            changes.extend((0..10).map(|index| {
                change(
                    &format!("terraform_data.zz_extra_{index:02}"),
                    ResourceChangeKind::Update,
                )
            }));
            complete_with_schemas(
                &mut state,
                changes,
                PlanRelations::not_collected(),
                Some(schemas.clone()),
            );
        }

        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('2'));
        press(&mut view, &mut state, KeyCode::Char('f'));
        press(&mut view, &mut state, KeyCode::End);
        let collapsed = render_text(&mut view, &state, (165, 50));
        assert!(
            collapsed.contains("Same change across envs: 11 patterns"),
            "{collapsed}"
        );
        assert!(
            collapsed.contains("    [unknown values] · instance counts differ"),
            "{collapsed}"
        );

        press(&mut view, &mut state, KeyCode::Char(' '));
        let expanded = render_text(&mut view, &state, (165, 50));
        assert_eq!(
            expanded.matches("[unknown values]").count(),
            2,
            "{expanded}"
        );

        press(&mut view, &mut state, KeyCode::End);
        let bottom = render_text(&mut view, &state, (40, 16));
        assert!(bottom.contains("zz_extra_09"), "{bottom}");
        for _ in 0..10 {
            press(&mut view, &mut state, KeyCode::Up);
        }
        let narrow = render_text(&mut view, &state, (40, 16));
        let narrow_lines = narrow.lines().collect::<Vec<_>>();
        let selected_group = narrow_lines.iter().position(|line| {
            line.starts_with("│>") && line.contains("▸ ") && line.contains("server[*]")
        });
        assert!(selected_group.is_some(), "{narrow}");
        assert!(
            narrow_lines
                .get(selected_group.unwrap() + 1)
                .is_some_and(|line| line.contains("[unknown values]")),
            "{narrow}"
        );
    }

    fn assert_matrix_footer_actions(view: &mut EnvironmentView, state: &EnvironmentSession) {
        for (width, height) in [(40, 16), (80, 24), (120, 40), (165, 50)] {
            let rendered = render_text(view, state, (width, height));
            let hints = [
                "[/] env",
                if width == 40 {
                    "Space expand"
                } else {
                    "Space expand selected"
                },
                "? help",
            ];
            assert!(!rendered.contains("Enter"), "{width}x{height}\n{rendered}");
            for hint in hints {
                assert!(
                    rendered.contains(hint),
                    "{width}x{height}: {hint}\n{rendered}"
                );
            }
            assert!(rendered.contains("q quit"), "{width}x{height}");
            if width != 40 {
                assert!(
                    rendered.contains("/ filter"),
                    "{width}x{height}: {rendered}"
                );
            }
            assert!(rendered.contains("v full plan"), "{width}x{height}");
            assert!(!rendered.contains("↑↓"), "{width}x{height}");
            assert!(!rendered.contains("←→"), "{width}x{height}");
        }
    }

    #[test]
    fn space_toggles_only_the_selected_group_in_a_single_environment_matrix() {
        let mut state = session(&["dev"]);
        let changes: Vec<_> = (0..2)
            .flat_map(|group| {
                (0..2).map(move |instance| {
                    change(
                        &format!("terraform_data.server_{group}[{instance}]"),
                        ResourceChangeKind::Update,
                    )
                })
            })
            .collect();
        complete(&mut state, changes);
        let mut view = EnvironmentView::default();

        let collapsed = render_text(&mut view, &state, (120, 40));
        assert!(
            collapsed.contains("▸ terraform_data.server_0[*]"),
            "{collapsed}"
        );
        assert!(
            collapsed.contains("▸ terraform_data.server_1[*]"),
            "{collapsed}"
        );
        press(&mut view, &mut state, KeyCode::Char(' '));
        let first_expanded = render_text(&mut view, &state, (120, 40));
        assert!(first_expanded.contains("▾ terraform_data.server_0[*]"));
        assert!(first_expanded.contains("▸ terraform_data.server_1[*]"));
        assert!(first_expanded.contains("server_0[0]"));
        assert!(!first_expanded.contains("server_1[0]"));

        for _ in 0..3 {
            press(&mut view, &mut state, KeyCode::Down);
        }
        press(&mut view, &mut state, KeyCode::Char(' '));
        let both_expanded = render_text(&mut view, &state, (120, 40));
        assert!(both_expanded.contains("▾ terraform_data.server_0[*]"));
        assert!(both_expanded.contains("▾ terraform_data.server_1[*]"));
        assert!(both_expanded.contains("server_1[1]"));

        press(&mut view, &mut state, KeyCode::Home);
        press(&mut view, &mut state, KeyCode::Char(' '));
        let collapsed = render_text(&mut view, &state, (120, 40));
        assert!(
            collapsed.contains("▸ terraform_data.server_0[*]"),
            "{collapsed}"
        );
        assert!(collapsed.contains("▾ terraform_data.server_1[*]"));
    }

    #[test]
    fn same_change_summary_counts_matrix_rows_and_replacements_once() {
        let mut state = session(&["dev", "prod"]);
        let changes = || {
            let mut changes: Vec<_> = (0..3)
                .map(|index| {
                    change(
                        &format!("terraform_data.server[{index}]"),
                        ResourceChangeKind::Update,
                    )
                })
                .collect();
            changes.push(change("terraform_data.api", ResourceChangeKind::Replace));
            changes
        };
        complete(&mut state, changes());
        complete(&mut state, changes());
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('2'));
        let rendered = render_text(&mut view, &state, (80, 24));

        assert!(rendered.contains("Same change across envs: 2 patterns: 1 replace"));
        assert!(!rendered.contains("patterns: ~3"));
        assert!(!rendered.contains("3 replace"));

        press(&mut view, &mut state, KeyCode::Char('f'));
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        press(&mut view, &mut state, KeyCode::Down);
        press(&mut view, &mut state, KeyCode::Char(' '));
        let expanded = render_text(&mut view, &state, (80, 24));
        assert!(
            expanded.contains("Same change across envs: 2 patterns: 1 replace"),
            "{expanded}"
        );
        assert!(expanded.contains("terraform_data.server[0]"), "{expanded}");
    }

    #[test]
    fn retry_and_new_ready_environment_keep_member_when_group_disappears() {
        let mut state = session(&["a", "b", "c"]);
        for _ in 0..2 {
            complete(
                &mut state,
                (0..2)
                    .map(|index| {
                        change(
                            &format!("terraform_data.server[{index}]"),
                            ResourceChangeKind::Update,
                        )
                    })
                    .collect(),
            );
        }
        let index = state.start_next().unwrap();
        state.complete(
            index,
            PlanResult::Error("synthetic error".to_owned()),
            Vec::new(),
        );
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('1'));
        press(&mut view, &mut state, KeyCode::Char(']'));
        press(&mut view, &mut state, KeyCode::Char(']'));
        press(&mut view, &mut state, KeyCode::Char('r'));
        assert_eq!(view.selection.column, 2);
        complete(
            &mut state,
            vec![
                change("terraform_data.server[0]", ResourceChangeKind::Replace),
                change("terraform_data.server[1]", ResourceChangeKind::NoOp),
            ],
        );
        let output = render_text(&mut view, &state, (80, 24));
        assert_eq!(view.selection.column, 2);
        assert!(output.contains("terraform_data.server[0]"), "{output}");
        assert!(output.contains("terraleph ▸ c"), "{output}");
        assert!(output.contains("Ready"));
        assert!(!output.contains("Compared:"));
    }
}

mod navigation {
    use super::*;

    #[test]
    fn vim_navigation_uses_the_focused_multi_environment_pane() {
        let mut state = session(&["development", "staging", "production"]);
        complete(
            &mut state,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.beta", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.gamma", ResourceChangeKind::Update)],
        );

        let size = Size::new(120, 40);
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (size.width, size.height));

        assert_eq!(view.focus, EnvironmentPane::Environments);
        let environment_pane = render_text(&mut view, &state, (size.width, size.height));
        press_at(&mut view, &mut state, KeyCode::Char('h'), size);
        press_at(&mut view, &mut state, KeyCode::Char('l'), size);
        assert_eq!(view.selection.column, 0);
        assert_eq!(
            render_text(&mut view, &state, (size.width, size.height)),
            environment_pane
        );
        press_at(&mut view, &mut state, KeyCode::Char('G'), size);
        assert_eq!(view.selection.column, 2);
        press_at(&mut view, &mut state, KeyCode::Char('g'), size);
        assert_eq!(view.selection.column, 0);

        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::Char('G'), size);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.gamma"
        ));
        press_at(&mut view, &mut state, KeyCode::Char('g'), size);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.alpha"
        ));
        let matrix_start = render_text(&mut view, &state, (size.width, size.height));
        press_at(&mut view, &mut state, KeyCode::Char('l'), size);
        let matrix_right = render_text(&mut view, &state, (size.width, size.height));
        assert_ne!(matrix_right, matrix_start);
        assert_eq!(view.selection.column, 0);
        press_at(&mut view, &mut state, KeyCode::Char('h'), size);
        assert_eq!(
            render_text(&mut view, &state, (size.width, size.height)),
            matrix_start
        );

        press_at(&mut view, &mut state, KeyCode::Char('3'), size);
        press_at(&mut view, &mut state, KeyCode::Char('l'), size);
        assert_eq!(view.relation_scrolls[0].horizontal, 1);
        press_at(&mut view, &mut state, KeyCode::Char('h'), size);
        assert_eq!(view.relation_scrolls[0].horizontal, 0);
        press_at(&mut view, &mut state, KeyCode::Char('G'), size);
        assert_eq!(view.relation_scrolls[0].vertical, u16::MAX);
        press_at(&mut view, &mut state, KeyCode::Char('g'), size);
        assert_eq!(view.relation_scrolls[0].vertical, 0);
        assert_eq!(view.selection.column, 0);
    }

    #[test]
    fn selected_environment_stays_viewable_after_leaving_the_comparison() {
        let mut state = session(&["dev", "prod"]);
        complete(
            &mut state,
            vec![change("terraform_data.alpha", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.zeta", ResourceChangeKind::Update)],
        );
        let mut view = EnvironmentView::default();
        press_at(&mut view, &mut state, KeyCode::Down, Size::new(120, 40));
        press_at(
            &mut view,
            &mut state,
            KeyCode::Char('1'),
            Size::new(120, 40),
        );
        press_at(
            &mut view,
            &mut state,
            KeyCode::Char(' '),
            Size::new(120, 40),
        );
        assert_eq!(view.selected_environments, Some(vec![0]));
        assert_eq!(view.selection.column, 1);

        let rendered = render_text(&mut view, &state, (120, 40));
        let header = rendered
            .lines()
            .find(|line| line.contains("Address"))
            .unwrap();
        assert!(header.contains("dev"), "{header}");
        assert!(!header.contains("prod"), "{header}");
        assert!(rendered.contains("> [ ] prod"), "{rendered}");

        press_at(&mut view, &mut state, KeyCode::Enter, Size::new(120, 40));
        assert_eq!(view.selection.raw, Some(1));
    }

    #[test]
    fn columns_remain_selectable_without_rows_and_scroll_beyond_nine() {
        let names: Vec<_> = (0..12).map(|index| format!("env-{index:02}")).collect();
        let mut state = session(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let mut view = EnvironmentView::default();
        for _ in 0..11 {
            press(&mut view, &mut state, KeyCode::Char(']'));
        }
        assert_eq!(view.selection.column, 11);
        insta::assert_snapshot!(
            "twelve_pending_selected_last",
            render_text(&mut view, &state, (80, 24))
        );
        for _ in 0..12 {
            complete(
                &mut state,
                vec![change("terraform_data.api", ResourceChangeKind::Update)],
            );
        }
        let rendered = render_text(&mut view, &state, (80, 24));
        insta::assert_snapshot!("twelve_ready_selected_last", rendered.as_str());
        let header = rendered
            .lines()
            .find(|line| line.contains("Address"))
            .expect("matrix header");
        assert!(header.contains("env-11"), "{header}");
        press(&mut view, &mut state, KeyCode::Char('v'));
        assert_eq!(view.selection.raw, Some(11));
        press(&mut view, &mut state, KeyCode::Char('['));
        assert_eq!(view.selection.raw, Some(10));
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.selection.column, 11);
    }

    #[test]
    fn horizontal_matrix_scrolling_does_not_change_the_selected_environment() {
        let names: Vec<_> = (0..12).map(|index| format!("env-{index:02}")).collect();
        let mut state = session(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let mut view = EnvironmentView::default();
        let first = render_text(&mut view, &state, (80, 24));
        let header = first
            .lines()
            .find(|line| line.contains("Address"))
            .expect("matrix header");
        assert!(header.contains("env-00"));

        press(&mut view, &mut state, KeyCode::Right);
        assert_eq!(view.selection.column, 0);
        let scrolled = render_text(&mut view, &state, (80, 24));
        let header = scrolled
            .lines()
            .find(|line| line.contains("Address"))
            .expect("matrix header");
        assert!(!header.contains("env-00"));
        assert!(header.contains("env-01"));

        press(&mut view, &mut state, KeyCode::Char(']'));
        assert_eq!(view.selection.column, 1);
        press(&mut view, &mut state, KeyCode::Left);
        assert_eq!(view.selection.column, 1);
        let returned = render_text(&mut view, &state, (80, 24));
        let header = returned
            .lines()
            .find(|line| line.contains("Address"))
            .expect("matrix header");
        assert!(header.contains("env-00"));
    }

    #[test]
    fn focus_keys_open_and_close_the_sidebar_without_changing_selection() {
        let mut state = session(&["dev", "prod", "stg"]);
        for _ in 0..3 {
            complete(
                &mut state,
                vec![change("terraform_data.api", ResourceChangeKind::Update)],
            );
        }
        let mut view = EnvironmentView::default();
        let size = Size::new(120, 40);

        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        assert_eq!(view.focus, EnvironmentPane::Matrix);
        press_at(&mut view, &mut state, KeyCode::Char('1'), size);
        assert_eq!(view.focus, EnvironmentPane::Environments);
        press_at(&mut view, &mut state, KeyCode::Down, size);
        assert_eq!(view.selection.column, 1);
        press_at(&mut view, &mut state, KeyCode::Char('b'), size);
        assert_eq!(view.sidebar, SidebarSetting::Closed);
        assert_eq!(view.focus, EnvironmentPane::Matrix);
        press_at(&mut view, &mut state, KeyCode::Char('b'), size);
        assert_eq!(view.sidebar, SidebarSetting::Open);
        assert_eq!(view.focus, EnvironmentPane::Matrix);
    }
}

mod search {
    use super::*;

    #[test]
    fn matrix_search_keeps_vim_navigation_aliases_as_query_text() {
        let mut state = session(&["development", "production"]);
        complete(
            &mut state,
            vec![change(
                "terraform_data.resource",
                ResourceChangeKind::Update,
            )],
        );
        complete(
            &mut state,
            vec![change(
                "terraform_data.resource",
                ResourceChangeKind::Update,
            )],
        );
        let size = Size::new(120, 40);
        let mut view = EnvironmentView::default();
        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::Char('/'), size);
        for character in ['h', 'l', 'g', 'G'] {
            press_at(&mut view, &mut state, KeyCode::Char(character), size);
        }
        assert_eq!(view.matrix.filter(), "hlgG");
        press_at(&mut view, &mut state, KeyCode::Enter, size);
        assert_eq!(view.matrix.filter(), "hlgG");
    }

    #[test]
    fn selection_tracks_visible_rows_after_search_and_clears_when_none_match() {
        let mut state = session(&["dev"]);
        complete(
            &mut state,
            vec![
                change("terraform_data.alpha", ResourceChangeKind::Update),
                change("terraform_data.beta", ResourceChangeKind::Update),
                change("terraform_data.gamma", ResourceChangeKind::Update),
            ],
        );
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Down);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.beta"
        ));

        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "gamma".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.gamma"
        ));

        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "missing".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        assert_eq!(view.matrix.relation_selection(), None);
        assert!(render_text(&mut view, &state, (80, 24)).contains("No matching resource changes"));

        press(&mut view, &mut state, KeyCode::Esc);
        assert!(matches!(
            view.matrix.relation_selection(),
            Some((OverviewRowId::Individual(address), None)) if address == "terraform_data.alpha"
        ));
    }

    #[test]
    fn filter_uses_complete_addresses_and_raw_return_preserves_expansion() {
        let mut state = session(&["a", "b", "c"]);
        for _ in 0..3 {
            complete(
                &mut state,
                (100..120)
                    .map(|index| {
                        change(
                            &format!("module.long_name.terraform_data.server[{index}]"),
                            ResourceChangeKind::Update,
                        )
                    })
                    .collect(),
            );
        }
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "server[118]".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        press(&mut view, &mut state, KeyCode::Char(' '));
        press(&mut view, &mut state, KeyCode::Down);
        press(&mut view, &mut state, KeyCode::Char(']'));
        let before = render_text(&mut view, &state, (80, 24));
        press(&mut view, &mut state, KeyCode::Enter);
        assert_eq!(view.selection.raw, Some(1));
        assert!(view.reviews[1].scroll().0 > 0);
        assert!(
            render_text(&mut view, &state, (80, 24))
                .contains("module.long_name.terraform_data.server[118]")
        );
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.selection.raw, None);
        assert_eq!(render_text(&mut view, &state, (80, 24)), before);

        press(&mut view, &mut state, KeyCode::Char('v'));
        assert_eq!(view.selection.raw, Some(1));
        assert_eq!(view.reviews[1].scroll().0, 0);
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.selection.column, 1);
        assert_eq!(render_text(&mut view, &state, (80, 24)), before);
        assert_eq!(view.reviews[1].scroll().0, 0);
    }

    #[test]
    fn matrix_search_edits_graphemes_and_restores_the_previous_filter_on_cancel() {
        let mut state = session(&["a"]);
        complete(
            &mut state,
            vec![
                change("terraform_data.alpha", ResourceChangeKind::Update),
                change("terraform_data.beta", ResourceChangeKind::Update),
            ],
        );
        let mut view = EnvironmentView::default();
        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "aあe\u{301}👩💻".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Home);
        press(&mut view, &mut state, KeyCode::Right);
        press(&mut view, &mut state, KeyCode::Right);
        press(&mut view, &mut state, KeyCode::Backspace);
        assert_eq!(view.matrix.filter(), "ae\u{301}👩💻");

        press(&mut view, &mut state, KeyCode::End);
        press(&mut view, &mut state, KeyCode::Left);
        press(&mut view, &mut state, KeyCode::Char('\u{200d}'));
        press(&mut view, &mut state, KeyCode::Char('x'));
        assert_eq!(view.matrix.filter(), "ae\u{301}👩\u{200d}💻x");

        press(&mut view, &mut state, KeyCode::Backspace);
        press(&mut view, &mut state, KeyCode::Backspace);
        press(&mut view, &mut state, KeyCode::Backspace);
        assert_eq!(view.matrix.filter(), "a");
        press(&mut view, &mut state, KeyCode::Home);
        press(&mut view, &mut state, KeyCode::Char('X'));
        press(&mut view, &mut state, KeyCode::End);
        press(&mut view, &mut state, KeyCode::Char('Y'));
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.matrix.filter(), "");
        assert!(render_text(&mut view, &state, (80, 24)).contains("terraform_data.beta"));

        press(&mut view, &mut state, KeyCode::Char('/'));
        for character in "terraform_data.beta".chars() {
            press(&mut view, &mut state, KeyCode::Char(character));
        }
        press(&mut view, &mut state, KeyCode::Enter);
        assert_eq!(view.matrix.filter(), "terraform_data.beta");
        assert!(render_text(&mut view, &state, (80, 24)).contains("terraform_data.beta"));

        press(&mut view, &mut state, KeyCode::Char('/'));
        press(&mut view, &mut state, KeyCode::Char('x'));
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.matrix.filter(), "terraform_data.beta");
        assert!(render_text(&mut view, &state, (80, 24)).contains("terraform_data.beta"));

        press(&mut view, &mut state, KeyCode::Char('/'));
        press(&mut view, &mut state, KeyCode::Char(']'));
        assert_eq!(view.selection.column, 0);
        assert_eq!(view.matrix.filter(), "terraform_data.beta]");
        press(&mut view, &mut state, KeyCode::Esc);
        assert_eq!(view.matrix.filter(), "terraform_data.beta");
    }

    #[rstest]
    #[case::matrix('2', EnvironmentPane::Matrix)]
    #[case::relations('3', EnvironmentPane::Relations)]
    fn escape_restores_maximized_filtered_pane_before_clearing_filter(
        #[case] pane_key: char,
        #[case] pane: EnvironmentPane,
    ) {
        let mut state = relation_session();
        let size = Size::new(120, 40);
        let mut view = EnvironmentView::default();
        let _ = render_text(&mut view, &state, (size.width, size.height));
        press_at(&mut view, &mut state, KeyCode::Char('2'), size);
        press_at(&mut view, &mut state, KeyCode::Char('/'), size);
        for character in "terraform_data.api".chars() {
            press_at(&mut view, &mut state, KeyCode::Char(character), size);
        }
        press_at(&mut view, &mut state, KeyCode::Enter, size);
        assert_eq!(view.matrix.filter(), "terraform_data.api");

        if pane_key != '2' {
            press_at(&mut view, &mut state, KeyCode::Char(pane_key), size);
        }
        assert_eq!(view.active_pane(size.width), pane);
        press_at(&mut view, &mut state, KeyCode::Char('f'), size);
        assert_eq!(view.maximized, Some(pane));

        press_at(&mut view, &mut state, KeyCode::Esc, size);
        assert_eq!(view.maximized, None);
        assert_eq!(view.matrix.filter(), "terraform_data.api");
        assert_eq!(view.active_pane(size.width), pane);

        press_at(&mut view, &mut state, KeyCode::Esc, size);
        assert!(view.matrix.filter().is_empty());
    }
}

mod colors {
    use super::*;

    #[test]
    fn why_reasons_use_the_default_terminal_foreground() {
        let mut state = session(&["dev", "prod"]);
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Update)],
        );
        complete(
            &mut state,
            vec![change("terraform_data.api", ResourceChangeKind::Create)],
        );
        let mut view = EnvironmentView::default();
        let buffer = render_to_buffer((120, 40), |frame| view.render(frame, &state));

        let (x, y) = text_position(&buffer, "action").expect("why reason is rendered");
        let reason = buffer.cell((x, y)).expect("reason cell exists");
        assert_eq!(reason.fg, Color::Reset);
        assert_eq!(reason.bg, Color::Reset);
    }

    #[test]
    fn sidebar_counts_and_matrix_actions_use_ansi_operation_colors() {
        let mut state = session(&["dev", "test", "stg", "prod"]);
        for kind in [
            ResourceChangeKind::Create,
            ResourceChangeKind::Update,
            ResourceChangeKind::Delete,
            ResourceChangeKind::Replace,
        ] {
            complete(&mut state, vec![change("terraform_data.api", kind)]);
        }
        let mut view = EnvironmentView::default();
        let buffer = render_to_buffer((160, 40), |frame| view.render(frame, &state));
        let rendered = buffer_text(&buffer);

        for (label, foreground) in [
            ("+1", Color::Green),
            ("~1", Color::Yellow),
            ("-1", Color::Red),
            ("1 replace", Color::Magenta),
        ] {
            let (y, line) = rendered
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(label))
                .unwrap_or_else(|| panic!("missing sidebar count {label}: {rendered}"));
            let byte_index = line.find(label).unwrap();
            let x = u16::try_from(ratatui::text::Line::from(&line[..byte_index]).width()).unwrap();
            let cell = buffer.cell((x, u16::try_from(y).unwrap())).unwrap();
            assert_eq!(cell.fg, foreground, "{label}");
            assert_eq!(cell.bg, Color::Reset, "{label}");
        }

        let (row_y, row) = rendered
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("terraform_data.api"))
            .expect("matrix API row");
        let replace_byte_index = row.find("+/-").expect("replace action");
        let replace_x =
            u16::try_from(ratatui::text::Line::from(&row[..replace_byte_index]).width()).unwrap();
        let replace_cell = buffer
            .cell((replace_x, u16::try_from(row_y).unwrap()))
            .unwrap();
        assert_eq!(replace_cell.fg, Color::Magenta);
        assert_eq!(replace_cell.bg, Color::Reset);
    }
}
