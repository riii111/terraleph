use std::{collections::HashMap, ops::Range};

use crate::app::review::{PlanBlock, PlanBlockKind, PlanDocument, PlanLineKind};

use super::PlanParseError;

pub(super) fn parse_document(
    bytes: Vec<u8>,
    resource_addresses: &[String],
    output_names: &[String],
) -> Result<PlanDocument, PlanParseError> {
    String::from_utf8(bytes)
        .map(|text| {
            let (blocks, line_kinds) =
                split_blocks_with_line_kinds(&text, resource_addresses, output_names);
            PlanDocument::with_blocks_and_line_kinds(text, blocks, line_kinds)
        })
        .map_err(|_| PlanParseError::InvalidUtf8)
}

fn split_blocks_with_line_kinds(
    text: &str,
    resource_addresses: &[String],
    output_names: &[String],
) -> (Vec<PlanBlock>, Vec<PlanLineKind>) {
    let lines = text.split('\n').collect::<Vec<_>>();
    let intro_end = leading_intro_end(&lines);
    let final_summary = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, line)| !line.trim().is_empty())
        .and_then(|(index, line)| is_terraform_summary(line).then_some(index));
    let mut line_kinds = vec![PlanLineKind::Body; lines.len()];
    for kind in line_kinds.iter_mut().take(intro_end) {
        *kind = PlanLineKind::Intro;
    }
    let mut resource_indices = HashMap::with_capacity(resource_addresses.len());
    for (index, address) in resource_addresses.iter().enumerate() {
        resource_indices.entry(address.as_str()).or_insert(index);
    }
    let mut output_indices = HashMap::with_capacity(output_names.len());
    for (index, name) in output_names.iter().enumerate() {
        output_indices.entry(name.as_str()).or_insert(index);
    }
    let mut candidates = Vec::new();
    let mut section_boundaries = Vec::new();
    let mut heredoc: Option<OpenHeredoc> = None;
    let mut in_output_section = false;
    for (line, text) in lines.iter().enumerate() {
        if let Some(open) = &heredoc {
            if heredoc_end(text, &open.terminator) {
                heredoc = None;
            } else {
                line_kinds[line] = PlanLineKind::HeredocBody {
                    marker_column: open.marker_column,
                };
            }
            continue;
        }
        if line < intro_end {
            continue;
        }
        if *text == "Changes to Outputs:" {
            line_kinds[line] = PlanLineKind::OutputSection;
            in_output_section = true;
            section_boundaries.push(line);
        } else if text.starts_with("Plan:") {
            if Some(line) == final_summary {
                line_kinds[line] = PlanLineKind::Summary;
            }
            in_output_section = false;
            section_boundaries.push(line);
        } else if is_note_line(text) {
            line_kinds[line] = PlanLineKind::Note;
        }
        if in_output_section {
            if output_header(text, &output_indices).is_some() {
                candidates.push((line, PlanBlockKind::Output, None));
            }
        } else if let Some(index) = resource_header(text, &resource_indices) {
            line_kinds[line] = PlanLineKind::ResourceHeader;
            candidates.push((
                line,
                PlanBlockKind::Resource,
                Some(resource_addresses[index].clone()),
            ));
        }
        heredoc = OpenHeredoc::opened_by(text);
    }

    let mut blocks = Vec::new();
    let mut cursor = 0;
    for (index, (start, kind, address)) in candidates.iter().enumerate() {
        if *start < cursor {
            continue;
        }
        if cursor < *start {
            push_block(&mut blocks, cursor..*start, PlanBlockKind::Common, None);
        }
        let end = block_end(
            lines.len(),
            *start,
            *kind,
            candidates.get(index + 1),
            &section_boundaries,
        );
        if *start < end {
            push_block(&mut blocks, *start..end, *kind, address.clone());
            cursor = end;
        }
    }
    if cursor < lines.len() {
        push_block(
            &mut blocks,
            cursor..lines.len(),
            PlanBlockKind::Common,
            None,
        );
    }
    if blocks.is_empty() {
        blocks.push(PlanBlock::new(0..lines.len(), PlanBlockKind::Common));
    }
    (blocks, line_kinds)
}

fn leading_intro_end(lines: &[&str]) -> usize {
    let mut index = 0;
    while lines.get(index).is_some_and(|line| line.trim().is_empty()) {
        index += 1;
    }
    let mut recognized = false;
    while let Some(line) = lines.get(index) {
        if is_intro_line(line) {
            recognized = true;
            index += 1;
        } else if recognized && line.trim().is_empty() {
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn is_intro_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("Terraform used the selected providers")
        || trimmed.starts_with("OpenTofu used the selected providers")
        || trimmed.starts_with("Resource actions are indicated with the following symbols:")
        || trimmed.starts_with("plan. Resource actions are indicated with the following symbols:")
        || trimmed.starts_with("+ create")
        || trimmed.starts_with("~ update in-place")
        || trimmed.starts_with("-/+ destroy and then create replacement")
        || trimmed.starts_with("- destroy")
        || trimmed.starts_with("<= read (data resources)")
        || trimmed == "Terraform will perform the following actions:"
        || trimmed == "OpenTofu will perform the following actions:"
}

fn is_terraform_summary(line: &str) -> bool {
    let Some(summary) = line
        .strip_prefix("Plan: ")
        .and_then(|summary| summary.strip_suffix('.'))
    else {
        return false;
    };
    let mut parts = summary.split(", ");
    let Some(additions) = parts.next().and_then(|part| part.strip_suffix(" to add")) else {
        return false;
    };
    let Some(changes) = parts
        .next()
        .and_then(|part| part.strip_suffix(" to change"))
    else {
        return false;
    };
    let Some(deletions) = parts
        .next()
        .and_then(|part| part.strip_suffix(" to destroy"))
    else {
        return false;
    };
    parts.next().is_none()
        && !additions.is_empty()
        && !changes.is_empty()
        && !deletions.is_empty()
        && additions
            .chars()
            .all(|character| character.is_ascii_digit())
        && changes.chars().all(|character| character.is_ascii_digit())
        && deletions
            .chars()
            .all(|character| character.is_ascii_digit())
}

fn is_note_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn push_block(
    blocks: &mut Vec<PlanBlock>,
    lines: Range<usize>,
    kind: PlanBlockKind,
    address: Option<String>,
) {
    if lines.is_empty() {
        return;
    }
    if matches!(kind, PlanBlockKind::Common) && blocks.last().is_some_and(PlanBlock::is_common) {
        if let Some(previous) = blocks.last_mut() {
            previous.lines_mut().end = lines.end;
        }
        return;
    }
    blocks.push(PlanBlock::with_addresses(
        lines,
        kind,
        address.into_iter().collect(),
    ));
}

fn block_end(
    line_count: usize,
    start: usize,
    kind: PlanBlockKind,
    next_candidate: Option<&(usize, PlanBlockKind, Option<String>)>,
    section_boundaries: &[usize],
) -> usize {
    let next_same_kind = next_candidate
        .filter(|(_, candidate_kind, _)| {
            matches!(
                (kind, candidate_kind),
                (PlanBlockKind::Resource, PlanBlockKind::Resource)
                    | (PlanBlockKind::Output, PlanBlockKind::Output)
            )
        })
        .map(|(line, _, _)| *line);
    let section_boundary = if matches!(kind, PlanBlockKind::Resource | PlanBlockKind::Output) {
        section_boundaries
            .get(section_boundaries.partition_point(|line| *line <= start))
            .copied()
    } else {
        None
    };
    next_same_kind
        .into_iter()
        .chain(section_boundary)
        .min()
        .unwrap_or(line_count)
}

fn resource_header(line: &str, indices: &HashMap<&str, usize>) -> Option<usize> {
    let rest = line.strip_prefix("  # ")?;
    let is_action = rest.contains(" will be ")
        || rest.contains(" must be ")
        || rest.contains(" has moved to ")
        || rest.contains(" will no longer be managed ");
    if !is_action {
        return None;
    }
    rest.char_indices()
        .filter(|&(_, character)| matches!(character, ' ' | ','))
        .map(|(index, _)| &rest[..index])
        .filter_map(|prefix| indices.get(prefix).copied())
        .chain(
            rest.split_whitespace()
                .filter_map(|word| indices.get(word.trim_matches(',')).copied()),
        )
        .min()
}

fn output_header(line: &str, indices: &HashMap<&str, usize>) -> Option<usize> {
    let rest = line.strip_prefix("  ")?;
    let rest = rest
        .strip_prefix('+')
        .or_else(|| rest.strip_prefix('-'))
        .or_else(|| rest.strip_prefix('~'))?;
    let (candidate, _) = rest.trim_start().split_once('=')?;
    indices.get(candidate.trim()).copied()
}

fn heredoc_start(line: &str) -> Option<String> {
    let mut quoted = false;
    let mut escaped = false;
    let marker = line.char_indices().find_map(|(index, character)| {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            return None;
        }
        if character == '"' {
            quoted = true;
            return None;
        }
        (character == '<'
            && line[index..].starts_with("<<")
            && line[..index].trim_end().ends_with('='))
        .then_some(index)
    })?;
    let mut value = line[marker + 2..].trim_start();
    value = value.strip_prefix('-').unwrap_or(value).trim_start();
    let terminator = value.split_whitespace().next()?;
    (!terminator.is_empty()
        && terminator
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')))
    .then(|| terminator.to_owned())
}

struct OpenHeredoc {
    terminator: String,
    marker_column: usize,
}

impl OpenHeredoc {
    fn opened_by(line: &str) -> Option<Self> {
        heredoc_start(line).map(|terminator| Self {
            terminator,
            marker_column: heredoc_marker_column(line),
        })
    }
}

// Terraform and OpenTofu indent heredoc lines one level past the opening attribute and keep a
// two-column action slot there, so a changed line's marker sits two columns right of the name.
fn heredoc_marker_column(opening: &str) -> usize {
    let indent = opening.len() - opening.trim_start_matches(' ').len();
    let rest = &opening[indent..];
    let name_column = if ["+ ", "- ", "~ "]
        .iter()
        .any(|marker| rest.starts_with(marker))
    {
        indent + 2
    } else {
        indent
    };
    name_column + 2
}

fn heredoc_end(line: &str, terminator: &str) -> bool {
    let trimmed = line.trim();
    if trimmed == terminator {
        return true;
    }
    trimmed
        .strip_prefix(terminator)
        .is_some_and(|suffix| suffix.trim_start().starts_with("->"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_blocks(
        text: &str,
        resource_addresses: &[String],
        output_names: &[String],
    ) -> Vec<PlanBlock> {
        split_blocks_with_line_kinds(text, resource_addresses, output_names).0
    }

    #[test]
    fn preserves_text_order_newlines_and_sensitive_markers() {
        let source = "first\n  password = (sensitive value)\nlast\n";

        let document = parse_document(
            source.as_bytes().to_vec(),
            &["terraform_data.api".to_owned()],
            &[],
        )
        .expect("text should parse");

        assert_eq!(document.text(), source);
        assert!(!format!("{document:?}").contains("password"));
    }

    #[test]
    fn indexes_each_resource_address_when_the_raw_block_is_parsed() {
        let source = "preamble\n  # terraform_data.api will be created\n  + resource \"terraform_data\" \"api\" {}\n";
        let document = parse_document(
            source.as_bytes().to_vec(),
            &["terraform_data.api".to_owned()],
            &[],
        )
        .expect("text should parse");

        let block = document
            .block_for_address("terraform_data.api")
            .expect("resource block should be indexed");
        assert_eq!(block.lines(), &(1..4));
        assert_eq!(block.addresses(), ["terraform_data.api"]);
    }

    #[test]
    fn rejects_invalid_utf8_without_exposing_bytes() {
        assert_eq!(
            parse_document(vec![0xff], &[], &[]),
            Err(PlanParseError::InvalidUtf8)
        );
    }

    #[test]
    fn classifies_display_intro_notes_and_heredoc_values_without_changing_source() {
        let source = "\nTerraform used the selected providers to generate the following execution\nplan. Resource actions are indicated with the following symbols:\n  + create\n\nTerraform will perform the following actions:\n\n  # terraform_data.api will be created\n  + resource \"terraform_data\" \"api\" {\n      value = <<EOF\n  Plan: 9 to add, 9 to change, 9 to destroy.\n  # value remains a body value\nEOF\n    }\n\nChanges to Outputs:\n  + endpoint = (known after apply)\n\nPlan: 1 to add, 0 to change, 0 to destroy.\n";
        let document = parse_document(
            source.as_bytes().to_vec(),
            &["terraform_data.api".to_owned()],
            &["endpoint".to_owned()],
        )
        .expect("text should parse");

        assert_eq!(document.text(), source);
        assert_eq!(document.line_kind(0), PlanLineKind::Intro);
        assert_eq!(document.line_kind(1), PlanLineKind::Intro);
        assert_eq!(document.line_kind(2), PlanLineKind::Intro);
        assert_eq!(document.line_kind(4), PlanLineKind::Intro);
        assert_eq!(document.line_kind(7), PlanLineKind::ResourceHeader);
        assert_eq!(document.line_kind(9), PlanLineKind::Body);
        for line in [10, 11] {
            assert_eq!(
                document.line_kind(line),
                PlanLineKind::HeredocBody { marker_column: 8 }
            );
        }
        assert_eq!(document.line_kind(12), PlanLineKind::Body);
        assert_eq!(document.line_kind(15), PlanLineKind::OutputSection);
        assert_eq!(document.line_kind(18), PlanLineKind::Summary);
    }

    #[test]
    fn classifies_opentofu_intro_and_current_planned_legend_as_intro() {
        let source = "OpenTofu used the selected providers to generate the following execution\n"
            .to_owned()
            + "plan. Resource actions are indicated with the following symbols:\n"
            + "  + create\n"
            + "  ~ update in-place (current -> planned)\n"
            + "  - destroy\n"
            + "-/+ destroy and then create replacement\n\n"
            + "OpenTofu will perform the following actions:\n\n"
            + "  # terraform_data.api will be created\n"
            + "  + resource \"terraform_data\" \"api\" {}\n\n"
            + "Plan: 1 to add, 0 to change, 0 to destroy.\n";
        let document = parse_document(
            source.as_bytes().to_vec(),
            &["terraform_data.api".to_owned()],
            &[],
        )
        .expect("OpenTofu text should parse");

        for line in 0..9 {
            assert_eq!(document.line_kind(line), PlanLineKind::Intro);
        }
        assert_eq!(document.line_kind(9), PlanLineKind::ResourceHeader);
        assert_eq!(document.line_kind(12), PlanLineKind::Summary);
    }

    #[test]
    fn classifies_resource_headers_and_heredoc_marker_columns_for_both_tools() {
        let addresses = [
            "terraform_data.created".to_owned(),
            "terraform_data.updated".to_owned(),
            "terraform_data.removed".to_owned(),
        ];
        for tool in ["Terraform", "OpenTofu"] {
            let removed_header = format!(
                "  # terraform_data.removed will no longer be managed by {tool}, but will not be destroyed"
            );
            let source = [
                &format!("{tool} will perform the following actions:"),
                "",
                "  # terraform_data.created will be created",
                "  + resource \"terraform_data\" \"created\" {",
                "      + input  = <<-EOT",
                "            - dash",
                "            + plus",
                "        EOT",
                "    }",
                "",
                "  # terraform_data.updated will be updated in-place",
                "  ~ resource \"terraform_data\" \"updated\" {",
                "      ~ input  = <<-EOT",
                "            - item one",
                "          - + item two",
                "            plain",
                "        EOT",
                "        # (1 unchanged attribute hidden)",
                "      ~ nested = {",
                "          ~ \"key\" = <<-EOT",
                "                - value",
                "            EOT -> null",
                "        }",
                "    }",
                "",
                &removed_header,
                "",
                "  # terraform_data.unknown will be created",
                "",
                "Plan: 1 to add, 1 to change, 0 to destroy.",
            ]
            .join("\n");
            let document = parse_document(source.into_bytes(), &addresses, &[])
                .expect("synthetic text should parse");

            let heredoc = |marker_column| PlanLineKind::HeredocBody { marker_column };
            assert_eq!(
                (0..document.line_count())
                    .map(|line| document.line_kind(line))
                    .collect::<Vec<_>>(),
                [
                    [PlanLineKind::Intro; 2].as_slice(),
                    &[PlanLineKind::ResourceHeader],
                    &[PlanLineKind::Body; 2],
                    &[heredoc(10); 2],
                    &[PlanLineKind::Body; 3],
                    &[PlanLineKind::ResourceHeader],
                    &[PlanLineKind::Body; 2],
                    &[heredoc(10); 3],
                    &[PlanLineKind::Body],
                    &[PlanLineKind::Note],
                    &[PlanLineKind::Body; 2],
                    &[heredoc(14)],
                    &[PlanLineKind::Body; 4],
                    &[PlanLineKind::ResourceHeader],
                    &[PlanLineKind::Body],
                    &[PlanLineKind::Note],
                    &[PlanLineKind::Body],
                    &[PlanLineKind::Summary],
                ]
                .concat(),
                "{tool}"
            );
        }
    }

    #[test]
    fn keeps_unknown_plan_text_as_body() {
        let source = "Plan: this is application text\nfollowing body text\n";
        let document =
            parse_document(source.as_bytes().to_vec(), &[], &[]).expect("text should parse");

        assert_eq!(document.line_kind(0), PlanLineKind::Body);
        assert_eq!(document.line_kind(1), PlanLineKind::Body);
    }

    #[test]
    fn keeps_nested_heading_like_text_inside_the_resource_block() {
        let source = "Terraform will perform the following actions:\n\n  # terraform_data.api will be updated in-place\n  ~ resource \"terraform_data\" \"api\" {\n      value = <<EOF\n  # terraform_data.worker will be created\nEOF\n    }\n\nChanges to Outputs:\n  ~ endpoint = \"new\"\n\nPlan: 0 to add, 1 to change, 0 to destroy.\n";
        let blocks = split_blocks(
            source,
            &[
                "terraform_data.api".to_owned(),
                "terraform_data.worker".to_owned(),
            ],
            &["endpoint".to_owned()],
        );

        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[1].lines(), &(2..9));
        assert_eq!(blocks[2].lines(), &(9..10));
        assert_eq!(blocks[3].lines(), &(10..12));
    }

    #[test]
    fn keeps_plan_summary_outside_the_last_resource_block() {
        let source = "  # terraform_data.api will be updated in-place\n  ~ resource \"terraform_data\" \"api\" {\n      input = \"after\"\n    }\n\nPlan: 0 to add, 1 to change, 0 to destroy.\n";
        let blocks = split_blocks(source, &["terraform_data.api".to_owned()], &[]);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lines(), &(0..5));
        assert_eq!(blocks[1].lines(), &(5..7));
    }

    #[test]
    fn ignores_shift_markers_inside_quoted_values() {
        let source = "  # terraform_data.api will be updated in-place\n  ~ resource \"terraform_data\" \"api\" {\n      input = \"a << b\"\n    }\n\n  # terraform_data.worker will be created\n  + resource \"terraform_data\" \"worker\" {\n      input = \"worker\"\n    }\n";
        let blocks = split_blocks(
            source,
            &[
                "terraform_data.api".to_owned(),
                "terraform_data.worker".to_owned(),
            ],
            &[],
        );

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lines(), &(0..5));
        assert_eq!(blocks[1].lines(), &(5..10));
    }

    #[test]
    fn recognizes_moved_and_removed_resource_headers() {
        let source = "  # terraform_data.old has moved to terraform_data.new\n  ~ resource \"terraform_data\" \"new\" {\n      input = \"new\"\n    }\n\n  # terraform_data.removed will no longer be managed by Terraform, but will not be destroyed\n  - resource \"terraform_data\" \"removed\" {\n      input = \"removed\"\n    }\n";
        let blocks = split_blocks(
            source,
            &[
                "terraform_data.new".to_owned(),
                "terraform_data.removed".to_owned(),
            ],
            &[],
        );

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lines(), &(0..5));
        assert_eq!(blocks[1].lines(), &(5..10));
    }

    #[test]
    fn indexes_resource_headers_without_prefix_collisions_or_unknown_matches() {
        let resource_addresses = [
            "terraform_data.api".to_owned(),
            "terraform_data.api_extra".to_owned(),
            "module.service[\"a, b\"]".to_owned(),
            "module.service[\"will be here\"]".to_owned(),
            "terraform_data.moved_new".to_owned(),
            "terraform_data.moved_old".to_owned(),
        ];
        let mut resource_indices = HashMap::new();
        for (index, address) in resource_addresses.iter().enumerate() {
            resource_indices.entry(address.as_str()).or_insert(index);
        }

        for (case_name, line, expected) in [
            (
                "prefix_collision",
                "  # terraform_data.api_extra will be created",
                Some(1),
            ),
            (
                "quoted_comma",
                "  # module.service[\"a, b\"] will be created",
                Some(2),
            ),
            (
                "quoted_action_phrase",
                "  # module.service[\"will be here\"] will be created",
                Some(3),
            ),
            (
                "moved_uses_metadata_order",
                "  # terraform_data.moved_old has moved to terraform_data.moved_new",
                Some(4),
            ),
            (
                "unknown_address",
                "  # terraform_data.unknown will be created",
                None,
            ),
            (
                "non_action_heading",
                "  # terraform_data.api is unchanged",
                None,
            ),
        ] {
            assert_eq!(
                resource_header(line, &resource_indices),
                expected,
                "case: {case_name}; line: {line}"
            );
        }

        let duplicate_addresses = ["terraform_data.duplicate", "terraform_data.duplicate"];
        let mut duplicate_indices = HashMap::new();
        for (index, address) in duplicate_addresses.iter().enumerate() {
            duplicate_indices.entry(*address).or_insert(index);
        }
        assert_eq!(
            resource_header(
                "  # terraform_data.duplicate will be created",
                &duplicate_indices
            ),
            Some(0)
        );
    }

    #[test]
    fn indexes_output_headers_with_the_existing_action_and_assignment_rules() {
        let output_names = ["endpoint".to_owned(), "endpoint_extra".to_owned()];
        let mut output_indices = HashMap::new();
        for (index, name) in output_names.iter().enumerate() {
            output_indices.entry(name.as_str()).or_insert(index);
        }

        for (case_name, line, expected) in [
            (
                "prefix_collision",
                "  + endpoint_extra = (known after apply)",
                Some(1),
            ),
            ("known_output", "  ~ endpoint = \"new\"", Some(0)),
            ("missing_assignment", "  + endpoint_extra", None),
            ("nested_name", "  + endpoint_extra.value = \"new\"", None),
            ("missing_action_marker", "  endpoint = \"new\"", None),
        ] {
            assert_eq!(
                output_header(line, &output_indices),
                expected,
                "case: {case_name}; line: {line}"
            );
        }

        let duplicate_names = ["duplicate", "duplicate"];
        let mut duplicate_indices = HashMap::new();
        for (index, name) in duplicate_names.iter().enumerate() {
            duplicate_indices.entry(*name).or_insert(index);
        }
        assert_eq!(
            output_header("  + duplicate = (known after apply)", &duplicate_indices),
            Some(0)
        );
    }

    #[test]
    fn preserves_indexed_resource_output_and_unknown_heading_block_ranges() {
        let source = "preamble\n  # terraform_data.moved_old has moved to terraform_data.moved_new\n  ~ resource \"terraform_data\" \"new\" {\n      value = \"new\"\n    }\n  # terraform_data.api_extra will be created\n  + resource \"terraform_data\" \"api_extra\" {\n      value = \"extra\"\n    }\n  # terraform_data.unknown will be created\nChanges to Outputs:\n  + endpoint_extra = (known after apply)\n  ~ endpoint = \"new\"\nPlan: 0 to add, 2 to change, 0 to destroy.\n";
        let blocks = split_blocks(
            source,
            &[
                "terraform_data.moved_new".to_owned(),
                "terraform_data.moved_old".to_owned(),
                "terraform_data.api_extra".to_owned(),
            ],
            &["endpoint".to_owned(), "endpoint_extra".to_owned()],
        );

        assert_eq!(blocks.len(), 7);
        assert_eq!(blocks[0].lines(), &(0..1));
        assert_eq!(blocks[1].lines(), &(1..5));
        assert_eq!(blocks[2].lines(), &(5..10));
        assert_eq!(blocks[3].lines(), &(10..11));
        assert_eq!(blocks[4].lines(), &(11..12));
        assert_eq!(blocks[5].lines(), &(12..13));
        assert_eq!(blocks[6].lines(), &(13..15));
    }

    #[test]
    fn recognizes_heredoc_termination_with_deleted_value() {
        let source = "  # terraform_data.api will be updated in-place\n  ~ resource \"terraform_data\" \"api\" {\n      value = <<-EOT\n      first\n      second\n      EOT -> null\n    }\n\n  # terraform_data.worker will be updated in-place\n  ~ resource \"terraform_data\" \"worker\" {\n      input = \"new\"\n    }\n\nPlan: 0 to add, 2 to change, 0 to destroy.\n";
        let blocks = split_blocks(
            source,
            &[
                "terraform_data.api".to_owned(),
                "terraform_data.worker".to_owned(),
            ],
            &[],
        );
        let document = parse_document(
            source.as_bytes().to_vec(),
            &[
                "terraform_data.api".to_owned(),
                "terraform_data.worker".to_owned(),
            ],
            &[],
        )
        .expect("text should parse");

        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].lines(), &(0..8));
        assert_eq!(blocks[1].lines(), &(8..13));
        assert_eq!(blocks[2].lines(), &(13..15));
        assert_eq!(
            document
                .filter("worker")
                .lines_with_indices()
                .map(|(_, line)| line)
                .collect::<Vec<_>>(),
            vec![
                "  # terraform_data.worker will be updated in-place",
                "  ~ resource \"terraform_data\" \"worker\" {",
                "      input = \"new\"",
                "    }",
                "",
                "Plan: 0 to add, 2 to change, 0 to destroy.",
                "",
            ]
        );
    }
}
