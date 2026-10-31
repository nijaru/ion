//! Source-bearing document fixtures for presentation policy tests.
use super::*;

pub(in crate::terminal_client) fn document(projection: TranscriptProjection) -> Conversation {
    let mut conversation = Conversation {
        scope: Some(Scope {
            path: "fixture.session".into(),
            session: uuid::Uuid::nil(),
            selection: 0,
        }),
        ..Conversation::default()
    };
    for item in projection.items {
        append(&mut conversation, item);
    }
    conversation
}

pub(in crate::terminal_client) fn append(conversation: &mut Conversation, item: TranscriptItem) {
    let source = item
        .source()
        .expect("fixture facts have source coordinates");
    assert!(
        conversation
            .positions
            .insert(source, conversation.canonical.items.len())
            .is_none()
    );
    conversation.canonical.items.push(item);
    conversation.revision += 1;
}

pub(in crate::terminal_client) fn publish(conversation: &mut Conversation) {
    let prepared = conversation.prepare_publication();
    assert!(conversation.acknowledge(prepared));
}
