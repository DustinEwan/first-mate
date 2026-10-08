/// Append a timestamped line to the local log file.
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

pub(crate) fn home_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn fm_dir() -> std::path::PathBuf {
    home_dir().join(".firstmate")
}

/// Scratch directory for generated command scripts (created on demand).
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

/// LRU hygiene: keep only the newest `keep` files in a scratch directory.
/// Run at startup so a previous session's orphans cannot pile up forever.
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
