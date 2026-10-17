use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use clap::Parser;
use futures_util::stream::StreamExt;
use tower_http::cors::{Any, CorsLayer};

const DEFAULT_API_BASE: &str = "https://index-translate.bilibili.com/v1";
const DEFAULT_MODEL: &str = "Index-Translate-35B-A3B";

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Index-Translate OpenAI-compatible Rust Proxy Server")]
struct Cli {
    #[arg(long, env = "HOST", default_value = "127.0.0.1")]
    host: String,

    #[arg(short, long, env = "PORT", default_value_t = 8080)]
    port: u16,

    #[arg(long, env = "API_BASE", default_value = DEFAULT_API_BASE)]
    api_base: String,

    #[arg(long, env = "API_KEY")]
    api_key: Option<String>,
}

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    api_base: String,
    api_key: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()?;

    let state = Arc::new(AppState {
        client,
        api_base: cli.api_base.trim_end_matches('/').to_string(),
        api_key: cli.api_key.clone(),
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .max_age(Duration::from_secs(86400));

    let app = Router::new()
        .route("/", get(ready_handler))
        .route("/v1/models", get(models_handler))
        .route("/models", get(models_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/chat/completions", post(chat_completions_handler))
        .route("/v1/completions", post(completions_handler))
        .route("/completions", post(completions_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/responses", post(responses_handler))
        .layer(cors)
        .with_state(state);

    let bind_addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    let display_host = if cli.host == "0.0.0.0" || cli.host == "::" {
        "127.0.0.1"
    } else {
        &cli.host
    };

    println!("{}", "=".repeat(60));
    println!(
        "Index-Translate Rust Proxy listening on {}:{} — http://{}:{}/v1",
        cli.host, cli.port, display_host, cli.port
    );
    println!("Upstream API: {}", cli.api_base);
    if let Some(ref key) = cli.api_key {
        println!("API Key Protection: Enabled (key: {})", key);
    } else {
        println!("API Key Protection: Disabled (Any/No key accepted)");
    }
    println!("{}", "=".repeat(60));

    axum::serve(listener, app).await?;
    Ok(())
}

fn verify_auth(headers: &HeaderMap, required_key: &Option<String>) -> Result<(), Response> {
    let Some(expected) = required_key else {
        return Ok(());
    };

    let incoming_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|auth| auth.strip_prefix("Bearer ").unwrap_or(auth).trim())
        .or_else(|| {
            headers
                .get("api-key")
                .and_then(|v| v.to_str().ok())
                .map(|k| k.trim())
        });

    if incoming_token != Some(expected.as_str()) {
        let err_body = serde_json::json!({
            "error": {
                "message": "Incorrect API key provided.",
                "type": "invalid_request_error",
                "code": "invalid_api_key"
            }
        });
        return Err((
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            Json(err_body),
        )
            .into_response());
    }

    Ok(())
}
async fn ready_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Index-Translate Proxy Ready\n",
    )
}

async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Response> {
    verify_auth(&headers, &state.api_key)?;

    let body = serde_json::json!({
        "object": "list",
        "data": [
            {"id": "Index-Translate-35B-A3B", "object": "model"},
            {"id": "Index-Translate-2B", "object": "model"},
            {"id": "Index-Translate-9B", "object": "model"},
        ]
    });
    Ok(Json(body))
}

fn patch_chat_payload(payload: &mut serde_json::Value) {
    if let Some(obj) = payload.as_object_mut() {
        let ctk = obj
            .entry("chat_template_kwargs")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(ctk_obj) = ctk.as_object_mut() {
            if !ctk_obj.contains_key("enable_thinking") {
                ctk_obj.insert("enable_thinking".to_string(), serde_json::Value::Bool(false));
            }
        }
        if !obj.contains_key("temperature") || obj["temperature"].is_null() {
            obj.insert("temperature".to_string(), serde_json::json!(0.0));
        }
    }
}

async fn chat_completions_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Response> {
    verify_auth(&headers, &state.api_key)?;

    let mut payload: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Invalid JSON: {}", e)})),
        )
            .into_response()
    })?;

    patch_chat_payload(&mut payload);

    let client_wants_stream = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    payload["stream"] = serde_json::Value::Bool(true);

    let upstream_url = format!("{}/chat/completions", state.api_base);

    let max_attempts = 4;
    let last_error_response: Option<Response> = None;

    for attempt in 1..=max_attempts {
        let req = state
            .client
            .post(&upstream_url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::USER_AGENT, "Index-Translate-Client/1.0")
            .json(&payload);

        let resp = match req.send().await {
            Ok(r) => r,
            Err(err) => {
                eprintln!("[Attempt {}/{}] 连接上游失败: {:?}", attempt, max_attempts, err);
                if attempt < max_attempts {
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                    continue;
                }
                return Ok(make_error_response(
                    StatusCode::BAD_GATEWAY,
                    "Bad Gateway: Upstream Unreachable",
                ));
            }
        };

        let status = resp.status();

        if is_retryable_status(status) {
            eprintln!(
                "[Attempt {}/{}] 上游返回可重试状态码: {}",
                attempt, max_attempts, status
            );
            if attempt < max_attempts {
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                continue;
            } else {
                let err_bytes = resp.bytes().await.unwrap_or_default();
                return Ok(Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(err_bytes))
                    .unwrap());
            }
        }

        if status.is_client_error() {
            let body_bytes = resp.bytes().await.unwrap_or_default();
            return Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body_bytes))
                .unwrap());
        }

        if client_wants_stream {
            let mut stream = resp.bytes_stream();
            match stream.next().await {
                Some(Ok(first_chunk)) if !first_chunk.is_empty() => {
                    let chained =
                        futures_util::stream::once(async move { Ok::<_, reqwest::Error>(first_chunk) })
                            .chain(stream);

                    return Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .header(header::CACHE_CONTROL, "no-cache")
                        .header(header::CONNECTION, "keep-alive")
                        .body(Body::from_stream(chained))
                        .unwrap());
                }
                _ => {
                    eprintln!(
                        "[Attempt {}/{}] 上游返回空流或首包中断，执行重试...",
                        attempt, max_attempts
                    );
                    if attempt < max_attempts {
                        tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                        continue;
                    }
                }
            }
        } else {
            match assemble_sse_to_json(resp).await {
                Ok(Some(assembled_json)) => {
                    return Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&assembled_json).unwrap()))
                        .unwrap());
                }
                Ok(None) => {
                    eprintln!(
                        "[Attempt {}/{}] 上游未产生有效 Token 即关闭，执行重试...",
                        attempt, max_attempts
                    );
                    if attempt < max_attempts {
                        tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                        continue;
                    }
                }
                Err(e) => {
                    eprintln!("[Attempt {}/{}] 读取流异常: {:?}", attempt, max_attempts, e);
                    if attempt < max_attempts {
                        tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                        continue;
                    }
                }
            }
        }
    }

    Ok(last_error_response.unwrap_or_else(|| {
        make_error_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout: Max Retries Exceeded")
    }))
}

async fn completions_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Response> {
    forward_generic_request(state, headers, body, "/completions").await
}

async fn responses_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Response> {
    forward_generic_request(state, headers, body, "/responses").await
}

async fn forward_generic_request(
    state: Arc<AppState>,
    headers: HeaderMap,
    body: Bytes,
    endpoint: &str,
) -> Result<Response, Response> {
    verify_auth(&headers, &state.api_key)?;

    let mut payload: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    if payload.is_object() {
        patch_chat_payload(&mut payload);
    }

    let upstream_url = format!("{}{}", state.api_base, endpoint);
    let max_attempts = 4;

    for attempt in 1..=max_attempts {
        let req = state
            .client
            .post(&upstream_url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::USER_AGENT, "Index-Translate-Client/1.0")
            .json(&payload);

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < max_attempts {
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                    continue;
                }
                return Ok(make_error_response(StatusCode::BAD_GATEWAY, &format!("Upstream Error: {}", e)));
            }
        };

        let status = resp.status();
        if is_retryable_status(status) {
            if attempt < max_attempts {
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                continue;
            }
        }

        let is_stream = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        return if is_stream {
            Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(resp.bytes_stream()))
                .unwrap())
        } else {
            let res_bytes = resp.bytes().await.unwrap_or_default();
            if res_bytes.is_empty() && attempt < max_attempts {
                continue;
            }
            Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(res_bytes))
                .unwrap())
        }
    }

    Ok(make_error_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout"))
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn strip_think(text: &str) -> String {
    let clean = if let Some((_, after)) = text.split_once("</think>") {
        after
    } else {
        text
    };
    let clean = clean.trim();
    let clean = clean.strip_prefix("<think>").unwrap_or(clean);
    clean.trim().to_string()
}

async fn assemble_sse_to_json(
    resp: reqwest::Response,
) -> Result<Option<serde_json::Value>, reqwest::Error> {
    let mut stream = resp.bytes_stream();
    let mut buffer = Vec::new();

    let mut id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut created: Option<i64> = None;
    let mut full_text = String::new();
    let mut finish_reason: Option<String> = None;
    let mut got_valid_chunk = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        buffer.extend_from_slice(&chunk);

        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\r', '\n']).trim();

            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }

            let data = line.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                break;
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                got_valid_chunk = true;
                if id.is_none() {
                    id = val.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                }
                if model.is_none() {
                    model = val.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
                }
                if created.is_none() {
                    created = val.get("created").and_then(|v| v.as_i64());
                }
                if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
                    if let Some(first) = choices.first() {
                        if let Some(content) = first
                            .get("delta")
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            full_text.push_str(content);
                        }
                        if let Some(fr) = first.get("finish_reason").and_then(|f| f.as_str()) {
                            finish_reason = Some(fr.to_string());
                        }
                    }
                }
            }
        }
    }

    if !got_valid_chunk {
        return Ok(None);
    }

    let final_translation = strip_think(&full_text);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    Ok(Some(serde_json::json!({
        "id": id.unwrap_or_else(|| format!("chatcmpl-{}", now)),
        "object": "chat.completion",
        "created": created.unwrap_or(now),
        "model": model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": final_translation,
                },
                "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string()),
            }
        ],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    })))
}

fn make_error_response(status: StatusCode, msg: &str) -> Response {
    let err_json = serde_json::json!({
        "error": {
            "message": msg,
            "type": "proxy_error"
        }
    });

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&err_json).unwrap()))
        .unwrap()
}
