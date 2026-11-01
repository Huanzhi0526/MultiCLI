//! MultiCLI startup boundary. Upstream startup is retained for reference but
//! never invoked: its migrations, CLI skill installation and background hosts
//! do not belong to this application's first version.
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::io::{Read, Write};
use std::time::Duration;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineInfo {
    product: &'static str,
    stage: &'static str,
    profile: String,
    data_root: String,
    upstream_commit: &'static str,
    disabled: [&'static str; 7],
}

fn selected_profile(value: Option<&str>, debug: bool) -> Result<&str, String> {
    let profile = value.unwrap_or(if debug { "dev" } else { "prod" });
    match profile {
        "dev" | "test" | "prod" => Ok(profile),
        _ => Err("MULTICLI_PROFILE must be dev, test or prod".into()),
    }
}

pub(crate) fn profile() -> Result<String, String> {
    let value = std::env::var("MULTICLI_PROFILE").ok();
    selected_profile(value.as_deref(), cfg!(debug_assertions)).map(str::to_owned)
}

fn data_root_for(base: &std::path::Path, profile: &str) -> Result<std::path::PathBuf, String> {
    if !base.is_absolute() {
        return Err("Local application data path must be absolute".into());
    }
    Ok(base
        .join("MultiCLI")
        .join(selected_profile(Some(profile), false)?))
}

pub(crate) fn data_root() -> Result<std::path::PathBuf, String> {
    let base = dirs::data_local_dir().ok_or("Local application data directory unavailable")?;
    data_root_for(&base, &profile()?)
}

#[tauri::command]
fn multicli_baseline_info() -> Result<BaselineInfo, String> {
    Ok(BaselineInfo {
        product: "MultiCLI",
        stage: "S00 — product features NOT_RUN",
        profile: profile()?,
        data_root: data_root()?.display().to_string(),
        upstream_commit: "6c46345a727e7c1b57e0dacecb4fe063d88ee8dc",
        disabled: [
            "remote",
            "relay",
            "telemetry",
            "computer-use",
            "plugins",
            "auto-update",
            "CLI-config-writes",
        ],
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PtySmoke {
    input_echo: bool,
    unicode_cwd: bool,
    resize: bool,
    exit_success: bool,
}

fn shell_smoke() -> Result<PtySmoke, String> {
    let cwd = data_root()?.join("smoke 中文 space");
    std::fs::create_dir_all(&cwd).map_err(|e| format!("create smoke directory: {e}"))?;
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("create PTY: {e}"))?;
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?;
    let shell = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    let mut command = CommandBuilder::new(shell);
    command.arg("/Q");
    command.arg("/D");
    command.cwd(&cwd);
    let mut child = pty
        .slave
        .spawn_command(command)
        .map_err(|e| format!("spawn owned shell: {e}"))?;
    drop(pty.slave);
    let result = (|| {
        let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pty.master.take_writer().map_err(|e| e.to_string())?;
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut total = 0;
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                total += count;
                if total > 256 * 1024 {
                    break;
                }
                if sender.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        pty.master
            .resize(PtySize {
                rows: 31,
                cols: 103,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let size = pty.master.get_size().map_err(|e| e.to_string())?;
        // cmd /D disables user AutoRun. chcp affects only this owned shell.
        writer
            .write_all(
                "chcp 65001\r\necho MULTICLI_S00_INPUT_OK\r\necho %CD%\r\nexit /b 0\r\n".as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        let status = loop {
            if std::time::Instant::now() >= deadline {
                return Err("owned shell PTY timed out".into());
            }
            while let Ok(chunk) = receiver.try_recv() {
                output.extend_from_slice(&chunk);
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(writer);
        // The exited child closes the ConPTY output stream. Drain any final
        // output before joining, with a bounded wait instead of read_to_end.
        while let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.extend_from_slice(&chunk);
        }
        drop(receiver);
        drop(pty.master);
        if reader_thread.is_finished() {
            let _ = reader_thread.join();
        }
        let text = String::from_utf8_lossy(&output);
        Ok(PtySmoke {
            input_echo: text.contains("MULTICLI_S00_INPUT_OK"),
            unicode_cwd: text.contains(&cwd.display().to_string()),
            resize: size.rows == 31 && size.cols == 103,
            exit_success: status.success(),
        })
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[tauri::command]
async fn multicli_shell_smoke() -> Result<PtySmoke, String> {
    tauri::async_runtime::spawn_blocking(shell_smoke)
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    let root = data_root().expect("invalid MultiCLI data profile");
    std::fs::create_dir_all(&root).expect("create isolated MultiCLI data root");
    // No upstream plugins, startup migrations, history scan, network listener,
    // credential probing, CLI installation, updater or analytics is registered.
    tauri::Builder::default()
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("MultiCLI — S00")
            .inner_size(960.0, 640.0)
            .data_directory(root.join("webview"))
            .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            multicli_baseline_info,
            multicli_shell_smoke
        ])
        .run(tauri::generate_context!())
        .expect("run MultiCLI baseline");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_root_is_outside_upstream_data() {
        let root = data_root().unwrap();
        assert!(root.is_absolute());
        assert!(root.starts_with(dirs::data_local_dir().unwrap().join("MultiCLI")));
        assert!(!root.starts_with(crate::paths::home_dir().join(".ccgui-next")));
    }

    #[test]
    fn data_profiles_are_disjoint_and_reject_paths() {
        let base = std::env::temp_dir().join("multicli-synthetic-root");
        let dev = data_root_for(&base, "dev").unwrap();
        let test = data_root_for(&base, "test").unwrap();
        let prod = data_root_for(&base, "prod").unwrap();
        assert_ne!(dev, test);
        assert_ne!(test, prod);
        assert_ne!(dev, prod);
        for bad in ["", "../.ccgui-next", "..\\.codex", "PROD", "C:\\Users"] {
            assert!(
                data_root_for(&base, bad).is_err(),
                "invalid profile accepted: {bad}"
            );
        }
        assert!(data_root_for(std::path::Path::new("relative"), "test").is_err());
        assert_eq!(selected_profile(None, true).unwrap(), "dev");
        assert_eq!(selected_profile(None, false).unwrap(), "prod");
    }

    #[cfg(windows)]
    #[test]
    fn real_windows_shell_round_trip() {
        let result = shell_smoke().expect("real Windows PTY smoke");
        assert!(result.input_echo, "shell did not execute typed echo");
        assert!(
            result.unicode_cwd,
            "shell cwd did not preserve Chinese/space path"
        );
        assert!(result.resize, "PTY geometry did not change");
        assert!(result.exit_success, "owned shell did not exit successfully");
    }
}
