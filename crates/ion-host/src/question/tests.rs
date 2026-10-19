use super::*;

fn question() -> Vec<Question> {
    vec![Question {
        question: "Which?".into(),
        options: vec![
            QuestionOption {
                label: "Alpha".into(),
                description: None,
            },
            QuestionOption {
                label: "Beta".into(),
                description: None,
            },
        ],
    }]
}

#[test]
fn answer_validation_preserves_custom_bytes_and_rejects_invalid_answers() {
    let questions = question();
    assert!(matches!(
        validate_answers(&questions, &[]),
        Err(QuestionError::InvalidAnswer(_))
    ));
    assert!(matches!(
        validate_answers(&questions, &[QuestionAnswer::Text { text: "  ".into() }]),
        Err(QuestionError::InvalidAnswer(_))
    ));
    assert!(matches!(
        validate_answers(&questions, &[QuestionAnswer::Option { index: usize::MAX }]),
        Err(QuestionError::InvalidAnswer(_))
    ));
    let completed = validate_answers(
        &questions,
        &[QuestionAnswer::Text {
            text: "  café 🦀  ".into(),
        }],
    )
    .unwrap();
    assert!(matches!(
        &completed[0],
        CompletedAnswer::Text { question: 1, text } if text == "  café 🦀  "
    ));
    let completed = validate_answers(&questions, &[QuestionAnswer::Option { index: 1 }]).unwrap();
    assert!(matches!(
        &completed[0],
        CompletedAnswer::Option { question: 1, option: 1, label } if label == "Beta"
    ));
}

#[test]
fn custody_has_exactly_one_setup_owner() {
    let custody = QuestionCustody::new();
    custody
        .bind_control(Uuid::now_v7(), Uuid::now_v7())
        .unwrap();
    assert!(matches!(
        custody.bind_control(Uuid::now_v7(), Uuid::now_v7()),
        Err(QuestionError::AlreadyBound)
    ));
}
