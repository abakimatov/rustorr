//! `/metrics`: Prometheus text exposition, behind the management API's
//! authentication. Not part of MatriX.145; unknown to its clients.

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, Response, header},
    middleware::Next,
    response::IntoResponse,
};
use rustorr_lifecycle::{CacheCommand, TorrentCommand, TorrentReply};

use crate::app::{AppState, management_authorized, unauthorized};

/// Upper bounds of the request duration histogram, in seconds.
const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// What the HTTP layer counts; the rest is read when `/metrics` is asked.
pub(crate) struct HttpMetrics {
    started: SystemTime,
    requests: Mutex<BTreeMap<(&'static str, u16), u64>>,
    buckets: [AtomicU64; BUCKETS.len()],
    count: AtomicU64,
    sum_micros: AtomicU64,
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            started: SystemTime::now(),
            requests: Mutex::default(),
            buckets: Default::default(),
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }
}

impl HttpMetrics {
    fn record(&self, method: &'static str, status: u16, elapsed: Duration) {
        *self
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((method, status))
            .or_default() += 1;
        let seconds = elapsed.as_secs_f64();
        for (bound, bucket) in BUCKETS.iter().zip(&self.buckets) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

/// Few label values, whatever clients send.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::OPTIONS => "OPTIONS",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        _ => "other",
    }
}

/// Counts every request and the time to its response head (a stream's
/// body is not waited for).
pub(crate) async fn middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let started = Instant::now();
    let method = method_label(request.method());
    let response = next.run(request).await;
    state
        .metrics
        .record(method, response.status().as_u16(), started.elapsed());
    response
}

fn gauge(text: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(
        text,
        "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}"
    );
}

/// `VmRSS` and the number of open descriptors, where `/proc` has them.
fn process_figures() -> (Option<u64>, Option<usize>) {
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| {
                    value
                        .trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
                .map(|kib| kib * 1024)
        });
    let fds = std::fs::read_dir("/proc/self/fd").ok().map(Iterator::count);
    (rss, fds)
}

pub(crate) async fn handler(State(state): State<AppState>, headers: HeaderMap) -> Response<Body> {
    if !management_authorized(&state, &headers) {
        return unauthorized();
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# HELP rustorr_build_info Rustorr's version and the API it keeps.\n# TYPE rustorr_build_info gauge\nrustorr_build_info{{version=\"{}\",api=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION"),
        state.info.version
    );
    let started = state
        .metrics
        .started
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64());
    gauge(
        &mut text,
        "process_start_time_seconds",
        "Start time of the process since the Unix epoch.",
        started,
    );
    let (rss, fds) = process_figures();
    if let Some(rss) = rss {
        gauge(
            &mut text,
            "process_resident_memory_bytes",
            "Resident memory size.",
            rss,
        );
    }
    if let Some(fds) = fds {
        gauge(&mut text, "process_open_fds", "Open file descriptors.", fds);
    }

    if let Ok(TorrentReply::List(torrents)) = state.core.torrents(TorrentCommand::List).await {
        // `stat` 5 is a catalog entry that is not loaded.
        let live: Vec<_> = torrents
            .iter()
            .filter(|torrent| torrent.stat != 5)
            .collect();
        let _ = writeln!(
            text,
            "# HELP rustorr_torrents Torrents in the list, loaded or only saved.\n# TYPE rustorr_torrents gauge\nrustorr_torrents{{state=\"loaded\"}} {}\nrustorr_torrents{{state=\"saved\"}} {}",
            live.len(),
            torrents.len() - live.len()
        );
        let sum = |value: fn(&&rustorr_lifecycle::TorrentView) -> u64| {
            live.iter().map(value).sum::<u64>()
        };
        let _ = writeln!(
            text,
            "# HELP rustorr_peers Peers of the loaded torrents.\n# TYPE rustorr_peers gauge\nrustorr_peers{{state=\"active\"}} {}\nrustorr_peers{{state=\"known\"}} {}",
            sum(|torrent| torrent.active_peers.unwrap_or(0) as u64),
            sum(|torrent| torrent.total_peers.unwrap_or(0) as u64)
        );
        let _ = writeln!(
            text,
            "# HELP rustorr_transfer_bytes_per_second BitTorrent transfer rate of the loaded torrents.\n# TYPE rustorr_transfer_bytes_per_second gauge\nrustorr_transfer_bytes_per_second{{direction=\"download\"}} {}\nrustorr_transfer_bytes_per_second{{direction=\"upload\"}} {}",
            sum(|torrent| torrent.download_speed.unwrap_or(0)),
            sum(|torrent| torrent.upload_speed.unwrap_or(0))
        );
    }
    if let Ok(cache) = state.core.cache(CacheCommand::List).await {
        let _ = writeln!(
            text,
            "# HELP rustorr_cache_bytes The cache's soft limit and what it holds.\n# TYPE rustorr_cache_bytes gauge\nrustorr_cache_bytes{{kind=\"capacity\"}} {}\nrustorr_cache_bytes{{kind=\"stored\"}} {}",
            cache.capacity, cache.filled
        );
        gauge(
            &mut text,
            "rustorr_active_readers",
            "Open playback readers (streams being served).",
            cache
                .snapshots
                .iter()
                .map(|snapshot| snapshot.active_readers.len())
                .sum::<usize>(),
        );
    }
    if let Some(gstreamer) = &state.gstreamer {
        gauge(
            &mut text,
            "rustorr_hls_tasks",
            "GStreamer HLS tasks.",
            gstreamer.task_count(),
        );
    }

    let _ = writeln!(
        text,
        "# HELP rustorr_http_requests_total HTTP requests by method and status.\n# TYPE rustorr_http_requests_total counter"
    );
    for ((method, status), count) in state
        .metrics
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
    {
        let _ = writeln!(
            text,
            "rustorr_http_requests_total{{method=\"{method}\",code=\"{status}\"}} {count}"
        );
    }
    let _ = writeln!(
        text,
        "# HELP rustorr_http_response_head_seconds Time until a response's head, streams included.\n# TYPE rustorr_http_response_head_seconds histogram"
    );
    for (bound, bucket) in BUCKETS.iter().zip(&state.metrics.buckets) {
        let _ = writeln!(
            text,
            "rustorr_http_response_head_seconds_bucket{{le=\"{bound}\"}} {}",
            bucket.load(Ordering::Relaxed)
        );
    }
    let count = state.metrics.count.load(Ordering::Relaxed);
    let _ = writeln!(
        text,
        "rustorr_http_response_head_seconds_bucket{{le=\"+Inf\"}} {count}\nrustorr_http_response_head_seconds_sum {}\nrustorr_http_response_head_seconds_count {count}",
        state.metrics.sum_micros.load(Ordering::Relaxed) as f64 / 1e6
    );
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode};
    use tower::ServiceExt;

    use crate::app::tests::playback_app;

    async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    use super::*;

    #[tokio::test]
    async fn metrics_count_requests_and_describe_the_server() {
        let (app, _, _) = playback_app();
        assert_eq!(get(&app, "/echo").await.0, StatusCode::OK);
        assert_eq!(get(&app, "/no/such/page").await.0, StatusCode::NOT_FOUND);
        let (status, text) = get(&app, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        for line in [
            "rustorr_http_requests_total{method=\"GET\",code=\"200\"} 1",
            "rustorr_http_requests_total{method=\"GET\",code=\"404\"} 1",
            "rustorr_http_response_head_seconds_count 2",
            "# TYPE rustorr_torrents gauge",
            "rustorr_cache_bytes{kind=\"capacity\"}",
            "rustorr_active_readers ",
            "rustorr_build_info{version=",
        ] {
            assert!(text.contains(line), "{line} missing from:\n{text}");
        }
    }
}
