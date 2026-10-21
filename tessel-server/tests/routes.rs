//! The routes with the real Ed25519 verifier: what each route refuses and admits, with tokens
//! minted by the same code `tessel-server admin token` runs. The first ten tests are the `routes`
//! cases of the deleted Worker glue, one for one; the rest are the conformance cases of S1. They
//! pin row R1 of `docs/reference/runtime-rules.md`.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use data_encoding::BASE64URL_NOPAD;
use ed25519_dalek::Signer;
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{AgentId, ClientMsg, CommitId, ServerMsg, PROTOCOL_VERSION};
use tessel_server::config::{Config, RepositoryConfig};
use tessel_server::identity::{self, Ed25519Verifier, MintError, Role, TokenRequest};
use tessel_server::keys;
use tessel_server::repo::NoLander;
use tessel_server::server::{self, Deps, RunningServer};
use tessel_server::ws::Verifier;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);
const AUD: &str = "tessel-test";
const DEMO: &str = "/tenant/acme/repo/demo";
const HOUR_MS: u64 = 3_600_000;

/// The refusal every token problem gets: the status and the one fixed body.
fn unauthorized() -> (u16, String) {
    (401, "unauthorized".to_string())
}

struct Http {
    status: u16,
    head: String,
    body: String,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> Result<Http, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr).await?;
    let auth = token.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\n{auth}Content-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = String::new();
    tokio::time::timeout(WAIT, stream.read_to_string(&mut raw)).await??;
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split_whitespace().nth(1).unwrap_or("0").parse()?;
    Ok(Http {
        status,
        head: head.to_string(),
        body: body.to_string(),
    })
}

/// The status and body of the answer, for comparing with a fixed refusal.
async fn answer(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let reply = http(addr, method, path, token).await?;
    Ok((reply.status, reply.body))
}

async fn connect(addr: SocketAddr, path: &str, token: &str) -> Result<Socket, WsError> {
    let mut request = format!("ws://{addr}{path}/ws").into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(socket)
}

fn refused_with(result: Result<Socket, WsError>) -> Result<u16, Box<dyn std::error::Error>> {
    if let Err(WsError::Http(response)) = &result {
        return Ok(response.status().as_u16());
    }
    Err(format!("expected an HTTP refusal, got {:?}", result.err()).into())
}

async fn hello(socket: &mut Socket, agent: &str) -> TestResult {
    let hello = ClientMsg::Hello {
        agent: AgentId(agent.to_string()),
        base: CommitId("ab".repeat(20)),
        protocol: PROTOCOL_VERSION,
    };
    socket
        .send(Message::text(serde_json::to_string(&hello)?))
        .await?;
    Ok(())
}

/// The next server message, or `None` once the socket is closed.
async fn next(socket: &mut Socket) -> Result<Option<ServerMsg>, Box<dyn std::error::Error>> {
    loop {
        match tokio::time::timeout(WAIT, socket.next()).await? {
            Some(Ok(Message::Text(text))) => return Ok(Some(serde_json::from_str(&text)?)),
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

/// A key directory holding a key pair for each of `kids`.
fn key_dir(root: &Path, name: &str, kids: &[&str]) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = root.join(name);
    for kid in kids {
        keys::generate(&dir, kid)?;
    }
    Ok(dir)
}

fn request<'a>(kid: &'a str, tenant: &'a str, repo: &'a str, agent: &'a str) -> TokenRequest<'a> {
    let role = if identity::is_reserved_service(agent) {
        Role::Service
    } else {
        Role::Agent
    };
    TokenRequest {
        kid,
        audience: AUD,
        tenant,
        repo,
        agent,
        role,
        ttl_ms: HOUR_MS,
    }
}

fn mint(
    keys: &Path,
    request: &TokenRequest<'_>,
    now_ms: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let key = keys::load_signing_key(keys, request.kid)?;
    Ok(identity::mint(&key, request, now_ms)?)
}

/// A token of kid `k1` for `acme/<repo>`, valid for an hour from now.
fn token_for(keys: &Path, repo: &str, agent: &str) -> Result<String, Box<dyn std::error::Error>> {
    mint(
        keys,
        &request("k1", "acme", repo, agent),
        identity::now_ms(),
    )
}

/// A token the minting code would refuse to write: `json` signed with the key of `kid`.
fn forge(keys: &Path, kid: &str, json: &str) -> Result<String, Box<dyn std::error::Error>> {
    let key = keys::load_signing_key(keys, kid)?;
    let payload = BASE64URL_NOPAD.encode(json.as_bytes());
    let signature = key.sign(payload.as_bytes());
    Ok(format!(
        "{payload}.{}",
        BASE64URL_NOPAD.encode(&signature.to_bytes())
    ))
}

fn claims_json(role: &str, agent: &str) -> String {
    let exp_ms = identity::now_ms() + HOUR_MS;
    format!(r#"{{"v":1,"kid":"k1","aud":"{AUD}","tenant":"acme","repo":"demo","agent":"{agent}","#)
        + &format!(r#""role":"{role}","exp_ms":{exp_ms}}}"#)
}

async fn start_over(
    dir: &Path,
    keys: &Path,
    repos: &[(&str, &str)],
) -> Result<RunningServer, Box<dyn std::error::Error>> {
    let verifier: Option<Arc<dyn Verifier>> = Some(Arc::new(Ed25519Verifier::load(keys, AUD)?));
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: dir.join("stores"),
        identity: None,
        repositories: repos
            .iter()
            .map(|(tenant, name)| RepositoryConfig {
                tenant: (*tenant).to_string(),
                name: (*name).to_string(),
                run: "test".to_string(),
            })
            .collect(),
    };
    let deps = Deps {
        verifier,
        calls: Arc::new(NoLander),
    };
    Ok(server::start(&config, deps).await?)
}

async fn start(dir: &Path, keys: &Path) -> Result<RunningServer, Box<dyn std::error::Error>> {
    start_over(dir, keys, &[("acme", "demo"), ("acme", "other")]).await
}

#[tokio::test]
async fn adapter_any_agent_of_the_repo_may_read_the_summary() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    for agent in ["dashboard", "a7"] {
        let token = token_for(&keys, "demo", agent)?;
        let reply = http(
            server.local_addr(),
            "GET",
            &format!("{DEMO}/summary"),
            Some(&token),
        )
        .await?;
        assert_eq!(reply.status, 200, "{agent}: {}", reply.head);
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_summary_read_without_a_valid_token_is_unauthorized() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let other_keys = key_dir(dir.path(), "other-keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let hour_ago = identity::now_ms() - 2 * HOUR_MS;
    let tokens = [
        None,
        Some("nonsense".to_string()),
        Some(token_for(&keys, "other", "dashboard")?),
        Some(mint(
            &keys,
            &request("k1", "acme", "demo", "dashboard"),
            hour_ago,
        )?),
        Some(token_for(&other_keys, "demo", "dashboard")?),
    ];
    for token in &tokens {
        let reply = answer(
            server.local_addr(),
            "GET",
            &format!("{DEMO}/summary"),
            token.as_deref(),
        )
        .await?;
        assert_eq!(reply, unauthorized());
    }
    server.stop().await;
    let bare = tempfile::tempdir()?;
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: bare.path().join("stores"),
        identity: None,
        repositories: vec![RepositoryConfig {
            tenant: "acme".to_string(),
            name: "demo".to_string(),
            run: "test".to_string(),
        }],
    };
    let unconfigured = server::start(
        &config,
        Deps {
            verifier: None,
            calls: Arc::new(NoLander),
        },
    )
    .await?;
    let token = token_for(&keys, "demo", "dashboard")?;
    let reply = answer(
        unconfigured.local_addr(),
        "GET",
        &format!("{DEMO}/summary"),
        Some(&token),
    )
    .await?;
    assert_eq!(reply, unauthorized(), "no identity at all");
    unconfigured.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_repo_the_server_does_not_serve_is_refused_before_the_token_is_looked_at(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let token = token_for(&keys, "elsewhere", "dashboard")?;
    for token in [Some(token.as_str()), None] {
        let reply = http(
            server.local_addr(),
            "GET",
            "/tenant/acme/repo/elsewhere/summary",
            token,
        )
        .await?;
        assert_eq!(reply.status, 403, "{}", reply.head);
    }
    let token = token_for(&keys, "demo", "dashboard")?;
    let served = http(
        server.local_addr(),
        "GET",
        &format!("{DEMO}/summary"),
        Some(&token),
    )
    .await?;
    assert_eq!(served.status, 200);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_the_summary_answers_get_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let token = token_for(&keys, "demo", "dashboard")?;
    let path = format!("{DEMO}/summary");
    for method in ["POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "get"] {
        let reply = http(server.local_addr(), method, &path, Some(&token)).await?;
        assert_eq!(reply.status, 405, "{method}");
        assert!(
            reply.head.to_lowercase().contains("allow: get"),
            "{method}: {}",
            reply.head
        );
    }
    let judged_first = http(server.local_addr(), "POST", &path, None).await?;
    assert_eq!(
        judged_first.status, 405,
        "the method is judged before the token"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_trunk_poke_with_a_token_for_its_repo_is_admitted() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let token = token_for(&keys, "demo", "lander")?;
    let reply = http(
        server.local_addr(),
        "POST",
        &format!("{DEMO}/trunk-moved"),
        Some(&token),
    )
    .await?;
    assert_eq!(reply.status, 204, "{}", reply.head);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_trunk_poke_without_a_valid_token_or_for_another_repo_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    let path = format!("{DEMO}/trunk-moved");
    let hour_ago = identity::now_ms() - 2 * HOUR_MS;
    let tokens = [
        None,
        Some("nonsense".to_string()),
        Some(token_for(&keys, "other", "lander")?),
        Some(mint(
            &keys,
            &request("k1", "acme", "demo", "lander"),
            hour_ago,
        )?),
    ];
    for token in &tokens {
        let reply = answer(addr, "POST", &path, token.as_deref()).await?;
        assert_eq!(reply, unauthorized());
    }
    let lander = token_for(&keys, "elsewhere", "lander")?;
    let unserved = http(
        addr,
        "POST",
        "/tenant/acme/repo/elsewhere/trunk-moved",
        Some(&lander),
    )
    .await?;
    assert_eq!(unserved.status, 403);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_trunk_poke_from_any_agent_but_the_lander_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let path = format!("{DEMO}/trunk-moved");
    let others = [
        "a1",
        "dashboard",
        "Lander",
        "lander2",
        "steward",
        "runner",
        "server",
    ];
    for who in others {
        let token = token_for(&keys, "demo", who)?;
        let reply = answer(server.local_addr(), "POST", &path, Some(&token)).await?;
        assert_eq!(reply, unauthorized(), "{who}");
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_trunk_poke_takes_post_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let token = token_for(&keys, "demo", "lander")?;
    let path = format!("{DEMO}/trunk-moved");
    for method in ["GET", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "post"] {
        let reply = http(server.local_addr(), method, &path, Some(&token)).await?;
        assert_eq!(reply.status, 405, "{method}");
        assert!(
            reply.head.to_lowercase().contains("allow: post"),
            "{method}: {}",
            reply.head
        );
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_the_ws_route_keeps_its_order_of_refusals() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    let token = token_for(&keys, "demo", "a1")?;
    let path = format!("{DEMO}/ws");
    for method in ["GET", "POST"] {
        let reply = http(addr, method, &path, Some(&token)).await?;
        assert_eq!(
            reply.status, 426,
            "{method}: the upgrade check judges the method of a ws request, after the token"
        );
    }
    let unserved = http(addr, "GET", "/tenant/acme/repo/elsewhere/ws", Some(&token)).await?;
    assert_eq!(unserved.status, 403);
    assert_eq!(http(addr, "GET", &path, None).await?.status, 401);
    assert_eq!(http(addr, "GET", "/nope", Some(&token)).await?.status, 404);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_the_ws_route_binds_the_agent_of_a_token_for_its_repo_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    let demo = token_for(&keys, "demo", "a7")?;
    let mut socket = connect(addr, DEMO, &demo).await?;
    hello(&mut socket, "a7").await?;
    let welcome = next(&mut socket).await?;
    assert!(
        matches!(welcome, Some(ServerMsg::Welcome { .. })),
        "{welcome:?}"
    );
    let mut liar = connect(addr, DEMO, &demo).await?;
    hello(&mut liar, "a8").await?;
    let refusal = next(&mut liar).await?;
    assert!(
        matches!(refusal, Some(ServerMsg::Error { .. })),
        "{refusal:?}"
    );
    assert!(next(&mut liar).await?.is_none(), "closed with 1008");
    let elsewhere = token_for(&keys, "other", "a7")?;
    assert_eq!(refused_with(connect(addr, DEMO, &elsewhere).await)?, 401);
    let mut other = connect(addr, "/tenant/acme/repo/other", &elsewhere).await?;
    hello(&mut other, "a7").await?;
    assert!(matches!(
        next(&mut other).await?,
        Some(ServerMsg::Welcome { .. })
    ));
    assert_eq!(
        refused_with(connect(addr, "/tenant/acme/repo/other", &demo).await)?,
        401
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_refusals_map_to_404_405_403_401_with_fixed_bodies() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    let token = token_for(&keys, "demo", "a1")?;
    let expired = mint(
        &keys,
        &request("k1", "acme", "demo", "a1"),
        identity::now_ms() - 2 * HOUR_MS,
    )?;
    let cases = [
        (
            "GET",
            "/repo/demo/ws".to_string(),
            404,
            "expected /tenant/<tenant>/repo/<name>/ws, /summary or /trunk-moved",
        ),
        ("POST", format!("{DEMO}/summary"), 405, "method not allowed"),
        (
            "GET",
            "/tenant/acme/repo/elsewhere/summary".to_string(),
            403,
            "this deployment does not serve that repo",
        ),
        ("GET", format!("{DEMO}/summary"), 401, "unauthorized"),
    ];
    for (method, path, status, body) in cases {
        let presented = if status == 401 { &expired } else { &token };
        let reply = answer(addr, method, &path, Some(presented)).await?;
        assert_eq!(reply, (status, body.to_string()), "{method} {path}");
    }
    let mut refused = Vec::new();
    for presented in [None, Some("nonsense"), Some(expired.as_str())] {
        let reply = answer(addr, "GET", &format!("{DEMO}/summary"), presented).await?;
        refused.push(reply);
    }
    assert!(
        refused.iter().all(|reply| *reply == unauthorized()),
        "every reason gets the same body: {refused:?}"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_token_for_another_audience_or_tenant_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let repos = [("acme", "demo"), ("other", "demo")];
    let server = start_over(dir.path(), &keys, &repos).await?;
    let addr = server.local_addr();
    let now = identity::now_ms();
    let mut wrong_audience = request("k1", "acme", "demo", "a1");
    wrong_audience.audience = "tessel-elsewhere";
    let path = format!("{DEMO}/summary");
    let token = mint(&keys, &wrong_audience, now)?;
    assert_eq!(
        answer(addr, "GET", &path, Some(&token)).await?,
        unauthorized()
    );
    let other_tenant = mint(&keys, &request("k1", "other", "demo", "a1"), now)?;
    assert_eq!(
        answer(addr, "GET", &path, Some(&other_tenant)).await?,
        unauthorized()
    );
    let own = answer(
        addr,
        "GET",
        "/tenant/other/repo/demo/summary",
        Some(&other_tenant),
    )
    .await?;
    assert_eq!(own.0, 200, "the same token is good for its own tenant");
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_rotated_kids_both_verify_and_a_removed_kid_is_refused_after_a_restart(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1", "k2"])?;
    let now = identity::now_ms();
    let old = mint(&keys, &request("k1", "acme", "demo", "a1"), now)?;
    let new = mint(&keys, &request("k2", "acme", "demo", "a1"), now)?;
    let path = format!("{DEMO}/summary");
    let server = start(dir.path(), &keys).await?;
    for token in [&old, &new] {
        assert_eq!(
            answer(server.local_addr(), "GET", &path, Some(token))
                .await?
                .0,
            200
        );
    }
    server.stop().await;
    std::fs::remove_file(keys.join("k1.pub"))?;
    let server = start(dir.path(), &keys).await?;
    let refused = answer(server.local_addr(), "GET", &path, Some(&old)).await?;
    assert_eq!(refused, unauthorized(), "the removed kid");
    assert_eq!(
        answer(server.local_addr(), "GET", &path, Some(&new))
            .await?
            .0,
        200
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_reserved_name_is_never_an_agent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let key = keys::load_signing_key(&keys, "k1")?;
    for name in ["lander", "runner", "server"] {
        let agent = TokenRequest {
            role: Role::Agent,
            ..request("k1", "acme", "demo", name)
        };
        let refused = identity::mint(&key, &agent, identity::now_ms());
        assert!(
            matches!(refused, Err(MintError::ReservedAgent { .. })),
            "{name}"
        );
    }
    let service = TokenRequest {
        role: Role::Service,
        ..request("k1", "acme", "demo", "a1")
    };
    let refused = identity::mint(&key, &service, identity::now_ms());
    assert!(matches!(refused, Err(MintError::NotAService)));
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    for (role, name) in [
        ("agent", "lander"),
        ("agent", "runner"),
        ("agent", "server"),
        ("service", "a1"),
    ] {
        let token = forge(&keys, "k1", &claims_json(role, name))?;
        for (method, path) in [
            ("GET", format!("{DEMO}/summary")),
            ("POST", format!("{DEMO}/trunk-moved")),
        ] {
            let reply = answer(addr, method, &path, Some(&token)).await?;
            assert_eq!(reply, unauthorized(), "{role} {name} {path}");
        }
    }
    let honest = forge(&keys, "k1", &claims_json("service", "lander"))?;
    let admitted = http(addr, "POST", &format!("{DEMO}/trunk-moved"), Some(&honest)).await?;
    assert_eq!(
        admitted.status, 204,
        "the forging helper writes what the server accepts"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_a_service_token_is_refused_on_ws_and_summary() -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = key_dir(dir.path(), "keys", &["k1"])?;
    let server = start(dir.path(), &keys).await?;
    let addr = server.local_addr();
    for service in ["lander", "runner", "server"] {
        let token = token_for(&keys, "demo", service)?;
        let summary = answer(addr, "GET", &format!("{DEMO}/summary"), Some(&token)).await?;
        assert_eq!(summary, unauthorized(), "{service}");
        assert_eq!(
            refused_with(connect(addr, DEMO, &token).await)?,
            401,
            "{service}"
        );
    }
    server.stop().await;
    Ok(())
}
