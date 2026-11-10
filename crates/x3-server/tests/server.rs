use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use http::{Request, StatusCode};
use tower::ServiceExt;
use x3_core::{
    audio, AliasTable, AudioFormat, Engine, LoadedModel, ModelId, ModelInfo, ModelRouter, Pricing,
    Result as CoreResult, SttRequest, Task, Transcript, TtsAudio, TtsRequest, Voice,
};
use x3_server::{build_app, Db, NewToken, Paths, Settings, VoiceService};
use x3_vault::{MemoryStore, SecretStore, SecretString};

// ---------------------------------------------------------------- fakes

struct Fake;
struct FakeModel(ModelId);

fn info(id: &str, task: Task, pricing: Pricing, cloud: bool) -> ModelInfo {
    ModelInfo {
        id: ModelId::parse(id).unwrap(),
        task,
        display_name: id.into(),
        cloud,
        available: true,
        pricing,
    }
}

#[async_trait]
impl Engine for Fake {
    fn provider(&self) -> &str {
        "fake"
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![
            info("fake/free", Task::Tts, Pricing::FREE_CHARS, false),
            // $1 per 1k chars = $0.001 per char.
            info("fake/paid", Task::Tts, Pricing::per_1k_chars(1.0), true),
            // $36 per hour = $0.01 per second.
            info("fake/ears", Task::Stt, Pricing::per_hour(36.0), true),
        ]
    }
    async fn load(&self, model: &ModelId) -> CoreResult<Arc<dyn LoadedModel>> {
        Ok(Arc::new(FakeModel(model.clone())))
    }
    async fn list_voices(&self) -> CoreResult<Vec<Voice>> {
        Ok(vec![Voice {
            id: "v1".into(),
            name: "Fake One".into(),
            provider: "fake".into(),
            language: Some("en".into()),
            quality: None,
            description: None,
        }])
    }
}

#[async_trait]
impl LoadedModel for FakeModel {
    fn id(&self) -> &ModelId {
        &self.0
    }
    async fn tts(&self, req: TtsRequest) -> CoreResult<TtsAudio> {
        let samples = vec![0.0f32; 1600];
        Ok(TtsAudio {
            bytes: audio::wav_from_f32(&samples, 16000)?,
            format: req.format,
            sample_rate: Some(16000),
            duration_secs: Some(0.1),
        })
    }
    async fn stt(&self, req: SttRequest) -> CoreResult<Transcript> {
        Ok(Transcript {
            text: format!("heard {} bytes", req.audio.len()),
            language: Some("en".into()),
            duration_secs: audio::wav_duration_secs(&req.audio),
            segments: vec![],
            words: req.word_timestamps.then(Vec::new),
        })
    }
}

// ---------------------------------------------------------------- harness

struct Harness {
    svc: Arc<VoiceService>,
    token: String,
    _dir: tempfile::TempDir,
}

async fn harness_with(router: ModelRouter, settings: Settings, new: NewToken) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path());
    paths.ensure().unwrap();
    let db = Db::in_memory().await.unwrap();
    let svc = Arc::new(VoiceService::new(Arc::new(router), db, paths, settings));
    let (_, token) = svc.db().mint_token(&new).await.unwrap();
    Harness {
        svc,
        token,
        _dir: dir,
    }
}

fn fake_router() -> ModelRouter {
    let mut r = ModelRouter::new(AliasTable::empty());
    r.register(Arc::new(Fake));
    r
}

fn fake_settings() -> Settings {
    let mut s = Settings {
        aliases: AliasTable::empty(),
        ..Default::default()
    };
    s.aliases
        .set("tts", ModelId::parse("fake/free").unwrap())
        .unwrap();
    s.aliases
        .set("stt", ModelId::parse("fake/ears").unwrap())
        .unwrap();
    s
}

async fn harness() -> Harness {
    harness_with(
        fake_router(),
        fake_settings(),
        NewToken {
            label: "test".into(),
            ..Default::default()
        },
    )
    .await
}

impl Harness {
    fn app(&self) -> axum::Router {
        build_app(self.svc.clone(), tokio_util::sync::CancellationToken::new())
    }

    fn req(&self, method: &str, uri: &str) -> http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:47800")
            .header("authorization", format!("Bearer {}", self.token))
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, http::HeaderMap, Vec<u8>) {
        let resp = self.app().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    async fn speak(&self, body: serde_json::Value) -> (StatusCode, http::HeaderMap, Vec<u8>) {
        self.send(
            self.req("POST", "/v1/audio/speech")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }
}

fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn health_needs_no_token_but_api_does() {
    let h = harness().await;
    let (s, _, _) = h
        .send(
            Request::get("/health")
                .header("host", "localhost:47800")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, body) = h
        .send(
            Request::get("/v1/models")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&body)["error"]["code"], "invalid_token");

    let (s, _, _) = h
        .send(
            Request::get("/v1/models")
                .header("host", "localhost")
                .header("authorization", "Bearer x3_live_deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, _, body) = h
        .send(h.req("GET", "/v1/models").body(Body::empty()).unwrap())
        .await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<String> = json(&body)["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"fake/paid".to_string()));
    assert!(ids.contains(&"tts".to_string()));
}

#[tokio::test]
async fn mcp_requires_token_too() {
    let h = harness().await;
    let (s, _, _) = h
        .send(
            Request::post("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_tokens_are_rejected() {
    let h = harness().await;
    let id = h.svc.db().list_tokens().await.unwrap()[0].id.clone();
    assert!(h.svc.db().revoke_token(&id).await.unwrap());
    let (s, _, _) = h
        .send(h.req("GET", "/v1/models").body(Body::empty()).unwrap())
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn host_and_origin_guard() {
    let h = harness().await;
    for host in [
        "evil.com",
        "localhost.evil.com",
        "127.0.0.1.nip.io:47800",
        "0.0.0.0:47800",
    ] {
        let (s, _, _) = h
            .send(
                Request::get("/v1/models")
                    .header("host", host)
                    .header("authorization", format!("Bearer {}", h.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "host {host}");
    }
    // Missing Host is rejected too.
    let (s, _, _) = h
        .send(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Cross-origin browser requests are rejected.
    let (s, _, _) = h
        .send(
            h.req("GET", "/v1/models")
                .header("origin", "https://evil.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, headers, _) = h
        .send(
            h.req("GET", "/v1/models")
                .header("origin", "http://localhost:5173")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers.get("access-control-allow-origin").is_none());
}

// ---------------------------------------------------------------- routing

#[tokio::test]
async fn alias_routing_and_openai_compat() {
    let h = harness().await;
    let (s, headers, body) = h.speak(serde_json::json!({ "input": "hello" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["x-x3-model"], "fake/free");
    assert_eq!(headers["content-type"], "audio/wav");
    assert!(audio::wav_duration_secs(&body).is_some());

    // OpenAI names map onto aliases; OpenAI voices fall back to default.
    let (s, headers, _) = h
        .speak(serde_json::json!({ "model": "tts-1", "input": "hi", "voice": "alloy" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["x-x3-model"], "fake/free");

    // Editing the alias table takes effect immediately.
    h.svc
        .update_settings(|s| {
            s.aliases
                .set("tts", ModelId::parse("fake/paid").unwrap())
                .unwrap()
        })
        .unwrap();
    let (s, headers, _) = h.speak(serde_json::json!({ "input": "hello" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["x-x3-model"], "fake/paid");
    assert_eq!(headers["content-type"], "audio/mpeg"); // cloud default is mp3

    // Unknown model / wrong task.
    let (s, _, body) = h
        .speak(serde_json::json!({ "model": "nope/x", "input": "hi" }))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["code"], "model_not_found");
    let (s, _, _) = h
        .speak(serde_json::json!({ "model": "fake/ears", "input": "hi" }))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = h
        .speak(serde_json::json!({ "input": "hi", "response_format": "aac" }))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn speech_to_path_writes_into_outputs_dir() {
    let h = harness().await;
    let (s, _, body) = h
        .speak(serde_json::json!({ "input": "hello", "response": "path" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    let v = json(&body);
    let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    assert!(path.starts_with(&h.svc.paths().outputs_dir));
    assert!(path.extension().unwrap() == "wav");
    assert!(path.is_file());
    assert!(v["duration_seconds"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn transcriptions_multipart() {
    let h = harness().await;
    let wav = audio::wav_from_f32(&vec![0.0; 16000 * 2], 16000).unwrap();
    let boundary = "XBOUNDARY";
    let mut body = Vec::new();
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    field("model", "whisper-1");
    field("response_format", "verbose_json");
    field("timestamp_granularities[]", "word");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let (s, _, out) = h
        .send(
            h.req("POST", "/v1/audio/transcriptions")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&out));
    let v = json(&out);
    assert_eq!(v["text"], format!("heard {} bytes", wav.len()));
    assert_eq!(v["model"], "fake/ears");
    assert!((v["duration"].as_f64().unwrap() - 2.0).abs() < 1e-6);
    assert!(v["words"].is_array());
    // 2 s at $0.01/s
    assert!((v["estimated_cost_usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
}

#[tokio::test]
async fn voices_endpoint() {
    let h = harness().await;
    let (s, _, body) = h
        .send(
            h.req("GET", "/v1/voices?provider=fake")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json(&body)["data"][0]["id"], "v1");
}

// ---------------------------------------------------------------- budgets & scopes

#[tokio::test]
async fn provider_budget_rejects_over_limit() {
    let mut settings = fake_settings();
    // 1 cent per month for the fake provider; each 5-char call costs $0.005.
    settings
        .budgets
        .provider_monthly_usd
        .insert("fake".into(), 0.01);
    let h = harness_with(
        fake_router(),
        settings,
        NewToken {
            label: "t".into(),
            ..Default::default()
        },
    )
    .await;
    let call = || h.speak(serde_json::json!({ "model": "fake/paid", "input": "hello" }));
    let (s, headers, _) = call().await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["x-x3-estimated-cost-usd"], "0.005000");
    assert_eq!(call().await.0, StatusCode::OK);
    let (s, _, body) = call().await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(json(&body)["error"]["code"], "budget_exceeded");
    // Free models are unaffected by budgets.
    assert_eq!(
        h.speak(serde_json::json!({ "input": "hello" })).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn token_cap_and_scopes() {
    let h = harness_with(
        fake_router(),
        fake_settings(),
        NewToken {
            label: "capped".into(),
            allowed_providers: Some(vec!["fake".into()]),
            allowed_tasks: Some(vec![Task::Tts]),
            spend_cap_usd: Some(0.006),
        },
    )
    .await;
    let call = || h.speak(serde_json::json!({ "model": "fake/paid", "input": "hello" }));
    assert_eq!(call().await.0, StatusCode::OK);
    let (s, _, body) = call().await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert!(json(&body)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("token 'capped'"));

    // Task scope: this token cannot transcribe.
    assert_eq!(
        h.svc
            .transcribe(
                &x3_server::Principal {
                    token_id: "x".into(),
                    label: "capped".into(),
                    allowed_providers: Some(vec!["fake".into()]),
                    allowed_tasks: Some(vec![Task::Tts]),
                    spend_cap_usd: None,
                },
                x3_server::service::TranscribeInput {
                    audio: vec![1, 2, 3],
                    filename: "a.wav".into(),
                    model: None,
                    language: None,
                    word_timestamps: false,
                },
            )
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );

    // Provider scope.
    let h2 = harness_with(
        fake_router(),
        fake_settings(),
        NewToken {
            label: "apple-only".into(),
            allowed_providers: Some(vec!["apple".into()]),
            ..Default::default()
        },
    )
    .await;
    let (s, _, _) = h2.speak(serde_json::json!({ "input": "hello" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn concurrent_calls_cannot_overshoot_budget() {
    let mut settings = fake_settings();
    settings
        .budgets
        .provider_monthly_usd
        .insert("fake".into(), 0.012);
    let h = harness_with(
        fake_router(),
        settings,
        NewToken {
            label: "t".into(),
            ..Default::default()
        },
    )
    .await;
    let calls =
        (0..6).map(|_| h.speak(serde_json::json!({ "model": "fake/paid", "input": "hello" })));
    let results = futures::future::join_all(calls).await;
    let ok = results.iter().filter(|r| r.0 == StatusCode::OK).count();
    assert_eq!(ok, 2, "only two $0.005 calls fit under $0.012");
}

#[tokio::test]
async fn usage_is_logged_without_content() {
    let h = harness().await;
    h.speak(serde_json::json!({ "input": "secret words here" }))
        .await;
    let rows = h.svc.db().recent_usage(10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["token_label"], "test");
    assert_eq!(rows[0]["model"], "fake/free");
    assert_eq!(rows[0]["units"], 17.0);
    assert!(!rows[0].to_string().contains("secret words"));
}

// ---------------------------------------------------------------- ElevenLabs end-to-end

#[tokio::test]
async fn elevenlabs_via_proxy_key_never_leaves_vault() {
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};
    const KEY: &str = "el_live_key_that_must_not_leak_1234";

    let upstream = MockServer::start().await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb"))
        .and(matchers::header("xi-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 1600]))
        .mount(&upstream)
        .await;

    let store = Arc::new(MemoryStore::new());
    store
        .set("elevenlabs", SecretString::from(KEY.to_string()))
        .unwrap();
    let store: Arc<dyn SecretStore> = store;
    let mut router = ModelRouter::new(AliasTable::default());
    router.register(Arc::new(x3_providers::ElevenLabsEngine::with_base_url(
        store,
        &upstream.uri(),
        reqwest::Client::new(),
    )));
    let h = harness_with(
        router,
        Settings::default(),
        NewToken {
            label: "claude-code".into(),
            ..Default::default()
        },
    )
    .await;

    let (s, headers, body) = h
        .speak(serde_json::json!({
            "model": "elevenlabs/eleven_multilingual_v2",
            "input": "Hello from the cloud",
        }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, vec![7u8; 1600]);
    for (_, v) in headers.iter() {
        assert!(!v.to_str().unwrap_or("").contains(KEY));
    }
    // 20 chars at the placeholder $0.30/1k
    let cost: f64 = headers["x-x3-estimated-cost-usd"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((cost - 0.006).abs() < 1e-9);

    // Nothing on disk contains the key.
    for entry in walk(h.svc.paths().data_dir.as_path()) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()),
            "key found in {}",
            entry.display()
        );
    }
    // Models listing does not reveal it either.
    let (_, _, body) = h
        .send(h.req("GET", "/v1/models").body(Body::empty()).unwrap())
        .await;
    assert!(!String::from_utf8_lossy(&body).contains(KEY));
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------- MCP over Streamable HTTP

#[tokio::test]
async fn mcp_tools_over_streamable_http() {
    use rmcp::{
        model::{CallToolRequestParams, ClientConfig},
        transport::{
            streamable_http_client::StreamableHttpClientTransportConfig,
            StreamableHttpClientTransport,
        },
        ServiceExt,
    };

    let h = harness().await;
    let handle = x3_server::start(h.svc.clone(), 0).await.unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", handle.addr.port());

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(h.token.clone()),
    );
    let client = ClientConfig::default().serve(transport).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        ["list_models", "list_voices", "play", "speak", "transcribe"]
    );
    for t in &tools {
        let blob = serde_json::to_string(t).unwrap().to_lowercase();
        assert!(!blob.contains("api_key") && !blob.contains("apikey"));
    }
    let speak = tools.iter().find(|t| t.name == "speak").unwrap();
    assert!(speak.description.as_deref().unwrap().contains("COST MONEY"));

    // speak → file path (no base64), no playback in tests.
    let args = serde_json::json!({ "text": "hello", "play": false });
    let res = client
        .call_tool(
            CallToolRequestParams::new("speak").with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(res.is_error, Some(true));
    let text = res.content[0].as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    assert!(path.is_file());
    assert_eq!(v["model"], "fake/free");
    assert_eq!(v["estimated_cost_usd"], 0.0);

    // transcribe the file we just made.
    let args = serde_json::json!({ "path": path, "word_timestamps": true });
    let res = client
        .call_tool(
            CallToolRequestParams::new("transcribe")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&res.content[0].as_text().unwrap().text).unwrap();
    assert!(v["text"].as_str().unwrap().starts_with("heard"));
    assert_eq!(v["model"], "fake/ears");

    // Disallowed extensions are refused with a tool error, not a crash.
    let args = serde_json::json!({ "path": "/etc/passwd" });
    let res = client
        .call_tool(
            CallToolRequestParams::new("transcribe")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(res.is_error, Some(true));

    // list_models includes aliases.
    let res = client
        .call_tool(CallToolRequestParams::new("list_models"))
        .await
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&res.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(v["aliases"]["tts"], "fake/free");

    let _ = client.cancel().await;

    // A bad token cannot even initialize.
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header("x3_live_wrong"),
    );
    assert!(ClientConfig::default().serve(transport).await.is_err());

    handle.shutdown().await;
}

#[test]
fn audio_format_mime() {
    assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
}
