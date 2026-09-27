use super::*;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn picker_cwd_restores_wsl_spelling_before_resume_or_fork() -> color_eyre::Result<()> {
    let current_dir = std::env::current_dir()?;
    if !codex_utils_path::is_wsl()
        || !codex_utils_path::wsl_paths_match_ignoring_case(&current_dir, &current_dir)
    {
        return Ok(());
    }
    let temp_dir = tempfile::tempdir_in(&current_dir)?;
    let session_cwd = temp_dir.path().join("MixedCaseProject");
    let other_cwd = temp_dir.path().join("OtherProject");
    let codex_home = temp_dir.path().join("home");
    std::fs::create_dir(&session_cwd)?;
    std::fs::create_dir(&other_cwd)?;
    std::fs::create_dir(&codex_home)?;
    let saved_cwd = PathBuf::from(session_cwd.to_string_lossy().to_ascii_lowercase());
    assert_ne!(saved_cwd, session_cwd);

    // The picker supplies database spelling directly, so no thread/read occurs.
    for (mode, launch_cwd, explicit_cwd, expected_cwd, remote_environment) in [
        (None, &session_cwd, None, &session_cwd, false),
        (Some("session"), &other_cwd, None, &session_cwd, false),
        (
            Some("session"),
            &other_cwd,
            Some(other_cwd.as_path()),
            &other_cwd,
            false,
        ),
        // A coincidentally matching local path cannot supply remote path spelling.
        (Some("session"), &other_cwd, None, &saved_cwd, true),
    ] {
        std::fs::write(
            codex_home.join("config.toml"),
            mode.map(|mode| format!("[tui]\nresume_cwd = \"{mode}\"\n"))
                .unwrap_or_default(),
        )?;
        let config = ConfigBuilder::default()
            .codex_home(codex_home.clone())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .harness_overrides(ConfigOverrides {
                cwd: Some(launch_cwd.clone()),
                ..Default::default()
            })
            .build()
            .await?;

        for action in [CwdPromptAction::Resume, CwdPromptAction::Fork] {
            let target = resume_picker::SessionTarget {
                path: None,
                thread_id: ThreadId::new(),
                cwd: Some(saved_cwd.clone()),
                history_mode: None,
            };
            let selection = match action {
                CwdPromptAction::Resume => resume_picker::SessionSelection::Resume(target),
                CwdPromptAction::Fork => resume_picker::SessionSelection::Fork(target),
            };
            let mut tui = tui::test_support::make_test_tui()?;
            let outcome = resolve_startup_resume_or_fork_cwd(
                &mut tui,
                &config,
                /*app_server*/ None,
                &selection,
                explicit_cwd,
                /*uses_remote_workspace*/ false,
                remote_environment,
            )
            .await?;
            assert_eq!(
                outcome,
                ResolveCwdOutcome::Continue(Some(expected_cwd.clone())),
            );
        }
    }
    Ok(())
}
