//! 集群 slot 路由 key. 取 `command_keys` 的第一个 key, 与注册表的 key 位置一致.
//! OBJECT 的 key 在子命令之后, ZINTER / EVAL 的 key 在个数参数之后.

use bytes::Bytes;

/// EVAL/EVALSHA 的 slot 路由 key: `EVAL script numkeys key ...`
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

    /// BITFIELD_RO 的 key 在第一个参数, 副本按这个槽位本地读.
    #[test]
    fn bitfield_ro_routes_by_key() {
        let args = [
            Bytes::from_static(b"bits"),
            Bytes::from_static(b"GET"),
            Bytes::from_static(b"u8"),
            Bytes::from_static(b"0"),
        ];
        assert_eq!(
            cluster_routing_key("BITFIELD_RO", &args),
            Some(&b"bits"[..])
        );
    }

    /// OBJECT 的 key 不是第一个参数. 按子命令路由会把只读请求送到错误节点.
    #[test]
    fn object_routes_by_key_not_subcommand() {
        let args = [
            Bytes::from_static(b"ENCODING"),
            Bytes::from_static(b"thekey"),
        ];
        assert_eq!(cluster_routing_key("OBJECT", &args), Some(&b"thekey"[..]));
    }

    /// ZINTER 第一个参数是 key 个数, 路由 key 是其后的第一个 key.
    #[test]
    fn zinter_routes_by_first_key() {
        let args = [
            Bytes::from_static(b"2"),
            Bytes::from_static(b"z1"),
            Bytes::from_static(b"z2"),
        ];
        assert_eq!(cluster_routing_key("ZINTER", &args), Some(&b"z1"[..]));
    }
}
