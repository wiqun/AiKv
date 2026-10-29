//! 集群自适应写凑批微窗口测试
//! @component aikv-storage

use tokio::sync::{mpsc, oneshot};
use aikv::storage::cluster_batcher::{collect_batch_items, WriteBatchItem};

#[tokio::test]
async fn test_adaptive_batch_aggregation_under_concurrent_writes() {
    let (tx, mut rx) = mpsc::channel(100);

    let (ack_tx, _ack_rx) = oneshot::channel();
    let first = WriteBatchItem::Put {
        key: b"k_first".to_vec(),
        value: b"v_first".to_vec(),
        ack: ack_tx,
    };

    // 模拟并发任务在当前协程之后极短时间内投递 20 条写请求
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        for i in 0..20 {
            let (ack_tx, _ack_rx) = oneshot::channel();
            let _ = tx_clone
                .send(WriteBatchItem::Put {
                    key: format!("k_{i}").into_bytes(),
                    value: format!("v_{i}").into_bytes(),
                    ack: ack_tx,
                })
                .await;
        }
    });

    let items = collect_batch_items(&mut rx, first).await;

    // 自适应微窗口应让步等待并发客户端完成入队, 聚合出大批次 (>= 20) 而非孤立的单项
    assert!(
        items.len() >= 20,
        "自适应微窗口必须聚合并发协程投递的写入项 (目标 >= 20), 实际仅收集: {}",
        items.len()
    );
}

#[tokio::test]
async fn test_adaptive_batch_aggregation_multiple_producers() {
    let (tx, mut rx) = mpsc::channel(200);

    let (ack_tx, _ack_rx) = oneshot::channel();
    let first = WriteBatchItem::Put {
        key: b"head".to_vec(),
        value: b"head_val".to_vec(),
        ack: ack_tx,
    };

    // 启动 4 个并发生产者协程, 每个协程投递 8 条写入
    for p in 0..4 {
        let producer_tx = tx.clone();
        tokio::spawn(async move {
            for i in 0..8 {
                let (ack_tx, _ack_rx) = oneshot::channel();
                let _ = producer_tx
                    .send(WriteBatchItem::Put {
                        key: format!("k_{p}_{i}").into_bytes(),
                        value: format!("v_{p}_{i}").into_bytes(),
                        ack: ack_tx,
                    })
                    .await;
            }
        });
    }

    let items = collect_batch_items(&mut rx, first).await;

    // 1 (first) + 32 (来自 4 个生产者的写入) = 33 项
    // 自适应微窗口让步后应聚合出大部分或全部已就绪生产者项
    assert!(
        items.len() >= 20,
        "多并发生产者下自适应微窗口必须聚合大批量写入项, 实际收集: {}",
        items.len()
    );
}
