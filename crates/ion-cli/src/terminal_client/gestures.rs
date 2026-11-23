//! A gesture belongs to one action and target, never to a press-count ladder.
use ion_app::{QuestionRequestId, StopTarget};
use ion_terminal::KeyEvent;
use tokio::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3);
const QUICK_TAP_WINDOW: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Stop(StopTarget),
    Clear(u64),
    Tree(u64),
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
                Target::Clear(expected) | Target::Tree(expected) => {
                    !composer || expected != baseline
                }
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
            .filter(|pending| !pending.consumed && !pending.prompt.is_empty())
            .map(|pending| pending.prompt.as_str())
    }

    pub(super) fn activate(&mut self, target: Target, key: KeyEvent, now: Instant) -> bool {
        self.key(key, now);
        if key.repeat
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.consumed)
        {
            return false;
        }
        // Consume before the caller starts the effect. Settlement must not
        // turn further taps into permission to clear a draft or leave Ion.
        self.pending = Some(Pending {
            target,
            key: crate::keybindings::key_label(key),
            deadline: now + WINDOW,
            consumed: true,
            prompt: String::new(),
        });
        true
    }

    pub(super) fn confirm(&mut self, target: Target, key: KeyEvent, now: Instant) -> bool {
        self.key(key, now);
        if key.repeat {
            return false;
        }
        if let Some(pending) = &mut self.pending {
            if pending.consumed {
                return false;
            }
            let confirmed = pending.target == target;
            if confirmed && matches!(target, Target::Tree(_)) {
                self.reset();
                return true;
            }
            // Suppress follow-on taps, but don't let those taps extend suppression.
            pending.consumed = true;
            pending.deadline = now + WINDOW;
            return confirmed;
        }
        let key = crate::keybindings::key_label(key);
        let action = match target {
            Target::Stop(_) => "Stop work?",
            Target::Clear(_) => "Clear draft?",
            Target::Exit => "Exit Ion?",
            Target::Tree(_) => "",
            Target::Clipboard(_) => "Cancel paste?",
            Target::Question(_) => "Cancel question?",
        };
        let window = if matches!(target, Target::Exit | Target::Tree(_)) {
            QUICK_TAP_WINDOW
        } else {
            WINDOW
        };
        self.pending = Some(Pending {
            target,
            prompt: if action.is_empty() {
                String::new()
            } else {
                format!("{action} {key} again")
            },
            key,
            deadline: now + window,
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

    fn cancel_stops_work(&self) -> bool {
        self.input_target() == super::interaction::InputTarget::Composer
            && !self.editor.completion.active()
            && !self.editor.completion.wants_files()
    }

    pub(super) fn stop_hint(&self) -> String {
        use crate::keybindings::KeyAction;
        if self.cancel_stops_work()
            && let Some(cancel) = self.keys.bound_hint(KeyAction::Cancel)
        {
            return cancel;
        }
        self.keys.hint(KeyAction::Interrupt)
    }

    pub(super) fn stop_key(&mut self, key: KeyEvent, target: Option<StopTarget>) -> Option<bool> {
        self.gestures.reconcile_stop(target.as_ref());
        let target = target?;
        let cancel_composer = self.cancel_stops_work()
            && self
                .keys
                .matches(crate::keybindings::KeyAction::Cancel, key);
        let interrupt = self
            .keys
            .matches(crate::keybindings::KeyAction::Interrupt, key);
        if (!interrupt && !cancel_composer) || self.clipboard_cancel_target().is_some() {
            return None;
        }
        self.editor.break_edit_focus();
        Some(
            self.gestures
                .activate(Target::Stop(target), key, Instant::now()),
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
                for millis in [0, 100, 200] {
                    assert!(!gestures.confirm(
                        target.clone(),
                        repeat,
                        now + Duration::from_millis(millis)
                    ));
                    assert!(gestures.prompt().is_some());
                }
                assert!(gestures.confirm(target.clone(), press, now + Duration::from_millis(300)));
                assert!(!gestures.confirm(target.clone(), repeat, now + Duration::from_secs(4)));
                assert!(gestures.prompt().is_none());
            }
        }
    }

    #[test]
    fn consumed_gesture_expires_even_if_the_same_key_keeps_arriving() {
        let now = Instant::now();
        let press = key('c');
        let repeat = KeyEvent {
            repeat: true,
            ..press
        };
        for confirmed in [false, true] {
            let mut gestures = Gestures::default();
            if confirmed {
                assert!(!gestures.confirm(Target::Clear(1), press, now));
                assert!(gestures.confirm(Target::Clear(1), press, now));
            } else {
                assert!(gestures.activate(Target::Clear(1), press, now));
            }
            for key in [press, repeat] {
                let before_expiry = now + WINDOW - Duration::from_millis(1);
                assert!(!gestures.activate(Target::Exit, key, before_expiry));
                assert!(!gestures.confirm(Target::Exit, key, before_expiry));
                assert!(gestures.prompt().is_none());
            }
            assert!(!gestures.confirm(Target::Exit, repeat, now + WINDOW));
            assert!(gestures.prompt().is_none());
            assert!(!gestures.confirm(Target::Exit, press, now + WINDOW));
            assert!(gestures.prompt().is_some());
            assert!(gestures.confirm(Target::Exit, press, now + WINDOW));
        }
    }

    #[test]
    fn tree_taps_expire_and_cannot_cross_draft_baselines() {
        let now = Instant::now();
        let escape = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
        let mut gestures = Gestures::default();
        assert!(!gestures.confirm(Target::Tree(0), escape, now));
        assert!(!gestures.confirm(Target::Tree(0), escape, now + QUICK_TAP_WINDOW));
        gestures.reconcile_local(1, true, None, None);
        assert!(!gestures.confirm(Target::Tree(1), escape, now + QUICK_TAP_WINDOW));
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
