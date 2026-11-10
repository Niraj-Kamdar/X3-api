use std::sync::Arc;

use wiremock::{
    matchers::{body_json, header, method, path, query_param},
    Match, Mock, MockServer, Request, ResponseTemplate,
};
use x3_core::{audio, AudioFormat, Engine, Error, ModelId, SttRequest, Task, TtsRequest};
use x3_providers::{CartesiaEngine, ElevenLabsEngine};
use x3_vault::{MemoryStore, SecretStore, SecretString};

/// Byte-level body matcher (multipart bodies with audio are not UTF-8).
struct BodyHas(&'static str);

impl Match for BodyHas {
    fn matches(&self, request: &Request) -> bool {
        let needle = self.0.as_bytes();
        request.body.windows(needle.len()).any(|w| w == needle)
    }
}

fn body_string_contains(s: &'static str) -> BodyHas {
    BodyHas(s)
}

const EL_KEY: &str = "el_test_key_abcdef_9876";
const CA_KEY: &str = "sk_car_test_key_abcd_5432";

fn store() -> Arc<dyn SecretStore> {
    let s = MemoryStore::new();
    s.set("elevenlabs", SecretString::from(EL_KEY.to_string()))
        .unwrap();
    s.set("cartesia", SecretString::from(CA_KEY.to_string()))
        .unwrap();
    Arc::new(s)
}

fn tts(text: &str, voice: Option<&str>, format: AudioFormat) -> TtsRequest {
    TtsRequest {
        text: text.into(),
        voice: voice.map(str::to_string),
        format,
        sample_rate: None,
        speed: None,
        language: None,
    }
}

fn stt(words: bool) -> SttRequest {
    SttRequest {
        audio: audio::wav_from_f32(&[0.0; 16000], 16000).unwrap(),
        filename: "clip.wav".into(),
        language: Some("en".into()),
        word_timestamps: words,
    }
}

async fn el(server: &MockServer) -> ElevenLabsEngine {
    ElevenLabsEngine::with_base_url(store(), &server.uri(), reqwest::Client::new())
}

async fn ca(server: &MockServer) -> CartesiaEngine {
    CartesiaEngine::with_base_url(store(), &server.uri(), reqwest::Client::new())
}

#[tokio::test]
async fn elevenlabs_tts_sends_expected_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb"))
        .and(query_param("output_format", "mp3_44100_128"))
        .and(header("xi-api-key", EL_KEY))
        .and(body_json(serde_json::json!({
            "text": "hello",
            "model_id": "eleven_multilingual_v2"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8; 16000]))
        .expect(1)
        .mount(&server)
        .await;

    let e = el(&server).await;
    let m = e
        .load(&ModelId::parse("elevenlabs/eleven_multilingual_v2").unwrap())
        .await
        .unwrap();
    let out = m.tts(tts("hello", None, AudioFormat::Mp3)).await.unwrap();
    assert_eq!(out.bytes.len(), 16000);
    assert_eq!(out.format, AudioFormat::Mp3);
    assert!((out.duration_secs.unwrap() - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn elevenlabs_stt_normalizes_words_and_segments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/speech-to-text"))
        .and(header("xi-api-key", EL_KEY))
        .and(body_string_contains("scribe_v2"))
        .and(body_string_contains("timestamps_granularity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "Hello there. Bye",
            "language_code": "eng",
            "language_probability": 0.98,
            "words": [
                {"text": "Hello", "start": 0.0, "end": 0.3, "type": "word"},
                {"text": " ", "start": 0.3, "end": 0.35, "type": "spacing"},
                {"text": "there.", "start": 0.35, "end": 0.7, "type": "word"},
                {"text": "(laughs)", "start": 0.7, "end": 0.9, "type": "audio_event"},
                {"text": "Bye", "start": 0.9, "end": 1.0, "type": "word"}
            ]
        })))
        .mount(&server)
        .await;

    let e = el(&server).await;
    let m = e
        .load(&ModelId::parse("elevenlabs/scribe_v2").unwrap())
        .await
        .unwrap();
    let t = m.stt(stt(true)).await.unwrap();
    assert_eq!(t.text, "Hello there. Bye");
    assert_eq!(t.language.as_deref(), Some("eng"));
    let words = t.words.unwrap();
    assert_eq!(words.len(), 3);
    assert_eq!(t.segments.len(), 2);
    assert_eq!(t.segments[0].text, "Hello there.");
    assert!((t.duration_secs.unwrap() - 1.0).abs() < 1e-6);

    let t = m.stt(stt(false)).await.unwrap();
    assert!(t.words.is_none());
}

#[tokio::test]
async fn elevenlabs_error_mapping() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/text-to-speech/a1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            serde_json::json!({"detail": {"status": "invalid_api_key", "message": "bad"}}),
        ))
        .mount(&server)
        .await;
    Mock::given(path("/v1/text-to-speech/a2"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "detail": {"status": "quota_exceeded", "message": "This request exceeds your quota"}
        })))
        .mount(&server)
        .await;
    Mock::given(path("/v1/text-to-speech/a3"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&server)
        .await;
    Mock::given(path("/v1/text-to-speech/a4"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let e = el(&server).await;
    let m = e
        .load(&ModelId::parse("elevenlabs/eleven_multilingual_v2").unwrap())
        .await
        .unwrap();
    let err = |v: &'static str| {
        let m = m.clone();
        async move {
            m.tts(tts("x", Some(v), AudioFormat::Mp3))
                .await
                .unwrap_err()
        }
    };
    assert!(matches!(err("a1").await, Error::Unauthorized { .. }));
    assert!(matches!(err("a2").await, Error::QuotaExceeded { .. }));
    assert!(matches!(
        err("a3").await,
        Error::RateLimited {
            retry_after_secs: Some(7),
            ..
        }
    ));
    match err("a4").await {
        Error::Provider {
            status, message, ..
        } => {
            assert_eq!(status, 500);
            assert!(!message.contains(EL_KEY));
        }
        other => panic!("unexpected {other:?}"),
    }
    // Invalid voice IDs never reach the network (path injection guard).
    assert!(matches!(err("../v1/user").await, Error::InvalidRequest(_)));
}

#[tokio::test]
async fn elevenlabs_missing_key() {
    let server = MockServer::start().await;
    let e = ElevenLabsEngine::with_base_url(
        Arc::new(MemoryStore::new()),
        &server.uri(),
        reqwest::Client::new(),
    );
    let m = e
        .load(&ModelId::parse("elevenlabs/eleven_multilingual_v2").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        m.tts(tts("x", None, AudioFormat::Mp3)).await,
        Err(Error::MissingKey(_))
    ));
    assert!(e.models().iter().all(|m| !m.available));
}

#[tokio::test]
async fn elevenlabs_voices_paginate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/voices"))
        .and(query_param("next_page_token", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "voices": [{"voice_id": "v2", "name": "Two", "category": "premade"}],
            "has_more": false,
            "next_page_token": null
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/voices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "voices": [{"voice_id": "v1", "name": "One", "labels": {"accent": "american"}}],
            "has_more": true,
            "next_page_token": "p2"
        })))
        .mount(&server)
        .await;
    let voices = el(&server).await.list_voices().await.unwrap();
    assert_eq!(voices.len(), 2);
    assert_eq!(voices[0].id, "v1");
    assert_eq!(voices[0].language.as_deref(), Some("american"));
    assert_eq!(voices[1].id, "v2");
}

#[tokio::test]
async fn elevenlabs_unlisted_models_pass_through() {
    let server = MockServer::start().await;
    let e = el(&server).await;
    let info = e
        .model_info(&ModelId::parse("elevenlabs/scribe_v9").unwrap())
        .unwrap();
    assert_eq!(info.task, Task::Stt);
    assert!(info.cloud);
}

#[tokio::test]
async fn cartesia_tts_sends_expected_request() {
    let server = MockServer::start().await;
    let wav = audio::wav_from_f32(&[0.0; 44100], 44100).unwrap();
    Mock::given(method("POST"))
        .and(path("/tts/bytes"))
        .and(header("authorization", format!("Bearer {CA_KEY}").as_str()))
        .and(header("cartesia-version", "2026-08-14"))
        .and(body_json(serde_json::json!({
            "model_id": "sonic-3",
            "transcript": "hello",
            "voice": {"id": "voice-123"},
            "output_format": {"container": "wav", "encoding": "pcm_s16le", "sample_rate": 44100}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(wav))
        .expect(1)
        .mount(&server)
        .await;
    let e = ca(&server).await;
    let m = e
        .load(&ModelId::parse("cartesia/sonic-3").unwrap())
        .await
        .unwrap();
    let out = m
        .tts(tts("hello", Some("voice-123"), AudioFormat::Wav))
        .await
        .unwrap();
    assert!((out.duration_secs.unwrap() - 1.0).abs() < 1e-6);

    // No voice → clear error, no request.
    assert!(matches!(
        m.tts(tts("hello", None, AudioFormat::Wav)).await,
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        m.tts(tts("hello", Some("v"), AudioFormat::Opus)).await,
        Err(Error::Unsupported(_))
    ));
}

#[tokio::test]
async fn cartesia_stt_and_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/stt"))
        .and(header("cartesia-version", "2026-08-14"))
        .and(body_string_contains("ink-whisper"))
        .and(body_string_contains("timestamp_granularities[]"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "type": "transcript",
            "request_id": "r1",
            "text": " Hi there. ",
            "language": "en",
            "duration": 1.5,
            "words": [
                {"word": "Hi", "start": 0.1, "end": 0.3},
                {"word": "there.", "start": 0.4, "end": 0.8}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(path("/voices"))
        .respond_with(ResponseTemplate::new(402).set_body_string("{\"error\":\"out of credits\"}"))
        .mount(&server)
        .await;

    let e = ca(&server).await;
    let m = e
        .load(&ModelId::parse("cartesia/ink-whisper").unwrap())
        .await
        .unwrap();
    let t = m.stt(stt(true)).await.unwrap();
    assert_eq!(t.text, "Hi there.");
    assert_eq!(t.duration_secs, Some(1.5));
    assert_eq!(t.words.as_ref().unwrap().len(), 2);
    assert_eq!(t.segments.len(), 1);

    assert!(matches!(
        e.list_voices().await,
        Err(Error::QuotaExceeded { .. })
    ));
}

#[tokio::test]
async fn cartesia_voices() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/voices"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "c1", "name": "Calm", "description": "d", "language": "en"}],
            "has_more": false,
            "next_page": null
        })))
        .mount(&server)
        .await;
    let v = ca(&server).await.list_voices().await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].provider, "cartesia");
}
