use super::tests::stream;
use super::*;
use crate::{Appearance, TerminalColors};

fn key(ch: char) -> InputEvent {
    InputEvent::Key(KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE))
}

fn all_replies() -> Vec<u8> {
    let mut bytes = b"\x1b]10;rgb:f/f/f\x07\x1b]11;rgb:0/0/0\x1b\\".to_vec();
    for index in 0..16 {
        bytes.extend_from_slice(format!("\x1b]4;{index};rgb:4/8/c\x07").as_bytes());
    }
    bytes
}

#[tokio::test]
async fn abandoned_osc_warns_without_input_and_releases_ordinary_typing_after_a_bound() {
    for oversized in [false, true] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        input.escape_grace = Duration::from_millis(10);
        input.parse(b"\x1b]11;rgb:abcd/", true);
        if oversized {
            input.parse(&vec![b'a'; 4096], true);
        }
        // No further input is needed to wake recovery and report the loss.
        let warning = tokio::time::timeout(Duration::from_millis(180), input.next())
            .await
            .expect("an abandoned reply must wake intake")
            .unwrap()
            .unwrap();
        assert_eq!(
            warning,
            InputEvent::Rejected(if oversized {
                InputError::TerminalReplyTooLarge
            } else {
                InputError::IncompleteTerminalReply
            })
        );
        tokio::time::sleep(Duration::from_millis(220)).await;
        sender.send(Ok(b"hello\r".to_vec())).await.unwrap();
        let mut expected: Vec<_> = "hello".chars().map(key).collect();
        expected.push(InputEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            Modifiers::NONE,
        )));
        for event in expected {
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(50), input.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                event
            );
        }
        assert!(input.pending.is_empty());
        assert_eq!(input.colors(), TerminalColors::default());
    }
}

#[tokio::test]
async fn newline_during_an_incomplete_reply_is_rejected_not_delivered_as_enter() {
    for recovering in [false, true] {
        for newline in [b"\r".as_slice(), b"\n", b"\r\n"] {
            let (_sender, chunks) = mpsc::channel(4);
            let mut input = stream(chunks);
            input.parse(b"\x1b]11;rgb:0/", true);
            if recovering {
                let deadline = input.replies.reply_deadline().unwrap();
                let frames = input.replies.expire(deadline);
                input.parse_reply_frames(frames, false);
                input.pending.clear();
            }
            input.parse(b"hello", true);
            input.parse(newline, false);
            assert_eq!(
                input.pending.drain(..).collect::<Vec<_>>(),
                [InputEvent::Rejected(InputError::IncompleteTerminalReply)],
                "recovering={recovering} newline={newline:?}"
            );
            // Once the rejected line ends, subsequent text and Enter are fresh
            // input. No old RGB or silently dropped text can submit an old draft.
            input.parse(b"new\r", false);
            let mut expected: Vec<_> = "new".chars().map(key).collect();
            expected.push(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                Modifiers::NONE,
            )));
            assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), expected);
        }
    }
}

#[tokio::test]
async fn reply_budget_is_distinct_from_escape_grace_and_recovery_is_absolute() {
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.parse(b"\x1b]11;rgb:0/", true);
    input.parse(&[], false); // Escape-key grace expires, not the reply budget.
    input.parse(b"0/0\x07", false);
    assert_eq!(input.pending.pop_front(), Some(InputEvent::ColorsChanged));
    assert_eq!(input.colors().background, Some([0; 3]));
    assert!(input.replies.reply_deadline().is_none());

    input.parse(b"\x1b]10;rgb:a/", true);
    let deadline = input.replies.reply_deadline().unwrap();
    // Cancelling/restarting intake and additional fragments cannot renew it.
    assert!(
        tokio::time::timeout(Duration::from_millis(5), input.next())
            .await
            .is_err()
    );
    assert_eq!(input.replies.reply_deadline(), Some(deadline));
    let frames = input.replies.expire(deadline);
    input.parse_reply_frames(frames, false);
    assert_eq!(
        input.pending.pop_front(),
        Some(InputEvent::Rejected(InputError::IncompleteTerminalReply))
    );
    let recovery = input.replies.reply_deadline().unwrap();
    input.parse(b"b/", true);
    assert_eq!(input.replies.reply_deadline(), Some(recovery));
    input.parse(b"c\x07x", false); // Sufficiently late suffix is contained.
    assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), [key('x')]);
    assert_eq!(
        input.colors().foreground,
        None,
        "abandoned RGB is never replayed or observed"
    );
    assert!(input.replies.reply_deadline().is_none());
}

#[tokio::test]
async fn query_waits_wake_for_reply_recovery_without_losing_typeahead_or_extending_query() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    sender.send(Ok(b"\x1b]11;rgb:0/".to_vec())).await.unwrap();
    let replies = async {
        tokio::time::sleep(Duration::from_millis(230)).await;
        sender.send(Ok(b"hello\x1b[?1u".to_vec())).await.unwrap();
    };
    let (result, ()) = tokio::join!(input.keyboard_support(), replies);
    assert_eq!(result.unwrap(), Some(true));
    let mut expected = vec![InputEvent::Rejected(InputError::IncompleteTerminalReply)];
    expected.extend("hello".chars().map(key));
    assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), expected);
}

#[tokio::test]
async fn late_mode_report_wakes_one_safe_fresh_capture_without_authorizing_stale_mode() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    assert!(input.begin_color_probe(true));
    assert_eq!(input.color_observations(true).await.unwrap(), None);
    assert!(
        !input.colors_need_refresh(),
        "absence must not cause a draw/query loop"
    );
    // Safe explicit refreshes while the old request is outstanding may sample
    // RGB, but cannot send another DECRQM or keep the refresh flag set.
    assert!(!input.begin_color_probe(true));
    sender.send(Ok(all_replies())).await.unwrap();
    input.color_observations(false).await.unwrap();
    input.pending.clear();
    assert!(!input.colors_need_refresh());
    sender
        .send(Ok(b"\x1b[?2031;2$y\x1b[?2031;2$ya".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        InputEvent::ColorsChanged
    );
    assert_eq!(
        input.replies.appearance_mode, None,
        "late reset is never authority"
    );
    assert!(input.colors_need_refresh());
    assert_eq!(input.next().await.unwrap().unwrap(), key('a'));
    assert!(
        input.begin_color_probe(true),
        "the next explicit normal-UI refresh can recapture"
    );
    sender.send(Ok(b"\x1b[?2031;1$y".to_vec())).await.unwrap();
    assert_eq!(input.color_observations(true).await.unwrap(), Some(1));
    assert!(!input.colors_need_refresh());
    assert!(
        !input.begin_color_probe(true),
        "a completed capture is not queried on every refresh"
    );
}

#[tokio::test]
async fn cancelled_mode_capture_can_drain_in_ordinary_or_secret_intake_then_retry_safely() {
    for quarantined in [false, true] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        assert!(input.begin_color_probe(true));
        assert!(
            tokio::time::timeout(Duration::from_millis(5), input.color_observations(true))
                .await
                .is_err()
        );
        sender.send(Ok(b"\x1b[?2031;2$y".to_vec())).await.unwrap();
        if quarantined {
            input.discard_for_credentials().unwrap();
            assert!(input.pending.is_empty());
        } else {
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(50), input.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                InputEvent::ColorsChanged
            );
        }
        assert_eq!(
            input.replies.appearance_mode, None,
            "cancelled capture cannot accept late authority"
        );
        assert!(input.colors_need_refresh());
        assert!(input.begin_color_probe(true));
        let mut fresh = all_replies();
        fresh.extend_from_slice(b"\x1b[?2031;1$y");
        sender.send(Ok(fresh)).await.unwrap();
        assert_eq!(input.color_observations(true).await.unwrap(), Some(1));
        assert!(!input.colors_need_refresh());
    }
}

#[tokio::test]
async fn unsupported_and_malformed_mode_answers_do_not_rearm_capture() {
    for (report, late) in [
        (b"\x1b[?2031;0$y".as_slice(), false),
        (b"\x1b[?2031;9$y".as_slice(), false),
        (b"\x1b[?2031;2$x".as_slice(), false),
        (b"\x1b[?2031;0$y".as_slice(), true),
        (b"\x1b[?2031;9$y".as_slice(), true),
        (b"\x1b[?2031;2$x".as_slice(), true),
    ] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        assert!(input.begin_color_probe(true));
        if late {
            input.color_observations(true).await.unwrap();
            input.parse(report, false);
        } else {
            sender.send(Ok(report.to_vec())).await.unwrap();
            input.color_observations(true).await.unwrap();
        }
        assert!(!input.colors_need_refresh());
        assert!(
            !input.begin_color_probe(true),
            "{report:?} must settle capture without a retry loop"
        );
        input.parse(report, false);
        input.parse(b"x", false);
        assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), [key('x')]);
    }
}

#[tokio::test]
async fn alt_closing_bracket_never_captures_subsequent_typing_or_stop() {
    let alt_bracket = InputEvent::Key(KeyEvent::new(KeyCode::Char(']'), Modifiers::ALT));
    let stop = InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL));
    for expire in [false, true] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        input.escape_grace = Duration::from_millis(10);
        input.parse(b"\x1b]", true);
        if expire {
            // Exercise the ordinary intake deadline, not only a parser flush.
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(100), input.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                alt_bracket,
            );
        }
        sender.send(Ok(b"hello\x03z".to_vec())).await.unwrap();
        let mut expected = Vec::new();
        if !expire {
            expected.push(alt_bracket.clone());
        }
        expected.extend("hello".chars().map(key));
        expected.extend([stop.clone(), key('z')]);
        for event in expected {
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(100), input.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                event
            );
        }
        assert_eq!(input.colors(), TerminalColors::default());
    }
}

#[tokio::test]
async fn truncated_osc_releases_stop_cancel_and_new_escape_sequences() {
    let esc = InputEvent::Key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE));
    let stop = InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL));
    for expire in [false, true] {
        for (bytes, event) in [
            (b"\x03".as_slice(), stop.clone()),
            (
                b"\x18",
                InputEvent::Key(KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL)),
            ),
            (
                b"\x1a",
                InputEvent::Key(KeyEvent::new(KeyCode::Char('z'), Modifiers::CONTROL)),
            ),
            (b"\x1b", esc.clone()),
            (b"\x1b[27u", esc.clone()),
            (
                b"\x1b[A",
                InputEvent::Key(KeyEvent::new(KeyCode::Up, Modifiers::NONE)),
            ),
        ] {
            let (_sender, chunks) = mpsc::channel(4);
            let mut input = stream(chunks);
            input.parse(b"\x1b]11;rgb:abcd/1234", true);
            if expire {
                input.parse(&[], false);
            }
            input.parse(bytes, true);
            input.parse(&[], false);
            input.parse(b"ok", false);
            assert_eq!(
                input.pending.drain(..).collect::<Vec<_>>(),
                [event, key('o'), key('k')],
                "expire={expire} boundary={bytes:?}"
            );
            assert_eq!(input.colors(), TerminalColors::default());
        }
    }
}

#[tokio::test]
async fn osc_escape_disambiguation_preserves_stop_and_contains_a_late_st() {
    let esc = InputEvent::Key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE));
    let stop = InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL));
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.parse(b"\x1b]11;rgb:0/0\x1b\x03z", false);
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [esc.clone(), stop, key('z')]
    );
    input.parse(b"\x1b]11;rgb:0/0/0\x1b", true);
    input.parse(&[], false);
    // Escape is actionable at its deadline, but a delayed ST backslash is
    // still protocol, not a draft character. Repeated flushes emit no repeats.
    input.parse(&[], false);
    input.parse(b"\\k", false);
    assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), [esc, key('k')]);
    assert_eq!(input.colors(), TerminalColors::default());
}

#[tokio::test]
async fn paste_boundary_ends_a_truncated_report_without_filtering_literal_payload() {
    for quarantined in [false, true] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        input.parse(b"\x1b]11;rgb:abcd/", true);
        let start = b"\x1b[200~\x03\x1b]11;rgb:f/f/f\x07";
        if quarantined {
            sender.send(Ok(start.to_vec())).await.unwrap();
            input.discard_for_credentials().unwrap();
        } else {
            input.parse(start, true);
        }
        input.parse(b"\x1b[201~ok", false);
        let mut expected = Vec::new();
        if !quarantined {
            expected.push(InputEvent::Paste("\x03\x1b]11;rgb:f/f/f\x07".into()));
        }
        expected.extend([key('o'), key('k')]);
        assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), expected);
        assert_eq!(input.colors(), TerminalColors::default());
    }
}

#[tokio::test]
async fn private_csi_custody_is_limited_to_owned_replies() {
    // An unknown private sequence is not Ion's reply to discard. Preserve its
    // exact bytes and parser boundaries, including after the Escape deadline.
    let mut replies = TerminalReplyFilter::default();
    let frames = replies.feed(b"\x1b[?123;", true, false);
    assert!(matches!(frames.as_slice(), [query::KeyFrame::Bytes(bytes)] if bytes == b"\x1b[?123;"));
    let frames = replies.feed(b"9nz", true, false);
    assert!(matches!(frames.as_slice(), [query::KeyFrame::Bytes(bytes)] if bytes == b"9nz"));
    let frames = replies.feed(b"\x1b[?123;9n", true, false);
    assert!(
        matches!(frames.as_slice(), [query::KeyFrame::Bytes(bytes)] if bytes == b"\x1b[?123;9n")
    );

    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.parse(b"\x1b[?997;", false);
    input.parse(b"\x03z", false);
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [
            InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
            key('z')
        ]
    );
    // Unknown private CSI is key disambiguation, not retained protocol custody
    // across credential quarantine, whether decoded or still read ahead.
    for read_ahead in [false, true] {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        if read_ahead {
            sender.send(Ok(b"\x1b[?123;".to_vec())).await.unwrap();
        } else {
            input.parse(b"\x1b[?123;", true);
        }
        input.discard_for_credentials().unwrap();
        input.parse(b"k", false);
        assert_eq!(input.pending.drain(..).collect::<Vec<_>>(), [key('k')]);
    }
}

#[tokio::test]
async fn full_probe_settles_early_with_typeahead_and_protocol_event_order() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    assert!(input.begin_color_probe(true));
    let mut bytes = b"a\x1b[?2031;2$y".to_vec();
    bytes.extend(all_replies());
    bytes.extend_from_slice(b"b");
    sender.send(Ok(bytes)).await.unwrap();
    // Keep the channel open: completion cannot depend on EOF or the timeout.
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(50), input.color_observations(true))
            .await
            .unwrap()
            .unwrap(),
        Some(2)
    );
    assert!(!input.colors_need_refresh());
    assert_eq!(input.colors().foreground, Some([255; 3]));
    assert_eq!(input.colors().background, Some([0; 3]));
    assert_eq!(input.colors().palette, [Some([68, 136, 204]); 16]);
    for expected in [key('a'), InputEvent::ColorsChanged, key('b')] {
        assert_eq!(input.next().await.unwrap().unwrap(), expected);
    }
    assert!(input.pending.is_empty());
}

#[tokio::test]
async fn absent_partial_and_late_colors_are_normal_and_do_not_flood_events() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.begin_color_probe(false);
    // Partial colors plus an unrelated DA1 must not use the old keyboard fence.
    sender
        .send(Ok(b"x\x1b]11;rgb:1/2/3\x07\x1b[?1;2c".to_vec()))
        .await
        .unwrap();
    assert_eq!(input.color_observations(false).await.unwrap(), None);
    assert_eq!(input.colors().background, Some([17, 34, 51]));
    assert!(!input.colors_need_refresh());
    assert_eq!(input.next().await.unwrap().unwrap(), key('x'));
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        InputEvent::ColorsChanged
    );
    // A late, split reply updates ordinary intake, not a future query only.
    sender
        .send(Ok(b"\x1b]10;rgb:abcd/12/8".to_vec()))
        .await
        .unwrap();
    sender.send(Ok(b"000\x1b\\z".to_vec())).await.unwrap();
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        InputEvent::ColorsChanged
    );
    assert_eq!(input.colors().foreground, Some([171, 18, 128]));
    assert_eq!(input.next().await.unwrap().unwrap(), key('z'));
    input.parse(b"\x1b]10;rgb:abcd/12/8000\x07", false);
    assert!(input.pending.is_empty());
    input.begin_color_probe(false);
    // No replies is also successful, retaining still-current observations.
    input.color_observations(false).await.unwrap();
    assert!(!input.colors_need_refresh());
    assert_eq!(input.colors().background, Some([17, 34, 51]));
}

#[tokio::test]
async fn color_query_errors_are_errors_and_cancellation_preserves_intake() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.begin_color_probe(false);
    sender
        .send(Err(io::Error::other("read fault")))
        .await
        .unwrap();
    assert_eq!(
        input
            .color_observations(false)
            .await
            .unwrap_err()
            .to_string(),
        "read fault"
    );
    assert!(input.colors_need_refresh());
    sender.send(Ok(b"a\x1b]11;rgb:0/0".to_vec())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), input.color_observations(false))
            .await
            .is_err()
    );
    assert!(input.colors_need_refresh());
    sender.send(Ok(b"/0\x07b".to_vec())).await.unwrap();
    for expected in [key('a'), InputEvent::ColorsChanged, key('b')] {
        assert_eq!(input.next().await.unwrap().unwrap(), expected);
    }
    input.begin_color_probe(false);
    drop(sender);
    assert_eq!(
        input.color_observations(false).await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn absolute_color_and_mode_budget_cannot_be_extended_by_ready_input() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    assert!(input.begin_color_probe(true));
    let mut probe = Box::pin(input.color_observations(true));
    let mut context = Context::from_waker(Waker::noop());
    assert!(probe.as_mut().poll(&mut context).is_pending());
    std::thread::sleep(Duration::from_millis(120));
    let mut bytes = all_replies();
    bytes.extend_from_slice(b"\x1b[?2031;2$y");
    sender.try_send(Ok(bytes)).unwrap();
    assert!(matches!(
        probe.as_mut().poll(&mut context),
        Poll::Ready(Ok(None))
    ));
    drop(probe);
    assert_eq!(
        input.replies.appearance_mode, None,
        "a late DECRQM cannot authorize mode changes"
    );
    // A receive already polled before expiry can transfer custody when polling
    // resumes. It is parsed as late ordinary input, never extending the probe.
    assert_eq!(input.colors().foreground, Some([255; 3]));
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        InputEvent::ColorsChanged
    );
    assert_eq!(input.colors().background, Some([0; 3]));
}

#[tokio::test]
async fn appearance_invalidates_rgb_and_refresh_is_explicit_not_intake_driven() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.parse(&all_replies(), true);
    input.pending.clear();
    sender.send(Ok(b"a\x1b[?997;2nb".to_vec())).await.unwrap();
    for expected in [key('a'), InputEvent::ColorsChanged, key('b')] {
        assert_eq!(input.next().await.unwrap().unwrap(), expected);
    }
    assert_eq!(
        input.colors(),
        TerminalColors {
            appearance: Some(Appearance::Light),
            ..TerminalColors::default()
        }
    );
    assert!(input.colors_need_refresh());
    // A reply from an already-issued probe cannot establish fresh RGB after
    // the switch. Consume it, preserving following user input.
    input.parse(b"\x1b]11;rgb:0/0/0\x07c\x1b[?997;2n", false);
    assert_eq!(input.pending.pop_front(), Some(key('c')));
    assert!(input.pending.is_empty());
    assert_eq!(input.colors().background, None);
    input.begin_color_probe(false);
    sender
        .send(Ok(b"\x1b]11;rgb:f/f/f\x07".to_vec()))
        .await
        .unwrap();
    input.color_observations(false).await.unwrap();
    assert_eq!(input.colors().background, Some([255; 3]));
    assert!(!input.colors_need_refresh());
    input.pending.clear();
    // Even the same appearance hint can mean the raw palette changed.
    input.parse(b"\x1b[?997;2n", false);
    assert_eq!(input.pending.pop_front(), Some(InputEvent::ColorsChanged));
    assert!(input.colors_need_refresh());
    assert_eq!(input.colors().background, None);
}

#[tokio::test]
async fn notification_during_probe_defers_a_new_probe_and_drops_old_samples() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.begin_color_probe(false);
    let mut bytes = all_replies();
    bytes.extend_from_slice(b"\x1b[?997;1n");
    bytes.extend(all_replies());
    sender.send(Ok(bytes)).await.unwrap();
    input.color_observations(false).await.unwrap();
    assert!(input.colors_need_refresh());
    assert_eq!(
        input.colors(),
        TerminalColors {
            appearance: Some(Appearance::Dark),
            ..TerminalColors::default()
        }
    );
}

#[tokio::test]
async fn literal_paste_precedes_color_filter_and_removed_replies_cannot_bridge_keys() {
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    let literal = "\x1b]10;rgb:f/f/f\x07\x1b[?997;2n\x1b[?2031;2$y";
    let bytes = format!("\x1b[200~{literal}\x1b[201~");
    for byte in bytes.bytes() {
        input.parse(&[byte], true);
    }
    assert_eq!(
        input.pending.pop_front(),
        Some(InputEvent::Paste(literal.into()))
    );
    assert_eq!(input.colors(), TerminalColors::default());
    input.parse(b"\x1b[20\x1b]11;rgb:0/0/0\x07", true);
    input.parse(b"0~suffix\x1b[201~", false);
    assert!(
        input
            .pending
            .iter()
            .all(|event| !matches!(event, InputEvent::Paste(_)))
    );
    assert!(!input.paste.is_open());
}

#[tokio::test]
async fn fragmented_notifications_and_mode_reports_never_become_keys() {
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    assert!(input.begin_color_probe(true));
    for byte in b"\x1b[?2031;2$y\x1b[?997;1n" {
        input.parse(&[*byte], true);
    }
    assert_eq!(input.replies.appearance_mode, Some(2));
    assert_eq!(input.colors().appearance, Some(Appearance::Dark));
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [InputEvent::ColorsChanged]
    );
    // A partial reply uses its own budget, not the shorter Escape-key grace.
    // Its valid continuation updates observations without ever becoming keys.
    input.parse(b"\x1b[?997;", false);
    input.parse(b"2nz", false);
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [InputEvent::ColorsChanged, key('z')]
    );
    assert_eq!(input.colors().appearance, Some(Appearance::Light));
}

#[tokio::test]
async fn credential_quarantine_keeps_protocol_custody_and_observations() {
    let (sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    input.parse(b"\x1b]11;rgb:0/0/0\x07\x1b]10;rgb:f/", true);
    sender
        .send(Ok(b"f/f\x07SECRET\x1b[?997;2n".to_vec()))
        .await
        .unwrap();
    input.discard_for_credentials().unwrap();
    assert!(input.pending.is_empty());
    assert!(input.colors_need_refresh());
    assert_eq!(input.colors().appearance, Some(Appearance::Light));
    assert_eq!(input.colors().foreground, None);
    assert_eq!(input.colors().background, None);
    // Literal paste during quarantine cannot be mistaken for a notification.
    sender
        .send(Ok(b"\x1b[200~\x1b[?997;1n\x1b[201~".to_vec()))
        .await
        .unwrap();
    input.discard_for_credentials().unwrap();
    assert_eq!(input.colors().appearance, Some(Appearance::Light));
    input.parse(b"k", false);
    assert_eq!(input.pending.pop_front(), Some(key('k')));
}

#[tokio::test]
async fn osc_framing_is_bounded_and_malformed_payloads_are_not_replayed() {
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    for bytes in [
        b"\x1b]11;rgb:nope\x07".as_slice(),
        b"\x1b]52;unknown\x1b\\",
        b"\x1b[?997;3n",
    ] {
        input.parse(bytes, false);
        assert!(input.pending.is_empty(), "{bytes:?}: {:?}", input.pending);
    }
    // ESC ends the old OSC and begins a new CSI key sequence. The following
    // BEL is outside OSC too (Ctrl-G); neither key may be swallowed as payload.
    input.parse(b"\x1b]11;rgb:0/0/0\x1b[13u\x07", false);
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [
            InputEvent::Key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
            InputEvent::Key(KeyEvent::new(KeyCode::Char('g'), Modifiers::CONTROL)),
        ]
    );
    assert_eq!(input.colors(), TerminalColors::default());
    input.parse(b"\x1b]11;rgb:", true);
    input.parse(&vec![b'a'; 4096], true);
    input.parse(b"\x1b\\z", false);
    assert_eq!(
        input.pending.pop_front(),
        Some(InputEvent::Rejected(InputError::TerminalReplyTooLarge))
    );
    assert_eq!(input.pending.pop_front(), Some(key('z')));
    assert!(input.pending.is_empty());
    input.parse(b"\x1b]11;rgb:0/0", true);
    input.parse(&[], false);
    input.parse(b"/0\x07k", false);
    assert_eq!(input.pending.pop_front(), Some(InputEvent::ColorsChanged));
    assert_eq!(input.pending.pop_front(), Some(key('k')));
    assert_eq!(input.colors().background, Some([0; 3]));
    // A user Escape adjacent to an OSC is still a separate Escape event.
    input.parse(b"\x1b\x1b]10;rgb:f/f/f\x07", false);
    assert_eq!(
        input.pending.pop_front(),
        Some(InputEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            Modifiers::NONE
        )))
    );
    assert_eq!(input.pending.pop_front(), Some(InputEvent::ColorsChanged));
}

#[tokio::test]
async fn expired_or_cancelled_mode_capture_cannot_authorize_a_new_mode_change() {
    let (_sender, chunks) = mpsc::channel(4);
    let mut input = stream(chunks);
    assert!(input.begin_color_probe(true));
    input.color_observations(true).await.unwrap();
    assert!(
        !input.begin_color_probe(true),
        "an old DECRQM is still outstanding"
    );
    input.parse(b"\x1b[?2031;2$y", false);
    assert_eq!(input.replies.appearance_mode, None);
    assert!(input.begin_color_probe(true));
    input.parse(b"\x1b[?2031;1$y", false);
    assert_eq!(input.replies.appearance_mode, Some(1));
    assert!(
        !input.begin_color_probe(true),
        "a fresh completed capture is settled"
    );
    input.reacquire_colors(); // A new physical acquisition captures again.
    assert!(input.begin_color_probe(true));
    assert!(
        !input.begin_color_probe(true),
        "abandon a cancelled capture too"
    );
    input.parse(b"\x1b[?2031;2$y", false);
    assert_eq!(input.replies.appearance_mode, None);
    assert_eq!(
        input.pending.drain(..).collect::<Vec<_>>(),
        [InputEvent::ColorsChanged]
    );
}
