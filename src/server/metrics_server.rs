//! Metrics HTTP 服务器: 在独立端口暴露 `/health` (生产指标经 OTLP 出口).
//! 仅在 `--features monitoring` 下编译.

use std::convert::Infallible;
use std::net::SocketAddr;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pprof::protos::Message;
use tokio::net::TcpListener;

pub struct MetricsServer {
    addr: SocketAddr,
    listener: Option<tokio::net::TcpListener>,
}

impl MetricsServer {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            listener: None,
        }
    }

    /// 使用已绑定的 TcpListener 创建 MetricsServer (用于测试, 避免端口冲突).
    pub fn from_listener(listener: tokio::net::TcpListener) -> Self {
        let addr = listener.local_addr().unwrap();
        Self {
            addr,
            listener: Some(listener),
        }
    }

    pub async fn run(self) {
        let listener = match self.listener {
            Some(l) => l,
            None => match TcpListener::bind(self.addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(error = %e, addr = %self.addr, "failed to bind metrics server");
                    return;
                }
            },
        };
        tracing::info!("Metrics server listening on http://{}", self.addr);

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "metrics server accept error");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let svc = service_fn(handle_request);
                if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                    tracing::warn!(error = %e, "metrics server connection error");
                }
            });
        }
    }
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/health" => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from("OK")))
            .unwrap()),
        "/metrics" => {
            let body = crate::server::export::metrics_text();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; version=0.0.4")
                .body(Full::new(Bytes::from(body)))
                .unwrap())
        }
        "/debug/pprof/profile" => profile_response(req.uri().query()).await,
        "/" => {
            let body = r#"<!DOCTYPE html>
<html>
<head><title>AiKv Metrics</title></head>
<body>
<h1>AiKv Metrics Server</h1>
<p>Production metrics are exported via OTLP and <a href="/metrics">/metrics</a>. Use <a href="/health">/health</a> for liveness.</p>
</body>
</html>"#;
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(Full::new(Bytes::from(body)))
                .unwrap())
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap()),
    }
}

pub fn pprof_enabled() -> bool {
    std::env::var("AIKV_PPROF").ok().as_deref() == Some("1")
}

fn profile_seconds(query: Option<&str>) -> u64 {
    let mut seconds = 1u64;
    if let Some(query) = query {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if parts.next() == Some("seconds") {
                if let Ok(parsed) = parts.next().unwrap_or("1").parse::<u64>() {
                    seconds = parsed.clamp(1, 30);
                }
            }
        }
    }
    seconds
}

async fn profile_response(query: Option<&str>) -> Result<Response<Full<Bytes>>, Infallible> {
    if !pprof_enabled() {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap());
    }
    match capture_profile(profile_seconds(query)).await {
        Ok(body) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .body(Full::new(Bytes::from(body)))
            .unwrap()),
        Err(err) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from(err)))
            .unwrap()),
    }
}

async fn capture_profile(seconds: u64) -> Result<Vec<u8>, String> {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(99)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .map_err(|err| err.to_string())?;
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    let report = guard.report().build().map_err(|err| err.to_string())?;
    let profile = report.pprof().map_err(|err| err.to_string())?;
    Ok(profile.encode_to_vec())
}

#[cfg(test)]
mod tests {
    use super::pprof_enabled;

    #[test]
    fn pprof_is_off_unless_the_env_is_one() {
        let previous = std::env::var("AIKV_PPROF").ok();
        std::env::remove_var("AIKV_PPROF");
        assert!(!pprof_enabled());
        std::env::set_var("AIKV_PPROF", "1");
        assert!(pprof_enabled());
        std::env::set_var("AIKV_PPROF", "0");
        assert!(!pprof_enabled());
        match previous {
            Some(value) => std::env::set_var("AIKV_PPROF", value),
            None => std::env::remove_var("AIKV_PPROF"),
        }
    }
}
