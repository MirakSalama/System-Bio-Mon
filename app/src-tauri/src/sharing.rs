use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use qrcodegen::{QrCode, QrCodeEcc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Default)]
pub struct Sharing(Arc<Mutex<Option<Session>>>);

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharingInfo {
    active: bool,
    url: Option<String>,
    qr_svg: Option<String>,
}

struct Session {
    info: SharingInfo,
    view: Arc<Viewer>,
    shutdown: Option<oneshot::Sender<()>>,
}

struct Viewer {
    authorization: String,
    latest: Mutex<Snapshot>,
}

// An already-connected viewer must also lose access when sharing stops.
enum Snapshot {
    Waiting,
    Frame(Bytes),
    Revoked,
}

impl Drop for Session {
    fn drop(&mut self) {
        *self.view.latest.lock().unwrap() = Snapshot::Revoked;
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Sharing {
    pub fn is_active(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    pub fn publish(&self, frame: &Value) {
        if let Some(session) = self.0.lock().unwrap().as_ref() {
            if let Ok(bytes) = serde_json::to_vec(frame) {
                *session.view.latest.lock().unwrap() = Snapshot::Frame(bytes.into());
            }
        }
    }

    fn info(&self) -> SharingInfo {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.info.clone())
            .unwrap_or_default()
    }

    // Entered from an asynchronous Tauri command with an active I/O runtime.
    fn start_on(&self, addr: Ipv4Addr) -> Result<SharingInfo, String> {
        let mut session = self.0.lock().unwrap();
        if let Some(existing) = &*session {
            return Ok(existing.info.clone());
        }
        let socket = TcpListener::bind((addr, 0))
            .map_err(|e| format!("Could not open local network sharing: {e}"))?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        let endpoint = socket.local_addr().map_err(|e| e.to_string())?;
        let listener = tokio::net::TcpListener::from_std(socket).map_err(|e| e.to_string())?;
        let mut entropy = [0; 16];
        getrandom::fill(&mut entropy).map_err(|e| e.to_string())?;
        let token = format!("{:032x}", u128::from_be_bytes(entropy));
        let url = format!("http://{endpoint}/?view=phone#{token}");
        let qr_svg = pairing_qr(&url)?;
        let info = SharingInfo {
            active: true,
            url: Some(url),
            qr_svg: Some(qr_svg),
        };
        let view = Arc::new(Viewer {
            authorization: format!("Bearer {token}"),
            latest: Mutex::new(Snapshot::Waiting),
        });
        let router = viewer_router(view.clone());
        let (shutdown, stopped) = oneshot::channel();
        tokio::spawn(serve(listener, router, stopped));
        *session = Some(Session {
            info: info.clone(),
            view,
            shutdown: Some(shutdown),
        });
        Ok(info)
    }

    fn stop(&self) {
        self.0.lock().unwrap().take();
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    mut stopped: oneshot::Receiver<()>,
) {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::service::TowerToHyperService;
    // Several browsers may open multiple connections each. Bound the number of
    // live sockets and discard slow headers so sharing stays light on a gaming PC.
    let mut clients = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stopped => return, // dropping JoinSet also closes existing sockets
            _ = clients.join_next(), if !clients.is_empty() => {}
            incoming = listener.accept(), if clients.len() < 32 => {
                match incoming {
                    Ok((socket, _)) => {
                        let service = TowerToHyperService::new(router.clone());
                        clients.spawn(async move {
                            let _ = hyper::server::conn::http1::Builder::new()
                                .timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10))
                                .max_buf_size(32 * 1024)
                                .serve_connection(TokioIo::new(socket), service).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        }
    }
}

fn lan_address() -> Result<Ipv4Addr, String> {
    // Asking Windows to select a route doesn't send data to the destination.
    let preferred = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .ok()
        .and_then(|socket| {
            socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
            socket.local_addr().ok().map(|addr| addr.ip())
        });
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut candidates = Vec::new();
    for (name, network) in &networks {
        let name = name.to_lowercase();
        for net in network.ip_networks() {
            if let IpAddr::V4(ip) = net.addr {
                if ip.is_private() || ip.is_link_local() {
                    let physical = name.starts_with("wi-fi")
                        || name.starts_with("wlan")
                        || name.starts_with("ethernet");
                    candidates.push((
                        !physical,
                        Some(net.addr) != preferred,
                        ip.is_link_local(),
                        ip,
                    ));
                }
            }
        }
    }
    candidates
        .into_iter()
        .min()
        .map(|candidate| candidate.3)
        .or_else(|| match preferred {
            Some(IpAddr::V4(ip)) if ip.is_private() => Some(ip),
            _ => None,
        })
        .ok_or_else(|| {
            "Connect this PC to a private Wi-Fi or Ethernet network before sharing.".into()
        })
}

fn pairing_qr(url: &str) -> Result<String, String> {
    let qr = QrCode::encode_text(url, QrCodeEcc::Medium).map_err(|e| e.to_string())?;
    let side = qr.size() + 8; // QR scanners require a four-module quiet zone.
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="100%" height="100%" fill="white"/><path fill="black" d=""#
    );
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                write!(svg, "M{} {}h1v1h-1z", x + 4, y + 4).unwrap();
            }
        }
    }
    svg.push_str(r#""/></svg>"#);
    Ok(svg)
}

async fn viewer_page(uri: Uri) -> Response {
    if uri.query() != Some("view=phone") {
        return Redirect::temporary("/?view=phone").into_response();
    }
    Html(include_str!("../../../ui/index.html")).into_response()
}

fn viewer_router(view: Arc<Viewer>) -> Router {
    Router::new()
        .route("/", get(viewer_page))
        .route("/index.html", get(viewer_page))
        .route("/themes.css", get(|| async {
            ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../../../ui/themes.css"))
        }))
        .route("/brand.svg", get(|| async {
            ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../../../ui/brand.svg"))
        }))
        .route("/dashboard.css", get(|| async {
            ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../../../ui/dashboard.css"))
        }))
        .route("/dashboard.js", get(|| async {
            ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../../../ui/dashboard.js"))
        }))
        .route("/vitals.js", get(|| async {
            ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../../../ui/vitals.js"))
        }))
        .route("/api/telemetry", get(telemetry))
        .layer(axum::middleware::map_response(|mut response: Response| async move {
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"
            ));
            response
        }))
        .with_state(view)
}

async fn telemetry(State(view): State<Arc<Viewer>>, headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .map(HeaderValue::as_bytes)
        != Some(view.authorization.as_bytes())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match &*view.latest.lock().unwrap() {
        Snapshot::Revoked => StatusCode::UNAUTHORIZED.into_response(),
        Snapshot::Waiting => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Snapshot::Frame(bytes) => {
            ([(header::CONTENT_TYPE, "application/json")], bytes.clone()).into_response()
        }
    }
}

#[tauri::command]
pub async fn start_sharing(state: tauri::State<'_, Sharing>) -> Result<SharingInfo, String> {
    state.start_on(lan_address()?)
}

#[tauri::command]
pub fn stop_sharing(state: tauri::State<'_, Sharing>) -> SharingInfo {
    state.stop();
    SharingInfo::default()
}

#[tauri::command]
pub fn get_sharing(state: tauri::State<'_, Sharing>) -> SharingInfo {
    state.info()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn request(endpoint: &str, method: &str, path: &str, token: &str) -> String {
        let mut socket = TcpStream::connect(endpoint).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n").unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn pairing_is_read_only_and_rotates_after_stop() {
        let sharing = Sharing::default();
        assert!(!sharing.is_active());
        let start = || {
            tauri::async_runtime::block_on(async { sharing.start_on(Ipv4Addr::LOCALHOST).unwrap() })
        };
        let url = start().url.unwrap();
        assert!(sharing.is_active());
        let (endpoint, token) = url
            .strip_prefix("http://")
            .unwrap()
            .split_once("/?view=phone#")
            .unwrap();
        assert_eq!(start().url.unwrap(), url);
        assert!(request(endpoint, "GET", "/?view=phone", "").starts_with("HTTP/1.1 200"));
        assert!(request(endpoint, "GET", "/themes.css", "").contains("text/css"));
        assert!(request(endpoint, "GET", "/brand.svg", "").contains("image/svg+xml"));
        assert!(request(endpoint, "GET", "/api/telemetry", "").starts_with("HTTP/1.1 401"));
        assert!(request(endpoint, "GET", "/api/telemetry", token).starts_with("HTTP/1.1 503"));
        let sample = serde_json::json!({"t": 123, "gpus": [{"id": "nv-0"}]});
        sharing.publish(&sample);
        let response = request(endpoint, "GET", "/api/telemetry", token);
        assert!(response.starts_with("HTTP/1.1 200"));
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), sample);
        assert!(request(endpoint, "PUT", "/api/telemetry", token).starts_with("HTTP/1.1 405"));
        assert!(request(endpoint, "GET", "/api/settings", token).starts_with("HTTP/1.1 404"));
        let existing_view = sharing.0.lock().unwrap().as_ref().unwrap().view.clone();
        sharing.stop();
        assert!(!sharing.is_active());
        assert!(!sharing.info().active);
        assert!(matches!(
            *existing_view.latest.lock().unwrap(),
            Snapshot::Revoked
        ));
        let next = start().url.unwrap();
        let (new_endpoint, new_token) = next
            .strip_prefix("http://")
            .unwrap()
            .split_once("/?view=phone#")
            .unwrap();
        assert_ne!(new_token, token);
        assert!(request(new_endpoint, "GET", "/api/telemetry", token).starts_with("HTTP/1.1 401"));
        sharing.stop();
    }

    #[test]
    fn rendered_qr_decodes_to_full_pairing_link() {
        let url = "http://192.168.1.20:51234/?view=phone#0123456789abcdef0123456789abcdef";
        let svg = pairing_qr(url).unwrap();
        let side: usize = svg
            .split_once("viewBox=\"0 0 ")
            .unwrap()
            .1
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let scale = 5;
        let width = side * scale;
        let mut pixels = vec![255; width * width];
        let path = svg
            .split_once(" d=\"")
            .unwrap()
            .1
            .split('"')
            .next()
            .unwrap();
        for square in path.split('M').skip(1) {
            let (x, y) = square
                .strip_suffix("h1v1h-1z")
                .unwrap()
                .split_once(' ')
                .unwrap();
            let x = x.parse::<usize>().unwrap() * scale;
            let y = y.parse::<usize>().unwrap() * scale;
            for row in y..y + scale {
                pixels[row * width + x..row * width + x + scale].fill(0);
            }
        }
        let mut decoder = quircs::Quirc::default();
        let code = decoder
            .identify(width, width, &pixels)
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(code.decode().unwrap().payload, url.as_bytes());
    }
}
