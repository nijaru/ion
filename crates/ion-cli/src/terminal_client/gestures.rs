//! Confirmation belongs to one action and target, never to a press-count ladder.
use ion_app::{QuestionRequestId, StopTarget};
use ion_terminal::KeyEvent;
use tokio::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Stop(StopTarget),
    Clear(u64),
    Exit,
    Clipboard(tokio::task::Id),
    Question(QuestionRequestId),
}

#[derive(Default)]
pub(super) struct Gestures {
    pending: Option<Pending>,
}

struct Pending {
    target: Target,
    key: String,
    deadline: Instant,
    consumed: bool,
    prompt: String,
}

impl Gestures {
    pub(super) fn reset(&mut self) {
        self.pending = None;
    }

    pub(super) fn invalidate(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.consumed = true;
        }
    }

    pub(super) fn different_pending_key(&self, key: KeyEvent) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            !pending.consumed && pending.key != crate::keybindings::key_label(key)
        })
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    pub(super) fn expire(&mut self, now: Instant) {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.reset();
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent, now: Instant) {
        self.expire(now);
        if let Some(pending) = &mut self.pending
            && pending.consumed
            && pending.key == crate::keybindings::key_label(key)
        {
            // A held/repeated chord must go quiet before it can start a new
            // destructive intent, even after a slow stop finishes.
            pending.deadline = now + WINDOW;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.key != crate::keybindings::key_label(key))
        {
            self.reset();
        }
    }

    // Keep a consumed gesture until neutral input or expiry. Work may finish
    // between taps: the next tap must not become permission to clear or quit.
    pub(super) fn reconcile_stop(&mut self, current: Option<&StopTarget>) {
        if let Some(pending) = &mut self.pending
            && let Target::Stop(target) = &pending.target
            && Some(target) != current
        {
            pending.consumed = true;
        }
    }

    pub(super) fn reconcile_local(
        &mut self,
        baseline: u64,
        composer: bool,
        question: Option<QuestionRequestId>,
        clipboard: Option<tokio::task::Id>,
    ) {
        let invalid = self
            .pending
            .as_ref()
            .is_some_and(|pending| match pending.target {
                Target::Clear(expected) => !composer || expected != baseline,
                Target::Exit => !composer,
                Target::Question(expected) => Some(expected) != question,
                Target::Clipboard(expected) => Some(expected) != clipboard,
                Target::Stop(_) => false,
            });
        if invalid {
            self.invalidate();
        }
    }

    pub(super) fn prompt(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|pending| !pending.consumed)
            .map(|pending| pending.prompt.as_str())
    }

    pub(super) fn confirm(&mut self, target: Target, key: KeyEvent, now: Instant) -> bool {
        self.key(key, now);
        if key.repeat {
            return false;
        }
        if let Some(pending) = &mut self.pending {
            let confirmed = !pending.consumed && pending.target == target;
            pending.consumed = true;
            pending.deadline = now + WINDOW;
            return confirmed;
        }
        let key = crate::keybindings::key_label(key);
        let action = match target {
            Target::Stop(_) => "Stop work?",
            Target::Clear(_) => "Clear draft?",
            Target::Exit => "Exit Ion?",
            Target::Clipboard(_) => "Cancel paste?",
            Target::Question(_) => "Cancel question?",
        };
        self.pending = Some(Pending {
            target,
            prompt: format!("{action} {key} again"),
            key,
            deadline: now + WINDOW,
            consumed: false,
        });
        false
    }
}

impl super::Frontend {
    pub(super) fn begin_key(&mut self, key: KeyEvent) -> bool {
        self.gestures.expire(Instant::now());
        let dismiss = self
            .keys
            .matches(crate::keybindings::KeyAction::Cancel, key)
            && self.gestures.different_pending_key(key);
        self.gestures.key(key, Instant::now());
        self.reconcile_local_gestures();
        dismiss
    }

    pub(super) fn reconcile_local_gestures(&mut self) {
        self.gestures.expire(Instant::now());
        self.gestures.reconcile_local(
            self.editor.baseline,
            self.input_target() == super::interaction::InputTarget::Composer,
            self.question.as_ref().map(super::QuestionModal::request),
            self.clipboard_job.as_ref().map(|job| job.task.id()),
        );
    }

    pub(super) fn stop_key(&mut self, key: KeyEvent, target: Option<StopTarget>) -> Option<bool> {
        self.gestures.reconcile_stop(target.as_ref());
        let target = target?;
        let cancel_composer = self
            .keys
            .matches(crate::keybindings::KeyAction::Cancel, key)
            && self.input_target() == super::interaction::InputTarget::Composer
            && !self.editor.completion.active()
            && !self.editor.completion.wants_files();
        if !self
            .keys
            .matches(crate::keybindings::KeyAction::Interrupt, key)
            && !cancel_composer
        {
            return None;
        }
        self.editor.break_edit_focus();
        Some(
            self.gestures
                .confirm(Target::Stop(target), key, Instant::now()),
        )
    }

    pub(super) fn question_key(&mut self, key: KeyEvent) -> super::QuestionAction {
        if self.begin_key(key) {
            return super::QuestionAction::None;
        }
        let question = self.question.as_ref().expect("question owns input");
        if self
            .keys
            .matches(crate::keybindings::KeyAction::Cancel, key)
            && !self
                .gestures
                .confirm(Target::Question(question.request()), key, Instant::now())
        {
            return super::QuestionAction::None;
        }
        self.question
            .as_mut()
            .expect("question owns input")
            .key(key, &self.keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_terminal::{KeyCode, Modifiers};

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), Modifiers::CONTROL)
    }

    #[test]
    fn repeat_events_neither_arm_nor_confirm_a_destructive_intent() {
        let now = Instant::now();
        for target in [Target::Clear(1), Target::Exit] {
            for press in [key('c'), KeyEvent::new(KeyCode::Esc, Modifiers::NONE)] {
                let repeat = KeyEvent {
                    repeat: true,
                    ..press
                };
                let mut gestures = Gestures::default();
                assert!(!gestures.confirm(target.clone(), repeat, now));
                assert!(gestures.prompt().is_none());
                assert!(!gestures.confirm(target.clone(), press, now));
                for second in 0..3 {
                    assert!(!gestures.confirm(
                        target.clone(),
                        repeat,
                        now + Duration::from_secs(second)
                    ));
                    assert!(gestures.prompt().is_some());
                }
                assert!(gestures.confirm(target.clone(), press, now + Duration::from_secs(2)));
                assert!(!gestures.confirm(target.clone(), repeat, now + Duration::from_secs(4)));
                assert!(gestures.prompt().is_none());
            }
        }
    }

    #[test]
    fn repeated_consumed_keys_require_a_quiet_window_before_a_fresh_intent() {
        let mut gestures = Gestures::default();
        let now = Instant::now();
        assert!(!gestures.confirm(Target::Clear(1), key('c'), now));
        assert!(gestures.confirm(Target::Clear(1), key('c'), now));
        for second in 1..10 {
            assert!(!gestures.confirm(Target::Exit, key('c'), now + Duration::from_secs(second)));
        }
        let quiet = now + Duration::from_secs(12);
        assert!(!gestures.confirm(Target::Exit, key('c'), quiet));
        assert!(gestures.confirm(Target::Exit, key('c'), quiet));
    }

    #[test]
    fn confirmation_expires_and_never_crosses_keys_targets_or_a_consumed_gesture() {
        let mut gestures = Gestures::default();
        let now = Instant::now();
        assert!(!gestures.confirm(Target::Clear(1), key('c'), now));
        assert!(!gestures.confirm(Target::Clear(2), key('c'), now));
        assert!(!gestures.confirm(Target::Exit, key('c'), now));
        gestures.key(key('a'), now);
        assert!(!gestures.confirm(Target::Exit, key('c'), now));
        assert!(gestures.confirm(Target::Exit, key('c'), now));
        assert!(!gestures.confirm(Target::Exit, key('c'), now));
        assert!(!gestures.confirm(Target::Exit, key('c'), now + WINDOW));
        assert!(!gestures.confirm(Target::Exit, key('d'), now + WINDOW));
        assert!(gestures.confirm(Target::Exit, key('d'), now + WINDOW));
    }
}
