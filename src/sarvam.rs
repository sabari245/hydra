//! Experimental speech-to-text with Sarvam, which detects the spoken language
//! and can translate it to English.

use crate::{config, wav};
use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::time::Instant;
use tokio::task::JoinSet;

const API_URL: &str = "https://api.sarvam.ai/speech-to-text";
/// The REST API takes at most 30 seconds of audio per request.
const CHUNK_SECONDS: usize = 29;

#[derive(Deserialize)]
struct Response {
    transcript: String,
    language_code: Option<String>,
}

/// Transcribes a WAV recording, in chunks sent in parallel when it is longer
/// than the API allows.
pub async fn transcribe(
    client: &reqwest::Client,
    api_key: &str,
    config: &config::Sarvam,
    wav: &[u8],
) -> Result<String> {
    let started = Instant::now();
    let chunks: Vec<&[u8]> = wav::pcm(wav)?
        .chunks(CHUNK_SECONDS * wav::BYTES_PER_SECOND)
        .collect();
    log!(
        "INFO",
        "transcription_request",
        "provider=sarvam model={} mode={} audio_bytes={} chunks={}",
        config.model,
        config.mode.as_str(),
        wav.len(),
        chunks.len()
    );
    let mut requests = JoinSet::new();
    for (index, chunk) in chunks.into_iter().enumerate() {
        let request = request(client, api_key, config, wav::encode(chunk));
        requests.spawn(async move { (index, request.await) });
    }
    let mut parts = Vec::new();
    while let Some(joined) = requests.join_next().await {
        let (index, result) = joined.context("Sarvam request task failed")?;
        parts.push((index, result?));
    }
    parts.sort_by_key(|(index, _)| *index);
    let text = parts
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    log!(
        "INFO",
        "transcription_completed",
        "provider=sarvam duration_ms={}",
        started.elapsed().as_millis()
    );
    Ok(text)
}

fn request(
    client: &reqwest::Client,
    api_key: &str,
    config: &config::Sarvam,
    wav: Vec<u8>,
) -> impl Future<Output = Result<String>> + Send + 'static {
    let request = Part::bytes(wav)
        .file_name("recording.wav")
        .mime_str("audio/wav")
        .map(|file| {
            let form = Form::new()
                .part("file", file)
                .text("model", config.model.clone())
                .text("mode", config.mode.as_str())
                // Detect the spoken language.
                .text("language_code", "unknown");
            client
                .post(API_URL)
                .header("api-subscription-key", api_key)
                .multipart(form)
        });
    async move {
        let response = request?.send().await.context("request to Sarvam failed")?;
        let status = response.status();
        log!("INFO", "sarvam_response", "status={status}");
        if !status.is_success() {
            bail!("Sarvam returned {status}: {}", response.text().await?);
        }
        let response: Response = response.json().await?;
        log!(
            "INFO",
            "sarvam_language",
            "language={}",
            response.language_code.as_deref().unwrap_or("unknown")
        );
        Ok(response.transcript)
    }
}
