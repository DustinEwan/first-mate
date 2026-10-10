use crate::log::log;

/// `open_path` is the only path by which anything leaves the webview. http
/// and https open the browser. `file://` opens the default app for the file
/// type.
#[tauri::command]
pub(crate) fn open_path(target: String) -> Result<(), String> {
    let result = open_path_inner(&target);
    log(&format!(
        "OPEN_PATH {target} -> {}",
        result.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str())
    ));
    result
}

fn open_path_inner(target: &str) -> Result<(), String> {
    shell_open(&resolve_open_target(target)?)
}

/// `file://` becomes an existing path with backslashes. `http(s)` is
/// returned verbatim. Every other scheme is refused. No `cmd` or `explorer`
/// argv string is ever built. URL metacharacters (`&`, `|`, `^`) therefore
/// stay data and cannot inject a command.
fn resolve_open_target(target: &str) -> Result<String, String> {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("file://") {
        let path = rest.split('#').next().unwrap_or(rest).split('?').next().unwrap_or(rest);
        let decoded = percent_decode(path.trim_start_matches('/'));
        if decoded.contains('"') {
            return Err("path contains a quote character".to_string());
        }
        // The default handler receives backslashes. A forward slash breaks
        // naive argv parsers.
        let is_unc = !t.starts_with("file:///");
        let winpath = decoded.replace('/', "\\");
        let winpath = if is_unc {
            format!("\\\\{winpath}")
        } else {
            winpath
        };
        if !std::path::Path::new(&winpath).exists() {
            return Err(format!("path does not exist: {winpath}"));
        }
        return Ok(winpath);
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Ok(t.to_string());
    }
    Err(format!("refusing to open: {t}"))
}

/// ShellExecuteW, the call Explorer's own UI uses. ShellExecuteW takes the
/// path as data. `explorer.exe` opens Documents when its argv parser rejects
/// the input, and `cmd /c start` re-parses quotes and metacharacters.
#[cfg(windows)]
fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let rc = unsafe {
        ShellExecuteW(
            None::<HWND>,
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = rc.0 as isize;
    if code > 32 {
        Ok(())
    } else if code == 31 {
        // SE_ERR_NOASSOC: Windows has shown its own Open With dialog.
        Ok(())
    } else {
        Err(format!("ShellExecute failed (code {code})"))
    }
}

#[cfg(not(windows))]
fn shell_open(target: &str) -> Result<(), String> {
    Err(format!("no default handler support on this platform: {target}"))
}

/// Percent decoding keeps '+' a literal '+'.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_path_refuses_everything_but_http_https_and_existing_files() {
        assert!(resolve_open_target("javascript:alert(1)").is_err());
        assert!(resolve_open_target("powershell.exe -c calc").is_err());
        assert!(resolve_open_target("file:///C:/nope/nothing-here-xyzzy.txt").is_err());
        // URL metacharacters are data for ShellExecuteW. A query string
        // must survive the gate untouched.
        assert_eq!(
            resolve_open_target("https://example.com/&calc|x").unwrap(),
            "https://example.com/&calc|x"
        );
        assert_eq!(
            resolve_open_target("file:///C:/Windows/System32/drivers/etc/hosts").unwrap(),
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        );
    }
}
