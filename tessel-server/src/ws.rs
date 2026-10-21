//! The HTTP side: the `/ws` upgrade, the summary read and the trunk poke.
//!
//! Routes. `/tenant/<tenant>/repo/<repo>/{ws,summary,trunk-moved}`. The part after the tenant is
//! the grammar the Worker served, `shell::parse_route`, reused unchanged; the tenant prefix is
//! what S5 adds. The CLI builds `<coordinator>/repo/<repo>/ws` from its configured address, so a
//! coordinator address of `ws://host:port/tenant/<tenant>` reaches a tenant's repositories
//! without a change to the CLI.
//!
//! Refusals come in the order the Worker used: path (404), method (405 with `Allow`), repository
//! not served (403), then the token (401). A `ws` request that is not an upgrade is then 426.
//!
//! The adapter sets the verified agent itself, from the token and nothing else (R2): no header
//! the client sends names an agent. A refusal is logged with its reason and path, never the
//! token (R3).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::{ALLOW, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tessel_coordinator::protocol::AgentId;
use tessel_coordinator::shell::{self, PokeReply, Route, MAX_FRAME_BYTES};

use crate::id::RepoId;
use crate::repo::{RepoHandle, Wire};

/// The body of every 401: the same for every reason, which is only logged.
const UNAUTHORIZED_BODY: &str = "unauthorized";

/// The longest path a log line quotes.
const LOGGED_PATH_MAX: usize = 200;

/// Why a token is refused. The reason is fixed text, so it can never carry the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
}

/// The one port the routes need for identity: from the token a request presented, for the
/// repository it asks for, to the agent it was issued to or a refusal.
pub trait Verifier: Send + Sync + 'static {
    fn verify(&self, token: &str, repo: &RepoId) -> Result<AgentId, Refusal>;
}

/// What a route serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Ws,
    Summary,
    TrunkMoved,
}

/// The repository and the target a URL path names, or `None` for any other path, a name that is
/// not a valid name included.
pub fn parse_path(path: &str) -> Option<(RepoId, Target)> {
    let segments: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
    let [prefix, tenant, rest @ ..] = segments.as_slice() else {
        return None;
    };
    if *prefix != "tenant" {
        return None;
    }
    let (repo, target) = match shell::parse_route(rest)? {
        Route::Ws { repo } => (repo, Target::Ws),
        Route::Summary { repo } => (repo, Target::Summary),
        Route::TrunkMoved { repo } => (repo, Target::TrunkMoved),
    };
    Some((RepoId::new(tenant, repo).ok()?, target))
}

/// What the routes serve and how they authenticate.
pub struct App {
    pub repos: BTreeMap<RepoId, RepoHandle>,
    /// `None` is a server with no identity configured: every authenticated route answers 401.
    pub verifier: Option<Arc<dyn Verifier>>,
}

pub fn router(app: Arc<App>) -> Router {
    Router::new().fallback(serve).with_state(app)
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

fn quoted(path: &str) -> String {
    let cut: String = path.chars().take(LOGGED_PATH_MAX).collect();
    format!("{cut:?}")
}

/// The agent the request's token was issued to, or the reason it was refused.
fn authenticate(app: &App, headers: &HeaderMap, id: &RepoId) -> Result<AgentId, &'static str> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or("no bearer token")?;
    let verifier = app.verifier.as_ref().ok_or("no identity is configured")?;
    verifier.verify(token, id).map_err(|refusal| refusal.reason)
}

async fn serve(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let Some((id, target)) = parse_path(uri.path()) else {
        return text(
            StatusCode::NOT_FOUND,
            "expected /tenant/<tenant>/repo/<name>/ws, /summary or /trunk-moved",
        );
    };
    let allowed = match target {
        Target::Ws => None,
        Target::Summary => Some(Method::GET),
        Target::TrunkMoved => Some(Method::POST),
    };
    if let Some(allowed) = allowed.filter(|allowed| *allowed != method) {
        let mut refusal = text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        if let Ok(value) = allowed.as_str().parse() {
            refusal.headers_mut().insert(ALLOW, value);
        }
        return refusal;
    }
    let Some(repo) = app.repos.get(&id) else {
        return text(
            StatusCode::FORBIDDEN,
            "this deployment does not serve that repo",
        );
    };
    let agent = match authenticate(&app, &headers, &id) {
        Ok(agent) => agent,
        Err(reason) => {
            tracing::warn!(path = %quoted(uri.path()), reason, "refused a request");
            return text(StatusCode::UNAUTHORIZED, UNAUTHORIZED_BODY);
        }
    };
    match target {
        Target::Summary => summary(repo).await,
        Target::TrunkMoved => trunk_moved(repo).await,
        Target::Ws => match upgrade {
            Ok(upgrade) => accept(repo.clone(), agent, upgrade),
            Err(_) => text(StatusCode::UPGRADE_REQUIRED, "expected WebSocket upgrade"),
        },
    }
}

async fn summary(repo: &RepoHandle) -> Response {
    match repo.summary().await {
        Ok(body) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, "application/json"),
                (CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(repo = %repo.id(), error = %error, "summary failed");
            text(StatusCode::INTERNAL_SERVER_ERROR, "summary unavailable")
        }
    }
}

async fn trunk_moved(repo: &RepoHandle) -> Response {
    match repo.poke().await {
        Ok(PokeReply::UnknownRepo) => StatusCode::NO_CONTENT.into_response(),
        Ok(PokeReply::Recorded) => StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            tracing::error!(repo = %repo.id(), error = %error, "trunk poke failed");
            text(StatusCode::INTERNAL_SERVER_ERROR, "poke unavailable")
        }
    }
}

fn accept(repo: RepoHandle, agent: AgentId, upgrade: WebSocketUpgrade) -> Response {
    if !repo.is_running() {
        return text(StatusCode::SERVICE_UNAVAILABLE, "repository unavailable");
    }
    upgrade
        .max_message_size(MAX_FRAME_BYTES * 4)
        .max_frame_size(MAX_FRAME_BYTES * 4)
        .on_upgrade(move |socket| socket_loop(socket, repo, agent))
}

/// One socket: frames in go to the repository's task, what the host sends comes out, in order.
async fn socket_loop(mut socket: WebSocket, repo: RepoHandle, agent: AgentId) {
    let Some((sock, mut wire)) = repo.open(agent).await else {
        return;
    };
    loop {
        tokio::select! {
            outgoing = wire.recv() => match outgoing {
                Some(Wire::Text(text)) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Some(Wire::Close { code, reason }) => {
                    let frame = CloseFrame { code, reason: reason.into() };
                    let _ = socket.send(Message::Close(Some(frame))).await;
                    break;
                }
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => repo.frame(sock, text.as_str().to_string()).await,
                Some(Ok(Message::Binary(_))) => repo.binary(sock).await,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
        }
    }
    repo.closed(sock).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_a_repository_of_a_tenant_and_what_to_serve() {
        let id = RepoId::new("acme", "demo").unwrap();
        for (path, target) in [
            ("/tenant/acme/repo/demo/ws", Target::Ws),
            ("/tenant/acme/repo/demo/summary", Target::Summary),
            ("/tenant/acme/repo/demo/trunk-moved", Target::TrunkMoved),
        ] {
            assert_eq!(parse_path(path), Some((id.clone(), target)), "{path}");
        }
    }

    #[test]
    fn any_other_path_names_nothing() {
        for path in [
            "",
            "/",
            "/repo/demo/ws",
            "/tenant",
            "/tenant/acme",
            "/tenant/acme/repo/demo",
            "/tenant/acme/repo//ws",
            "/tenant//repo/demo/ws",
            "/tenant/acme/repo/demo/ws/",
            "/tenant/acme/repo/demo/events",
            "/tenant/Acme/repo/demo/ws",
            "/tenant/acme/repo/../ws",
            "/tenant/acme/repo/a%2Fb/ws",
            "/tenant/ac.me/repo/demo/ws",
            "/other/acme/repo/demo/ws",
        ] {
            assert_eq!(parse_path(path), None, "{path}");
        }
    }
}
