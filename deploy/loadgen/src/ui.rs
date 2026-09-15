//! 控制台静态文件: 从磁盘白名单读取, 不嵌入二进制.

use std::path::{Path, PathBuf};

pub const REQUIRED_FILES: &[&str] = &["index.html", "app.css", "app.js", "wiqun.svg"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticAsset {
    Index,
    Css,
    Js,
    Svg,
}

impl StaticAsset {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Index => "index.html",
            Self::Css => "app.css",
            Self::Js => "app.js",
            Self::Svg => "wiqun.svg",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Index => "text/html; charset=utf-8",
            Self::Css => "text/css; charset=utf-8",
            Self::Js => "text/javascript; charset=utf-8",
            Self::Svg => "image/svg+xml; charset=utf-8",
        }
    }
}

pub fn crate_web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
}

pub fn ensure_web_dir(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Err(format!("未找到页面目录: {}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("{} 不是目录", dir.display()));
    }
    for name in REQUIRED_FILES {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(format!("缺少 {name}: {}", path.display()));
        }
    }
    Ok(())
}

pub fn read_asset(dir: &Path, asset: StaticAsset) -> std::io::Result<String> {
    std::fs::read_to_string(dir.join(asset.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn crate_web_dir_is_complete() {
        ensure_web_dir(&crate_web_dir()).unwrap();
    }

    #[test]
    fn missing_dir_fails() {
        let err = ensure_web_dir(Path::new("/tmp/loadgen-web-missing-surely")).unwrap_err();
        assert!(
            err.contains("未找到页面目录") || err.contains("不是目录"),
            "{err}"
        );
    }

    #[test]
    fn empty_dir_fails_missing_file() {
        let dir = std::env::temp_dir().join(format!("loadgen-web-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = ensure_web_dir(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(err.contains("缺少"), "{err}");
    }

    #[test]
    fn missing_app_js_fails() {
        let dir = std::env::temp_dir().join(format!("loadgen-web-nojs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["index.html", "app.css", "wiqun.svg"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let err = ensure_web_dir(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(err.contains("app.js"), "{err}");
    }

    #[test]
    fn not_a_directory_fails() {
        let path = std::env::temp_dir().join(format!("loadgen-web-file-{}", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let err = ensure_web_dir(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(err.contains("不是目录"), "{err}");
    }
}
