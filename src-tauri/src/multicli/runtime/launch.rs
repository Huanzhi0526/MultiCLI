//! Launch planning for the structured runtime.
//!
//! The argv is always `executable + argv[]`, never a shell string. The three
//! Windows launch chains (`.exe`, `.cmd`, `.ps1`) are S04's verified
//! `terminal::launch::plan_launch`, reused rather than re-implemented: it
//! already refuses a `.cmd` argument that cannot survive the double parse and
//! keeps user text out of the trampoline for `.ps1`.

use std::path::PathBuf;

use crate::multicli::error::AppError;
use crate::multicli::terminal::launch::{plan_launch, LaunchPaths, LaunchPlan, PinnedTarget};
use crate::multicli::terminal::model::LaunchChain;

use super::adapters::claude;
use super::adapters::codebuddy;
use super::model::RuntimeProduct;

/// Fixed product arguments. No user text ever reaches these strings.
pub fn product_args(
    product: RuntimeProduct,
    new_session_id: &str,
    resume_session: Option<&str>,
) -> Vec<String> {
    match product {
        RuntimeProduct::Codex => vec!["app-server".to_string()],
        RuntimeProduct::ClaudeCode => claude::launch_args(new_session_id, resume_session),
        RuntimeProduct::CodeBuddy => vec![
            "--acp".to_string(),
            "--acp-transport".to_string(),
            "stdio".to_string(),
        ],
    }
}

pub struct LaunchRequest<'a> {
    pub product: RuntimeProduct,
    pub executable: PathBuf,
    pub chain: LaunchChain,
    pub cwd: PathBuf,
    pub run_id: &'a str,
    pub new_session_id: &'a str,
    pub resume_session: Option<&'a str>,
}

pub struct PlannedLaunch {
    pub plan: LaunchPlan,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

pub fn plan(
    request: LaunchRequest<'_>,
    paths: &LaunchPaths,
) -> Result<PlannedLaunch, AppError> {
    let target = PinnedTarget {
        kind: crate::multicli::terminal::model::TerminalTargetKind::Installation,
        label: request
            .executable
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| request.product.agent_id().to_string()),
        product: Some(request.product.display_name().to_string()),
        agent_id: request.product.agent_id().to_string(),
        installation_id: None,
        executable: request.executable,
        chain: request.chain,
        shell_flavor: None,
        version: None,
    };
    let args = product_args(
        request.product,
        request.new_session_id,
        request.resume_session,
    );
    let plan = plan_launch(&target, &args, request.run_id, paths)?;
    let (env, env_remove) = match request.product {
        // The ACP entry point inherits a host server port and a headless
        // bundle selector; both are handled per process, never globally.
        RuntimeProduct::CodeBuddy => (
            codebuddy::ENV_SET
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            codebuddy::ENV_REMOVE.iter().map(|name| name.to_string()).collect(),
        ),
        _ => (Vec::new(), Vec::new()),
    };
    Ok(PlannedLaunch {
        plan,
        env,
        env_remove,
    })
}

/// The cwd a launched process really gets: the canonical spelling when the
/// workspace resolver produced one.
pub fn cwd_path(cwd: &crate::multicli::domain::HostPath) -> PathBuf {
    PathBuf::from(
        cwd.canonical_path
            .clone()
            .unwrap_or_else(|| cwd.display_path.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_arguments_are_fixed_and_never_carry_user_text() {
        assert_eq!(
            product_args(RuntimeProduct::Codex, "x", None),
            vec!["app-server"]
        );
        let acp = product_args(RuntimeProduct::CodeBuddy, "x", None);
        assert_eq!(acp, vec!["--acp", "--acp-transport", "stdio"]);
        let claude = product_args(RuntimeProduct::ClaudeCode, "sid", None);
        assert!(claude.contains(&"--print".to_string()) || claude.contains(&"-p".to_string()));
        assert!(!claude.iter().any(|arg| arg.contains("dangerously")));
        let resumed = product_args(RuntimeProduct::ClaudeCode, "sid", Some("old"));
        assert!(resumed.iter().any(|arg| arg == "--resume"));
    }

    #[test]
    fn codebuddy_env_is_a_process_local_delta() {
        let dir = std::env::temp_dir().join(format!(
            "multicli-runtime-launch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = LaunchPaths { root: dir.clone(), note: None };
        // A real file is needed: `plan_launch` refuses a missing executable.
        let exe = dir.join("fake.exe");
        std::fs::write(&exe, b"not really an exe").unwrap();
        let planned = plan(
            LaunchRequest {
                product: RuntimeProduct::CodeBuddy,
                executable: exe.clone(),
                chain: LaunchChain::Exe,
                cwd: dir.clone(),
                run_id: "run-1",
                new_session_id: "sid",
                resume_session: None,
            },
            &paths,
        )
        .expect("plan");
        assert!(planned
            .env_remove
            .iter()
            .any(|name| name == "SERVER__PORT"));
        assert!(planned
            .env
            .iter()
            .any(|(name, value)| name == "FORCE_HEADLESS" && value == "0"));
        assert_eq!(planned.plan.program, exe);
        assert!(planned.plan.program_args.iter().any(|arg| arg == "--acp"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
