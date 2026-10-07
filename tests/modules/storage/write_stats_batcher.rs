//! 集群写批不得在 propose 前用 exists/get_local 做键计数.

#[test]
fn submit_write_op_put_must_not_call_get_local_before_propose() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/storage/cluster_batcher.rs"
    ));
    let start = src
        .find("pub(super) async fn submit_write_op")
        .expect("submit_write_op present");
    let body = &src[start..];
    let end = body[1..]
        .find("\npub(super) async fn ")
        .map(|i| i + 1)
        .unwrap_or(body.len());
    let submit_fn = &body[..end];

    assert!(
        submit_fn.contains("if value.is_none()"),
        "DELETE short-circuit must gate get_local behind value.is_none()"
    );
    // 只计方法调用, 忽略文档注释中的 `get_local` 字样.
    let get_local_calls = submit_fn.matches(".get_local(").count();
    assert_eq!(
        get_local_calls, 1,
        "submit_write_op should call get_local exactly once (DELETE only)"
    );
}

/// write_batch 不得在 propose 前用 exists/get_local 累计 inserted/deleted.
#[test]
fn write_batch_must_consume_write_stats_not_pre_exists() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/storage/cluster_adapter.rs"
    ));
    let start = src
        .find("async fn write_batch(&self, batch: Vec<AdapterWriteOp>)")
        .expect("write_batch present");
    let body = &src[start..];
    let end = body[1..]
        .find("\n    async fn ")
        .map(|i| i + 1)
        .unwrap_or(body.len());
    let write_batch_fn = &body[..end];

    assert_eq!(
        write_batch_fn.matches("self.exists(").count(),
        0,
        "write_batch must not call exists before/after propose for counters"
    );
    assert_eq!(
        write_batch_fn.matches("get_local(").count(),
        0,
        "write_batch must not call get_local for counters"
    );
}
