//! @component aikv-observability
//! 命令层 span 级别契约 (源码扫描, 无需起服务)
//!
//! `src/command/` 下 `#[instrument]` / `#[tracing::instrument]` 必须显式
//! `level = "debug"`; `kv.accept` 不得为 `tracing::info!`.

use std::path::{Path, PathBuf};

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn instrument_blocks(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = content;
    while let Some(start_rel) = rest
        .find("#[instrument")
        .or_else(|| rest.find("#[tracing::instrument"))
    {
        let start = start_rel;
        let slice = &rest[start..];
        let Some(end) = slice.find(']') else {
            break;
        };
        blocks.push(slice[..=end].to_string());
        rest = &slice[end + 1..];
    }
    blocks
}

#[test]
fn command_instruments_are_debug() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs(&root.join("src/command"), &mut files);
    let mut violations = Vec::new();
    for path in files {
        let content = std::fs::read_to_string(&path).unwrap();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        for attr in instrument_blocks(&content) {
            if !attr.contains("level = \"debug\"") && !attr.contains("level = \"trace\"") {
                violations.push(format!("{rel}: {attr}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "src/command instrument 必须显式 level = \"debug\":\n{}",
        violations.join("\n")
    );
}

#[test]
fn kv_accept_is_not_info() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let content = std::fs::read_to_string(root.join("src/server/listener.rs")).unwrap();
    let idx = content
        .find("kv.accept")
        .expect("listener.rs 必须仍有 kv.accept");
    let window = &content[idx.saturating_sub(80)..idx];
    assert!(
        window.contains("debug!"),
        "kv.accept 必须由 tracing::debug! 打出, 窗口: {window}"
    );
    assert!(
        !window.contains("info!"),
        "kv.accept 不得再是 tracing::info!"
    );
}
