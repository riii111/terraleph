use std::fmt::{Debug, Formatter};
use std::time::{Duration, Instant};

use super::{
    execution::{Diagnostic, ExecutionStage, ExecutionState, SensitiveValue},
    review::PlanReview,
};

const REDACTION_TEXT: &str = "(sensitive value)";
const PROTECTED_REDACTION: &str = "\u{0}terraleph-redacted\u{0}";
const FLASH_DURATION: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyTarget {
    Diagnostic,
    Plan,
    Execution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyResult {
    Written,
    // Handed to the terminal with OSC 52, which cannot confirm that it took the text.
    SentToTerminal,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyNotice {
    Copied { target: CopyTarget },
    SentToTerminal { target: CopyTarget },
    Failed,
}

impl CopyNotice {
    #[must_use]
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Copied { .. } => "Copied.",
            Self::SentToTerminal { .. } => "Sent to terminal clipboard.",
            Self::Failed => "Copy failed.",
        }
    }

    #[must_use]
    const fn duration(self) -> Duration {
        match self {
            Self::Copied { .. } | Self::SentToTerminal { .. } => Duration::from_secs(3),
            Self::Failed => Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CopyFeedback {
    notice: Option<CopyNotice>,
    notice_until: Option<Instant>,
    flash_until: Option<Instant>,
}

impl CopyFeedback {
    #[must_use]
    pub(crate) const fn notice(&self) -> Option<CopyNotice> {
        self.notice
    }

    #[must_use]
    pub(crate) fn notice_at(&self, now: Instant) -> Option<CopyNotice> {
        self.notice_until
            .is_some_and(|until| now < until)
            .then_some(self.notice)
            .flatten()
    }

    #[must_use]
    pub(crate) fn flash_active(&self, now: Instant) -> bool {
        self.flash_until.is_some_and(|until| now < until)
    }

    #[must_use]
    pub(crate) const fn pending(&self) -> bool {
        self.notice_until.is_some() || self.flash_until.is_some()
    }

    pub(crate) fn record(
        &mut self,
        target: CopyTarget,
        result: CopyResult,
        now: Instant,
        flash: bool,
    ) {
        let notice = match result {
            CopyResult::Written => CopyNotice::Copied { target },
            CopyResult::SentToTerminal => CopyNotice::SentToTerminal { target },
            CopyResult::Failed => CopyNotice::Failed,
        };
        self.notice = Some(notice);
        self.notice_until = Some(now + notice.duration());
        self.flash_until = (flash && result != CopyResult::Failed).then(|| now + FLASH_DURATION);
    }

    pub(crate) fn clear_expired(&mut self, now: Instant) -> bool {
        let clear_notice = self.notice_until.is_some_and(|until| now >= until);
        let clear_flash = self.flash_until.is_some_and(|until| now >= until);
        if clear_notice {
            self.notice = None;
            self.notice_until = None;
        }
        if clear_flash {
            self.flash_until = None;
        }
        clear_notice || clear_flash
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CopyEffect {
    target: CopyTarget,
    text: String,
}

impl CopyEffect {
    #[must_use]
    pub(crate) const fn new(target: CopyTarget, text: String) -> Self {
        Self { target, text }
    }

    #[must_use]
    pub(crate) const fn target(&self) -> CopyTarget {
        self.target
    }

    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

impl Debug for CopyEffect {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopyEffect")
            .field("target", &self.target)
            .field("text", &"<redacted>")
            .finish()
    }
}

#[must_use]
pub(crate) fn plan_effect(review: &PlanReview) -> CopyEffect {
    let mut text = diagnostic_text(review.diagnostics(), review.metadata().sensitive_values());
    if !text.is_empty() && !review.document().text().is_empty() {
        text.push('\n');
    }
    text.push_str(review.document().text());
    CopyEffect::new(CopyTarget::Plan, text)
}

#[must_use]
pub(crate) fn diagnostic_effect(
    diagnostics: &[Diagnostic],
    fallback: Option<&str>,
    sensitive_values: &[SensitiveValue],
) -> CopyEffect {
    let text = if diagnostics.is_empty() {
        sanitize_text(
            fallback.unwrap_or("Diagnostic unavailable."),
            sensitive_values,
        )
    } else {
        diagnostic_text(diagnostics, sensitive_values)
    };
    CopyEffect::new(CopyTarget::Diagnostic, text)
}

#[must_use]
pub(crate) fn execution_effect(state: &ExecutionState) -> CopyEffect {
    let mut sections = Vec::new();
    match state.stage() {
        ExecutionStage::ApplySucceeded => sections.push("Apply complete.".to_owned()),
        ExecutionStage::ApplyInterrupted => {
            sections.push("Apply interrupted.".to_owned());
            sections.push("Changes may already be applied.".to_owned());
        }
        ExecutionStage::ApplyFailed => {
            sections.push("Apply failed.".to_owned());
            sections.push("Changes may already be applied.".to_owned());
        }
        _ => sections.push(format!("{} failed.", state.context().tool_name())),
    }
    let progress = state.progress();
    sections.push(format!(
        "Completed: {}/{}    Failed: {}    Incomplete: {}    Skipped: {}",
        progress.completed_count(),
        progress.targets().len(),
        progress.failed_count(),
        progress.incomplete_count(),
        progress.skipped_count(),
    ));
    let elapsed = state.elapsed_at(std::time::Instant::now());
    sections.push(format!(
        "Elapsed: {}.{:01}s",
        elapsed.as_secs(),
        elapsed.subsec_millis() / 100
    ));
    if let Some(result) = state.result() {
        let log = progress.log();
        if let Some(summary) = result.summary_line()
            && !log
                .iter()
                .any(|line| line.text.lines().any(|text| text == summary))
        {
            sections.push(sanitize_text(summary, state.progress().sensitive_values()));
        }
        sections.extend(log.iter().map(|line| line.text.clone()));
    }
    CopyEffect::new(CopyTarget::Execution, sections.join("\n"))
}

pub(crate) fn sanitize_text(text: &str, sensitive_values: &[SensitiveValue]) -> String {
    let mut values = sensitive_values
        .iter()
        .filter(|value| match value {
            SensitiveValue::Text(value) | SensitiveValue::Number(value) => !value.is_empty(),
            SensitiveValue::Bool(_) => true,
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(sensitive_value_text(value).len()));
    values.dedup();

    let sanitized = values.into_iter().fold(
        text.replace(REDACTION_TEXT, PROTECTED_REDACTION),
        |text, value| match value {
            SensitiveValue::Text(value) if value.len() < 4 && !value.is_empty() => {
                transform_unmasked(&text, |text| redact_lines_containing(text, value))
            }
            SensitiveValue::Text(value) => {
                transform_unmasked(&text, |text| text.replace(value, PROTECTED_REDACTION))
            }
            SensitiveValue::Number(value) => {
                transform_unmasked(&text, |text| replace_scalar_tokens(text, value, true))
            }
            SensitiveValue::Bool(value) => transform_unmasked(&text, |text| {
                replace_scalar_tokens(text, if *value { "true" } else { "false" }, false)
            }),
        },
    );
    sanitized.replace(PROTECTED_REDACTION, REDACTION_TEXT)
}

fn transform_unmasked(text: &str, transform: impl Fn(&str) -> String) -> String {
    text.split(PROTECTED_REDACTION)
        .map(transform)
        .collect::<Vec<_>>()
        .join(PROTECTED_REDACTION)
}

fn redact_lines_containing(text: &str, value: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.contains(value) {
                if line.ends_with('\n') {
                    format!("{PROTECTED_REDACTION}\n")
                } else {
                    PROTECTED_REDACTION.to_owned()
                }
            } else {
                line.to_owned()
            }
        })
        .collect()
}

fn sensitive_value_text(value: &SensitiveValue) -> &str {
    match value {
        SensitiveValue::Text(value) | SensitiveValue::Number(value) => value,
        SensitiveValue::Bool(value) if *value => "true",
        SensitiveValue::Bool(_) => "false",
    }
}

fn replace_scalar_tokens(text: &str, value: &str, numeric: bool) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, _) in text.match_indices(value) {
        let end = start + value.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let is_boundary = |character: Option<char>| {
            !character.is_some_and(|character| {
                character.is_ascii_alphanumeric()
                    || character == '_'
                    || (numeric && matches!(character, '.' | '-'))
            })
        };
        if !is_boundary(before) || !is_boundary(after) {
            continue;
        }
        result.push_str(&text[cursor..start]);
        result.push_str(PROTECTED_REDACTION);
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

fn diagnostic_text(diagnostics: &[Diagnostic], sensitive_values: &[SensitiveValue]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let text = diagnostic.detail.as_ref().map_or_else(
                || diagnostic.summary.clone(),
                |detail| format!("{}\n{detail}", diagnostic.summary),
            );
            sanitize_text(&text, sensitive_values)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::app::{
        execution::{
            ApplyStatus, DiagnosticSeverity, DiagnosticSource, EventStream, ExecutionContext,
            ExecutionEvent, ExecutionEventKind, ExecutionLogLine,
        },
        plan::Plan,
        review::{
            PlanBlock, PlanBlockKind, PlanDocument, PlanMetadata,
            test_support::{plan_document, plan_document_with_blocks},
        },
    };

    use super::*;

    #[test]
    fn plan_copy_preserves_diagnostics_and_complete_show_text() {
        struct Case {
            name: &'static str,
            document: PlanDocument,
            metadata: PlanMetadata,
            diagnostics: Vec<Diagnostic>,
            expected: String,
            source: String,
        }

        let warning = |detail: Option<&str>| Diagnostic {
            severity: DiagnosticSeverity::Warning,
            summary: "Provider warning".to_owned(),
            detail: detail.map(str::to_owned),
            address: None,
            position: None,
            source: DiagnosticSource::Terraform,
        };
        let plan_text = "Terraform plan body\n";
        let show_text =
            "Terraform used the selected providers to generate the following execution\n"
                .to_owned()
                + "plan. Resource actions are indicated with the following symbols:\n\n"
                + "  # terraform_data.api will be created\n"
                + "  + resource \"terraform_data\" \"api\" {\n"
                + "      value = (sensitive value)\n"
                + "    }\n\n"
                + "Changes to Outputs:\n"
                + "  + endpoint = (known after apply)\n\n"
                + "Plan: 1 to add, 0 to change, 0 to destroy.\n";
        let show_end = show_text.split('\n').count();

        for case in [
            Case {
                name: "summary_only",
                document: plan_document(plan_text.to_owned()),
                metadata: PlanMetadata::new(true),
                diagnostics: vec![warning(None)],
                expected: "Provider warning\nTerraform plan body\n".to_owned(),
                source: plan_text.to_owned(),
            },
            Case {
                name: "summary_and_detail",
                document: plan_document(plan_text.to_owned()),
                metadata: PlanMetadata::new(true),
                diagnostics: vec![warning(Some("warning detail"))],
                expected: "Provider warning\nwarning detail\nTerraform plan body\n".to_owned(),
                source: plan_text.to_owned(),
            },
            Case {
                name: "complete_show_text",
                document: plan_document_with_blocks(
                    show_text.clone(),
                    vec![PlanBlock::new(0..show_end, PlanBlockKind::Common)],
                ),
                metadata: PlanMetadata::new(true),
                diagnostics: Vec::new(),
                expected: show_text.clone(),
                source: show_text,
            },
        ] {
            let review = PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                case.document,
                Plan::empty(),
                case.metadata,
                case.diagnostics,
            );

            let effect = plan_effect(&review);

            assert_eq!(effect.text(), case.expected, "case: {}", case.name);
            assert!(!format!("{effect:?}").contains(case.source.as_str()));
        }
    }

    #[test]
    fn apply_copy_keeps_human_output_without_repeating_the_summary() {
        let now = std::time::Instant::now();
        let mut state = ExecutionState::applying(now, ExecutionContext::loading("/project"));
        let summary = "Apply complete! Resources: 1 added, 0 changed, 0 destroyed.";
        state.record(ExecutionEvent {
            received_at: now,
            kind: ExecutionEventKind::Log(ExecutionLogLine {
                stream: EventStream::Stdout,
                text: format!("Applying saved plan...\n{summary}"),
            }),
        });
        state.finish_apply(ApplyStatus::Succeeded, Some(summary.to_owned()), None, now);

        let effect = state
            .copy_effect(CopyTarget::Execution)
            .expect("apply result copy should be available");
        assert!(effect.text().contains("Applying saved plan..."));
        assert_eq!(effect.text().matches(summary).count(), 1);
    }

    #[test]
    fn scalar_sensitive_values_are_replaced_only_at_token_boundaries() {
        let text = "true feature=true id=1 total=10 version1";
        let sensitive = [
            SensitiveValue::Bool(true),
            SensitiveValue::Number("1".to_owned()),
        ];

        assert_eq!(
            sanitize_text(text, &sensitive),
            "(sensitive value) feature=(sensitive value) id=(sensitive value) total=10 version1"
        );
    }

    #[test]
    fn short_text_sensitive_values_redact_the_whole_affected_line() {
        let sensitive = [SensitiveValue::Text("abc".to_owned())];

        assert_eq!(
            sanitize_text("terraform_data.api\nrequest xabcx failed\nsafe", &sensitive),
            "terraform_data.api\n(sensitive value)\nsafe"
        );
    }

    #[test]
    fn sanitizing_already_redacted_text_is_idempotent() {
        let sensitive = [SensitiveValue::Text("value".to_owned())];
        let text = sanitize_text("value", &sensitive);

        assert_eq!(text, "(sensitive value)");
        assert_eq!(sanitize_text(&text, &sensitive), text);
    }

    #[test]
    fn clearing_feedback_reports_flash_and_notice_expiration() {
        let started_at = Instant::now();
        let mut feedback = CopyFeedback::default();
        feedback.record(CopyTarget::Execution, CopyResult::Written, started_at, true);

        let flash_expired_at = started_at + FLASH_DURATION;
        assert!(feedback.clear_expired(flash_expired_at));
        assert!(feedback.pending());
        assert!(!feedback.flash_active(flash_expired_at));
        assert!(!feedback.clear_expired(flash_expired_at));

        let notice_expired_at = started_at
            + CopyNotice::Copied {
                target: CopyTarget::Execution,
            }
            .duration();
        assert!(feedback.clear_expired(notice_expired_at));
        assert!(!feedback.pending());
        assert!(!feedback.clear_expired(notice_expired_at));
    }

    #[test]
    fn terminal_copy_keeps_the_flash_with_its_own_notice() {
        let started_at = Instant::now();
        let mut feedback = CopyFeedback::default();
        feedback.record(
            CopyTarget::Plan,
            CopyResult::SentToTerminal,
            started_at,
            true,
        );

        assert!(feedback.flash_active(started_at));
        assert_eq!(
            feedback.notice_at(started_at).map(CopyNotice::message),
            Some("Sent to terminal clipboard.")
        );
        assert_eq!(
            feedback.notice_at(started_at + Duration::from_secs(3)),
            None
        );
    }
}
