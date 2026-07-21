use super::super::PreviousSectionState;
use super::super::test_support::render_section_cases;
use super::*;

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let empty = AgentsMdState::default();
    let project_formatter = LoadedAgentsMd::from_text_for_testing("use the project formatter");
    let project_formatter = AgentsMdState::new(Some(&project_formatter));
    let old = LoadedAgentsMd::from_text_for_testing("old instructions");
    let old = AgentsMdState::new(Some(&old));
    let new = LoadedAgentsMd::from_text_for_testing("new instructions");
    let new = AgentsMdState::new(Some(&new));

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&empty)),
        (Absent, Known(&project_formatter)),
        (Known(&project_formatter), Known(&project_formatter)),
        (Known(&old), Known(&new)),
        (Known(&new), Known(&empty)),
        (Unknown, Known(&new)),
        (Unknown, Known(&empty)),
    ]));
}

#[test]
fn wsl_case_alias_does_not_replace_unchanged_instructions() {
    if !codex_utils_path::is_wsl() {
        return;
    }

    let current = AgentsMdState {
        instructions: Some(UserInstructions {
            directory: Some("/mnt/F/GH/Codex".to_string()),
            text: "use the project formatter".to_string(),
        }),
    };
    let previous = AgentsMdSnapshot {
        directory: Some("/mnt/f/gh/codex".to_string()),
        text: Some("use the project formatter".to_string()),
    };

    assert!(
        WorldStateSection::render_diff(&current, PreviousSectionState::Known(&previous)).is_none()
    );
}
