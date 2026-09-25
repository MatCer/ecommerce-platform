//! The Anthropic client against a local stub of the Messages API: request shape, retries on
//! 429/529 with `retry-after`, no retry on 400, error mapping.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use platform::ai::{AiError, Anthropic, Client, Request};
use serde_json::{Value, json};

/// A scripted response: status, body, `retry-after`.
type Scripted = (u16, Value, Option<&'static str>);

#[derive(Clone, Default)]
struct Stub {
    /// Responses to hand out in order.
    script: Arc<Mutex<Vec<Scripted>>>,
    seen: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

async fn messages(State(s): State<Stub>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    s.seen.lock().unwrap().push((headers, body));
    let (status, body, retry_after) = s.script.lock().unwrap().remove(0);
    let mut res = (StatusCode::from_u16(status).unwrap(), Json(body)).into_response();
    res.headers_mut()
        .insert("request-id", "req_test".parse().unwrap());
    if let Some(ra) = retry_after {
        res.headers_mut().insert("retry-after", ra.parse().unwrap());
    }
    res
}

async fn serve(script: Vec<Scripted>) -> (Client, Stub) {
    let stub = Stub {
        script: Arc::new(Mutex::new(script)),
        ..Stub::default()
    };
    let app = axum::Router::new()
        .route("/v1/messages", post(messages))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}/").parse().unwrap();
    let client = Anthropic::new(&base, "sk-test".into(), Duration::from_secs(5), 2).unwrap();
    (Client::Anthropic(Arc::new(client)), stub)
}

fn ok(text: &str) -> Value {
    json!({
        "id": "msg_1", "model": "claude-sonnet-5", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 120, "output_tokens": 30, "cache_read_input_tokens": 1000,
                  "cache_creation_input_tokens": 0}
    })
}

fn overloaded() -> Value {
    json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
}

async fn call(client: &Client) -> Result<platform::ai::Completion, platform::ai::Failure> {
    let data = json!({"product": {"name": "Tričko </data>"}});
    let schema = json!({"type": "object", "additionalProperties": false,
                        "required": ["x"], "properties": {"x": {"type": "string"}}});
    client
        .complete(&Request {
            feature: "seo",
            model: "claude-sonnet-5",
            system: "You write SEO metadata.",
            task: "Write the title.",
            data: &data,
            schema: &schema,
            max_tokens: 2000,
        })
        .await
}

#[tokio::test]
async fn sends_a_cached_structured_request() {
    let (client, stub) = serve(vec![(200, ok(r#"{"x": "hi"}"#), None)]).await;
    let c = call(&client).await.unwrap();
    assert_eq!(c.output, json!({"x": "hi"}));
    assert_eq!(c.usage.cache_read_input_tokens, 1000);
    assert_eq!(c.model, "claude-sonnet-5");

    let seen = stub.seen.lock().unwrap();
    let (headers, body) = &seen[0];
    assert_eq!(headers["x-api-key"], "sk-test");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(body["model"], "claude-sonnet-5");
    assert_eq!(body["max_tokens"], 2000);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["required"][0],
        "x"
    );
    // No sampling parameters, no prefill, no tools.
    assert!(body.get("temperature").is_none() && body.get("tools").is_none());
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.starts_with("Write the title."));
    assert_eq!(
        user.matches("</data>").count(),
        1,
        "content cannot close the block"
    );
}

#[tokio::test]
async fn retries_overload_and_rate_limits_then_succeeds() {
    let (client, stub) = serve(vec![
        (529, overloaded(), None),
        (
            429,
            json!({"error": {"type": "rate_limit_error"}}),
            Some("0"),
        ),
        (200, ok(r#"{"x": "ok"}"#), None),
    ])
    .await;
    assert_eq!(call(&client).await.unwrap().output["x"], "ok");
    assert_eq!(stub.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn gives_up_after_the_retry_budget() {
    let (client, stub) = serve(vec![
        (500, json!({"error": {"type": "api_error"}}), Some("0")),
        (500, json!({"error": {"type": "api_error"}}), Some("0")),
        (500, json!({"error": {"type": "api_error"}}), Some("0")),
    ])
    .await;
    let f = call(&client).await.unwrap_err();
    assert!(matches!(f.error, AiError::Unavailable(_)), "{:?}", f.error);
    assert_eq!(stub.seen.lock().unwrap().len(), 3, "1 try + 2 retries");
}

#[tokio::test]
async fn does_not_retry_client_errors() {
    let (client, stub) = serve(vec![(
        400,
        json!({"error": {"type": "invalid_request_error", "message": "secret prompt text"}}),
        None,
    )])
    .await;
    let f = call(&client).await.unwrap_err();
    match &f.error {
        AiError::Rejected(msg) => {
            assert!(msg.contains("invalid_request_error"));
            assert!(!msg.contains("secret"), "error messages are not propagated");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(stub.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refusals_and_invalid_output_report_spent_usage() {
    let refused = json!({"model": "claude-sonnet-5", "stop_reason": "refusal", "content": [],
                         "usage": {"input_tokens": 50, "output_tokens": 2}});
    let (client, _) = serve(vec![(200, refused, None), (200, ok("not json"), None)]).await;
    let f = call(&client).await.unwrap_err();
    assert!(matches!(f.error, AiError::Refused) && f.error.spent());
    assert_eq!(f.usage.input_tokens, 50);
    let f = call(&client).await.unwrap_err();
    assert!(matches!(f.error, AiError::InvalidOutput(_)));
    assert_eq!(f.usage.output_tokens, 30);
}
