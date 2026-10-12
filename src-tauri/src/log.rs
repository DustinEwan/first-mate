/// The debug log records each tool call with its arguments, so a release build
/// writes no log file.
#[cfg(debug_assertions)]
pub(crate) fn log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("firstmate.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(f, "[{}.{:03}] {}", ts.as_secs(), ts.subsec_millis(), msg);
    }
}

#[cfg(not(debug_assertions))]
pub(crate) fn log(_msg: &str) {}

pub(crate) fn home_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

pub(crate) fn fm_dir() -> std::path::PathBuf {
    home_dir().join(".firstmate")
}

/// Scratch directory for generated command scripts.
pub(crate) fn fm_tmp_dir() -> std::path::PathBuf {
    let d = fm_dir().join("tmp");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Spill directory for full tool output that exceeded the context budget.
pub(crate) fn fm_out_dir() -> std::path::PathBuf {
    let d = fm_dir().join("output");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// The user's Downloads folder. The leak report export lands there, because the
/// exported file holds the real values in the clear, and `crypto::is_store`
/// covers everything under `fm_dir`, where the agent reads on the user's behalf.
pub(crate) fn downloads_dir() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // The shell hands back a task-allocated buffer, so the path is copied
    // before the buffer is freed.
    let dir = unsafe {
        let text = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let dir = std::path::PathBuf::from(text.to_string().ok()?);
        CoTaskMemFree(Some(text.0 as *const core::ffi::c_void));
        dir
    };
    (!dir.as_os_str().is_empty()).then_some(dir)
}

/// A failed request body, sealed in the spill directory. `%TEMP%` keeps no
/// plaintext copy, so a support read needs `read_file`.
pub(crate) fn dump_request(turn: usize, body: &str) -> std::path::PathBuf {
    let path = fm_out_dir().join(format!("req-{}-t{turn}.json", std::process::id()));
    let _ = crate::crypto::seal(body.as_bytes())
        .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string()));
    path
}

pub(crate) fn trim_dir(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), m))
        })
        .filter(|(p, _)| p.is_file())
        .collect();
    files.sort_by_key(|(_, m)| *m);
    let drop_count = files.len().saturating_sub(keep);
    for (path, _) in files.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_dir_keeps_only_the_newest() {
        let dir = std::env::temp_dir().join(format!("fm-trim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        for i in 0..5u32 {
            let p = dir.join(format!("f{i}.txt"));
            std::fs::write(&p, "x").unwrap();
            if i < 3 {
                let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
                f.set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
        }
        trim_dir(&dir, 2);
        assert!(!dir.join("f0.txt").exists() && !dir.join("f2.txt").exists());
        assert!(dir.join("f3.txt").exists() && dir.join("f4.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
