use crate::log::log;

/// Open a URL or filesystem path in the user's default handler — the ONLY
/// sanctioned way anything leaves the webview (browser for http/https,
/// default app for file://). Rejects anything else (powershell schemes,
/// arbitrary executables-by-scheme, command injection).
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

/// Scheme gate + normalization: file:// -> existing backslash path, http(s) ->
/// verbatim URL. Everything else refused. No cmd/explorer argv is ever built,
/// so URL metacharacters (& | ^) are ordinary data, not injection vectors.
fn resolve_open_target(target: &str) -> Result<String, String> {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("file://") {
        let path = rest.split('#').next().unwrap_or(rest).split('?').next().unwrap_or(rest);
        let decoded = percent_decode(path.trim_start_matches('/'));
        if decoded.contains('"') {
            return Err("path contains a quote character".to_string());
        }
        // file:///C:/x -> C:\x ; file://server/share -> \\server\share (UNC).
        // Handlers get backslashes: forward slashes break naive argv parsers.
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

/// ShellExecuteW — the same call Explorer's own UI uses. Unlike explorer.exe's
/// broken argv parser (which opens Documents on input it can't parse) or
/// `cmd /c start` (quote/metacharacter hell), it takes the path as data.
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
        // SE_ERR_NOASSOC: no handler — Windows showed the Open With dialog.
        Ok(())
    } else {
        Err(format!("ShellExecute failed (code {code})"))
    }
}

#[cfg(not(windows))]
fn shell_open(target: &str) -> Result<(), String> {
    Err(format!("no default handler support on this platform: {target}"))
}

/// Minimal percent-decoding for file:// paths (%20 etc.); leaves '+' alone.
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
        // URL metacharacters are data for ShellExecuteW, not injection vectors;
        // query strings must survive untouched.
        assert_eq!(
            resolve_open_target("https://example.com/&calc|x").unwrap(),
            "https://example.com/&calc|x"
        );
        // file:// paths are normalized to backslashes for handler argv parsers.
        assert_eq!(
            resolve_open_target("file:///C:/Windows/System32/drivers/etc/hosts").unwrap(),
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        );
    }
}
