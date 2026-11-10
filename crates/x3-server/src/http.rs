//! OpenAI-compatible HTTP endpoints.

use axum::{
    extract::{Multipart, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use x3_core::{AudioFormat, Error, Task};

use crate::{
    auth::Principal,
    error::ApiError,
    service::{SpeakInput, TranscribeInput},
    AppState,
};

/// `GET /v1/models` — concrete models plus alias entries.
pub async fn list_models(
    State(state): State<AppState>,
    Query(q): Query<ModelsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task = q.task.as_deref().map(Task::parse).transpose()?;
    let mut data: Vec<serde_json::Value> = state
        .service
        .list_models(task)
        .into_iter()
        .map(|(m, aliases, pricing)| {
            serde_json::json!({
                "id": m.id,
                "object": "model",
                "owned_by": m.id.provider(),
                "task": m.task,
                "display_name": m.display_name,
                "cloud": m.cloud,
                "available": m.available,
                "aliases": aliases,
                "pricing": pricing,
            })
        })
        .collect();
    for (alias, target) in state.service.router().aliases().iter() {
        data.push(serde_json::json!({
            "id": alias,
            "object": "model",
            "owned_by": "x3",
            "alias_for": target,
        }));
    }
    Ok(Json(serde_json::json!({ "object": "list", "data": data })))
}

#[derive(Deserialize)]
pub struct ModelsQuery {
    task: Option<String>,
}

#[derive(Deserialize)]
pub struct VoicesQuery {
    provider: Option<String>,
}

/// `GET /v1/voices?provider=`
pub async fn list_voices(
    State(state): State<AppState>,
    Query(q): Query<VoicesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (voices, errors) = state.service.list_voices(q.provider.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "object": "list",
        "data": voices,
        "errors": errors,
    })))
}

/// OpenAI `POST /v1/audio/speech` body, plus x3 extensions.
#[derive(Deserialize)]
pub struct SpeechBody {
    #[serde(default)]
    model: Option<String>,
    input: String,
    #[serde(default)]
    voice: Option<String>,
    /// `mp3 | opus | wav | pcm` (OpenAI's `aac`/`flac` are rejected).
    #[serde(default)]
    response_format: Option<String>,
    #[serde(default)]
    speed: Option<f32>,
    /// x3: output sample rate.
    #[serde(default)]
    sample_rate: Option<u32>,
    /// x3: language hint.
    #[serde(default)]
    language: Option<String>,
    /// x3: `"bytes"` (default, audio in the body) or `"path"` (write to the
    /// outputs dir and return JSON with the path).
    #[serde(default)]
    response: Option<String>,
}

pub async fn speech(
    State(state): State<AppState>,
    Extension(who): Extension<Principal>,
    Json(body): Json<SpeechBody>,
) -> Result<Response, ApiError> {
    let as_path = match body.response.as_deref() {
        None | Some("bytes") => false,
        Some("path") => true,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "response must be \"bytes\" or \"path\", got \"{other}\""
            )))
        }
    };
    let format = body
        .response_format
        .as_deref()
        .map(AudioFormat::parse)
        .transpose()?;
    let out = state
        .service
        .speak(
            &who,
            SpeakInput {
                text: body.input,
                model: body.model,
                voice: body.voice,
                format,
                sample_rate: body.sample_rate,
                speed: body.speed,
                language: body.language,
                save: as_path,
            },
        )
        .await?;
    let cost = HeaderValue::from_str(&format!("{:.6}", out.estimated_cost_usd))
        .unwrap_or(HeaderValue::from_static("0"));
    let model = HeaderValue::from_str(out.model.as_str()).unwrap_or(HeaderValue::from_static(""));
    if as_path {
        let mut resp = Json(&out).into_response();
        resp.headers_mut().insert("x-x3-estimated-cost-usd", cost);
        return Ok(resp);
    }
    let mut resp = out.bytes.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(out.format.mime_type()),
    );
    h.insert("x-x3-estimated-cost-usd", cost);
    h.insert("x-x3-model", model);
    Ok(resp)
}

/// OpenAI `POST /v1/audio/transcriptions` (multipart).
pub async fn transcriptions(
    State(state): State<AppState>,
    Extension(who): Extension<Principal>,
    mut form: Multipart,
) -> Result<Response, ApiError> {
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut model = None;
    let mut language = None;
    let mut response_format = "json".to_string();
    let mut words = false;
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("audio.wav").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::bad_request(e.to_string()))?;
                file = Some((filename, bytes.to_vec()));
            }
            "model"
            | "language"
            | "response_format"
            | "timestamp_granularities[]"
            | "timestamp_granularities" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| ApiError::bad_request(e.to_string()))?;
                match name.as_str() {
                    "model" => model = Some(v),
                    "language" => language = Some(v),
                    "response_format" => response_format = v,
                    _ => words |= v.split(',').any(|g| g.trim() == "word"),
                }
            }
            _ => {} // ignore unknown fields (prompt, temperature, ...)
        }
    }
    let (filename, audio) = file.ok_or_else(|| ApiError::bad_request("missing 'file' field"))?;
    let out = state
        .service
        .transcribe(
            &who,
            TranscribeInput {
                audio,
                filename,
                model,
                language,
                word_timestamps: words,
            },
        )
        .await?;
    let t = &out.transcript;
    Ok(match response_format.as_str() {
        "json" => Json(serde_json::json!({ "text": t.text })).into_response(),
        "text" => t.text.clone().into_response(),
        "verbose_json" => Json(serde_json::json!({
            "task": "transcribe",
            "language": t.language,
            "duration": t.duration_secs,
            "text": t.text,
            "segments": t.segments.iter().enumerate().map(|(i, s)| serde_json::json!({
                "id": i, "start": s.start, "end": s.end, "text": s.text
            })).collect::<Vec<_>>(),
            "words": t.words,
            "model": out.model,
            "estimated_cost_usd": out.estimated_cost_usd,
        }))
        .into_response(),
        other => {
            return Err(Error::Unsupported(format!(
                "response_format '{other}' (supported: json, text, verbose_json)"
            ))
            .into())
        }
    })
}
