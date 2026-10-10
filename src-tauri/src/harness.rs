use crate::exec::{AppNotifier, spawn_background_notify};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) async fn cli_version(name: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(name);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn cli_available(name: &str) -> impl std::future::Future<Output = bool> {
    let name = name.to_string();
    async move { cli_version(&name).await.is_some() }
}

/// Harness readiness, shown by the Setup wizard.
#[derive(serde::Serialize)]
pub(crate) struct BootstrapStatus {
    winapp: Option<String>,
    /// Whether winget exists, so the wizard can offer an unattended install.
    winget: bool,
}

#[tauri::command]
pub(crate) async fn bootstrap_status() -> BootstrapStatus {
    let (winapp, winget) = tokio::join!(cli_version("winapp"), cli_available("winget"));
    BootstrapStatus { winapp, winget }
}

#[tauri::command]
pub(crate) async fn install_winapp(app: tauri::AppHandle) -> Result<String, String> {
    spawn_background_notify(
        std::sync::Arc::new(AppNotifier(app)),
        winapp_install_cmd(),
        "winget install Microsoft.WinAppCli",
    )
    .await
}

/// Runs only when the user clicks Install in the Setup wizard, or explicitly
/// asks the agent to install.
fn winapp_install_cmd() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("winget");
    cmd.args([
        "install",
        "Microsoft.WinAppCli",
        "--source",
        "winget",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_detects_missing_cli() {
        assert!(!cli_available("definitely-not-a-real-cli-xyz").await);
    }

    #[test]
    fn winapp_install_is_non_interactive_winget() {
        let cmd = winapp_install_cmd();
        let argv: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(argv[0], "install");
        assert_eq!(argv[1], "Microsoft.WinAppCli");
        assert!(argv.contains(&"--accept-package-agreements".to_string()));
        assert!(argv.contains(&"--accept-source-agreements".to_string()));
        assert!(argv.contains(&"--disable-interactivity".to_string()));
    }
}
