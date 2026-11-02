use super::*;

fn question() -> Vec<Question> {
    vec![Question {
        question: "Which?".into(),
        header: None,
        multi_select: false,
        options: vec![
            QuestionOption {
                label: "Alpha".into(),
                description: None,
                preview: None,
            },
            QuestionOption {
                label: "Beta".into(),
                description: None,
                preview: None,
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
fn multi_select_is_mode_checked_unique_and_derived_in_request_order() {
    let mut questions = question();
    assert!(validate_answers(&questions, &[QuestionAnswer::Options { indices: vec![] }]).is_err());
    questions[0].multi_select = true;
    for answer in [
        QuestionAnswer::Option { index: 0 },
        QuestionAnswer::Options {
            indices: vec![0, 0],
        },
        QuestionAnswer::Options {
            indices: vec![usize::MAX],
        },
        QuestionAnswer::Options {
            indices: vec![0, 1, 0],
        },
    ] {
        assert!(validate_answers(&questions, &[answer]).is_err());
    }
    for (indices, expected) in [
        (
            vec![1, 0],
            serde_json::json!([
                {"option":0,"label":"Alpha"}, {"option":1,"label":"Beta"}
            ]),
        ),
        (vec![1], serde_json::json!([{"option":1,"label":"Beta"}])),
        (vec![], serde_json::json!([])),
    ] {
        let completed =
            validate_answers(&questions, &[QuestionAnswer::Options { indices }]).unwrap();
        assert_eq!(
            serde_json::to_value(completed).unwrap(),
            serde_json::json!([
                {"kind":"options","question":1,"options":expected}
            ])
        );
    }
    let completed = validate_answers(
        &questions,
        &[QuestionAnswer::Text {
            text: "  neither 🦀  ".into(),
        }],
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(completed).unwrap(),
        serde_json::json!([
            {"kind":"text","question":1,"text":"  neither 🦀  "}
        ])
    );
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
