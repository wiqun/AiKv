//! 集群 slot 路由 key 提取 (EVAL/EVALSHA 首 key 等).

use bytes::Bytes;

/// 集群 slot 路由 key. 取命令表里的第一把 key.
pub fn cluster_routing_key<'a>(cmd: &str, args: &'a [Bytes]) -> Option<&'a [u8]> {
    crate::command::command_keys(cmd, args).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// 验证 EVAL 命令提取第一个声明的 Key 用于 Slot 路由
    #[test]
    fn eval_routing_key_uses_first_declared_key() {
        let args = [
            Bytes::from_static(b"return 1"),
            Bytes::from_static(b"1"),
            Bytes::from_static(b"mykey:{0}"),
            Bytes::from_static(b"0"),
        ];
        assert_eq!(cluster_routing_key("EVAL", &args), Some(&b"mykey:{0}"[..]));
    }

    /// 验证 普通 GET 命令提取第一个参数作为路由 Key
    #[test]
    fn get_routing_key_uses_first_arg() {
        let args = [Bytes::from_static(b"userkey")];
        assert_eq!(cluster_routing_key("GET", &args), Some(&b"userkey"[..]));
    }

    #[test]
    fn object_routes_on_the_key_not_the_subcommand() {
        let args = [
            Bytes::from_static(b"ENCODING"),
            Bytes::from_static(b"mykey"),
        ];
        assert_eq!(cluster_routing_key("OBJECT", &args), Some(&b"mykey"[..]));
        assert_eq!(
            cluster_routing_key(
                "SWAPDB",
                &[Bytes::from_static(b"1"), Bytes::from_static(b"2")]
            ),
            None
        );
    }
}
