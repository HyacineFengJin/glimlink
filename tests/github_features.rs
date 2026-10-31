// SPDX-License-Identifier: MIT
//! Native collaboration command checks; no network or credentials.
use platform_github::{CollaborationAction as Action, ReviewLine};
#[test]
fn review_comments_can_be_submitted_without_a_redundant_summary() {
    let action = Action::Review {
        event: "COMMENT".into(),
        body: String::new(),
        commit_id: "a".repeat(40),
        comments: vec![ReviewLine {
            path: "src/lib.rs".into(),
            body: "Check overflow here".into(),
            line: 12,
            side: "RIGHT".into(),
            start_line: None,
            start_side: None,
        }],
    };
    assert!(action.validate().is_ok());
}
#[test]
fn issue_labels_and_assignees_have_separate_validation() {
    assert!(
        Action::CreateIssue {
            title: "test".into(),
            body: String::new(),
            labels: vec!["bug: needs triage".into()],
            assignees: vec!["valid-user".into()]
        }
        .validate()
        .is_ok()
    );
    assert!(
        Action::SetAssignees {
            assignees: vec!["../other".into()]
        }
        .validate()
        .is_err()
    );
    assert!(
        Action::SetLabels {
            labels: vec!["bug".into(), "BUG".into()]
        }
        .validate()
        .is_err()
    );
    assert!(
        Action::SetState {
            state: "merged".into(),
            pull: true
        }
        .validate()
        .is_err()
    );
}
