//! @component aikv-command
//! Redis 命令注册表与公开兼容矩阵的一致性契约.
//!
//! 必须以 `--features cluster` 运行: 兼容矩阵含 `CLUSTER`, `READONLY`, `READWRITE`, `ASKING`
//! 等仅在 cluster feature 下注册的条目; 无该 feature 时 `all_commands()` 与文档不一致.

use std::collections::BTreeSet;

#[test]
fn compatibility_matrix_matches_command_registry() {
    let doc = include_str!("../../../docs/compatibility.md");
    let section = doc
        .split("<!-- command-list:start -->")
        .nth(1)
        .and_then(|tail| tail.split("<!-- command-list:end -->").next())
        .expect("compatibility.md must contain command-list markers");
    let documented: BTreeSet<&str> = section.split('`').skip(1).step_by(2).collect();
    let registered: BTreeSet<&str> = aikv::command::all_commands()
        .iter()
        .map(|info| info.name)
        .collect();
    assert_eq!(documented, registered);
}
