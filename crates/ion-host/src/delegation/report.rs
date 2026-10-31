//! Latest bounded visible response. Gaps/retries never splice unrelated prefixes.
use ion_core::{DelegationFailure, DelegationPartialOutput};

use crate::{ControlUpdate, Observation, OperationOutcome};

#[derive(Default)]
pub(super) struct ChildReport {
    pub progress: super::ChildProgress,
    pub failure: Option<DelegationFailure>,
    answer: Option<String>,
}

impl ChildReport {
    pub fn new(progress: super::ChildProgress) -> Self {
        Self {
            progress,
            failure: None,
            answer: None,
        }
    }

    pub fn observe(&mut self, update: ControlUpdate<()>) {
        match update {
            ControlUpdate::Activity(Observation::Gap { skipped }) if skipped != 0 => {
                self.progress.gap();
            }
            ControlUpdate::Activity(Observation::Event { event, .. }) => {
                self.progress.observe(&event)
            }
            ControlUpdate::Settled(settlement) => {
                if let OperationOutcome::Turn(outcome) = settlement.outcome {
                    self.settled(outcome);
                }
            }
            _ => {}
        }
    }

    fn settled(&mut self, outcome: Result<String, ion_core::AgentError>) {
        match outcome {
            Ok(text) => {
                // Reliable settlement supersedes a lost progress prefix. Keep
                // its bounded visible text even if subsequent cleanup fails.
                self.progress.settle(Some(&text));
                self.answer = Some(text);
            }
            Err(error) => {
                self.progress.settle(None);
                let failure = if super::infrastructure(&error) {
                    DelegationFailure::Infrastructure(error.to_string())
                } else {
                    DelegationFailure::Execution(error.to_string())
                };
                self.fail(failure);
            }
        }
    }

    pub fn fail(&mut self, failure: DelegationFailure) {
        super::retain_failure(&mut self.failure, failure);
    }

    pub fn output(&self) -> Option<DelegationPartialOutput> {
        self.progress.output()
    }

    pub fn take_result(&mut self) -> Result<Option<String>, DelegationFailure> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        self.answer.take().map(Some).ok_or_else(|| {
            DelegationFailure::Infrastructure(
                "child control joined without its successful answer".into(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::Content;
    use ion_core::{AgentEvent, MAX_DELEGATION_PARTIAL_BYTES};

    fn event(report: &mut ChildReport, event: AgentEvent) {
        report.progress.observe(&event);
    }

    #[test]
    fn utf8_prefix_stays_bounded_and_does_not_fill_a_truncation_hole() {
        let mut output = DelegationPartialOutput::default();
        output.push_text(&"x".repeat(MAX_DELEGATION_PARTIAL_BYTES - 1));
        output.push_text("🚀");
        output.push_text("z");
        assert_eq!(output.text, "x".repeat(MAX_DELEGATION_PARTIAL_BYTES - 1));
        assert!(output.truncated);
        assert!(!output.observation_gap);
    }

    #[test]
    fn reports_replace_responses_and_discard_retries_and_gapped_prefixes() {
        let mut report = ChildReport::default();
        event(
            &mut report,
            AgentEvent::AssistantCommitted {
                turn: 1,
                content: vec![
                    Content::Thinking("hidden".into()),
                    Content::Text("committed".into()),
                    Content::Text("second block".into()),
                ],
                tool_activities: vec![],
                termination: ion_ai::ResponseTermination::Completed,
            },
        );
        assert_eq!(report.output().unwrap().text, "committed\nsecond block");
        event(
            &mut report,
            AgentEvent::TextDelta("discarded attempt".into()),
        );
        event(&mut report, AgentEvent::ResponseRestarted);
        assert_eq!(report.output().unwrap().text, "committed\nsecond block");
        event(
            &mut report,
            AgentEvent::ThinkingDelta {
                block: 0,
                text: "never disclose".into(),
            },
        );
        event(&mut report, AgentEvent::TextDelta("fresh 🚀".into()));
        assert_eq!(report.output().unwrap().text, "fresh 🚀");
        report.observe(ControlUpdate::Activity(Observation::Gap { skipped: 3 }));
        event(&mut report, AgentEvent::TextDelta("orphan suffix".into()));
        let gap = report.output().unwrap();
        assert!(gap.text.is_empty() && gap.observation_gap);
        event(&mut report, AgentEvent::ResponseRestarted);
        event(&mut report, AgentEvent::TextDelta("new full prefix".into()));
        assert_eq!(report.output().unwrap().text, "new full prefix");
        assert!(!report.output().unwrap().observation_gap);
        event(
            &mut report,
            AgentEvent::AssistantCommitted {
                turn: 1,
                content: vec![Content::Text("authoritative text".into())],
                tool_activities: vec![],
                termination: ion_ai::ResponseTermination::Completed,
            },
        );
        assert_eq!(report.output().unwrap().text, "authoritative text");
        report.observe(ControlUpdate::Activity(Observation::Gap { skipped: 7 }));
        report.settled(Ok("reliable settlement despite gap".into()));
        report.fail(DelegationFailure::Infrastructure(
            "late cleanup fault".into(),
        ));
        assert!(report.take_result().is_err());
        let output = report.output().unwrap();
        assert_eq!(output.text, "reliable settlement despite gap");
        assert!(!output.observation_gap);
    }
}
