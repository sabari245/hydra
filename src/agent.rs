//! Computer-control agent: a tool-calling loop over the IsoQuant chat API.
//! The loop follows Anthropic's computer-use reference implementation
//! (anthropics/anthropic-quickstarts, computer_use/loop.py, MIT): call the
//! model, run every tool call it makes, return the results, and repeat until
//! it answers without tool calls, keeping only the newest screenshots.

use crate::{
    config, screenshot,
    tools::{ToolResult, Toolbox, home, memory::Memory},
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    env,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const KEEP_SCREENSHOTS: usize = 2;
const MAX_ACTION: usize = 200;
const NOTIFY_MS: u32 = 3000;

pub struct Settings<'a> {
    pub api_key: &'a str,
    pub chat_url: &'a str,
    pub timeout: Duration,
    pub model: &'a str,
    pub computer: &'a config::Computer,
    pub memory: Option<&'a Memory>,
}

pub struct Outcome {
    pub summary: String,
    pub actions: Vec<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: AssistantMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: Option<String>,
    /// Null or missing when the model answers without tools.
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ToolCall {
    id: String,
    function: FunctionCall,
}

#[derive(Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

fn one_line(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…", &flat[..index]),
        None => flat,
    }
}

/// Describes the desktop so the model can act without exploring first.
async fn environment() -> String {
    let monitors = tokio::task::spawn_blocking(screenshot::monitors)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|monitors| monitors);
    let focused_window = Command::new("niri")
        .args(["msg", "--json", "focused-window"])
        .output()
        .ok()
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .map(|window| {
            format!(
                "{:?} (app_id {:?})",
                window.get("title").and_then(Value::as_str).unwrap_or(""),
                window.get("app_id").and_then(Value::as_str).unwrap_or("")
            )
        });
    let date = Command::new("date")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();

    let mut text = format!(
        "\n\nEnvironment:\n- Date: {date}\n- User: {}, home {}\n- Session: {}, compositor: {}",
        env::var("USER").unwrap_or_default(),
        home().display(),
        env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        if env::var_os("NIRI_SOCKET").is_some() {
            "niri (control windows with `niri msg action ...`; `niri msg --json windows` lists them)"
        } else {
            "unknown"
        },
    );
    match monitors {
        Ok(monitors) => {
            for monitor in monitors {
                text.push_str(&format!(
                    "\n- Monitor {} ({}): {}x{} at ({}, {})",
                    monitor.name,
                    monitor.description,
                    monitor.width,
                    monitor.height,
                    monitor.x,
                    monitor.y
                ));
            }
        }
        Err(error) => text.push_str(&format!("\n- Monitors unavailable: {error:#}")),
    }
    if let Some(window) = focused_window {
        text.push_str(&format!("\n- Focused window: {window}"));
    }
    text
}

/// Replaces all but the newest screenshots with a note to keep requests small.
fn prune_screenshots(messages: &mut [Value]) {
    let mut seen = 0;
    for message in messages.iter_mut().rev() {
        let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            if part.get("type").and_then(Value::as_str) == Some("image_url") {
                seen += 1;
                if seen > KEEP_SCREENSHOTS {
                    *part = json!({ "type": "text", "text": "[older screenshot removed]" });
                }
            }
        }
    }
}

pub fn notify(summary: &str, body: &str, replace: Option<&str>) -> Option<String> {
    let mut command = Command::new("notify-send");
    // Short-lived and kept out of the notification history.
    command.args([
        "--app-name=Hydra STT",
        "--print-id",
        "--urgency=low",
        "--hint=int:transient:1",
    ]);
    command.arg(format!("--expire-time={}", NOTIFY_MS));
    if let Some(id) = replace {
        command.arg(format!("--replace-id={id}"));
    }
    let output = command
        .arg(summary)
        .arg(body)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|id| !id.is_empty())
}

pub async fn run(
    client: &reqwest::Client,
    settings: &Settings<'_>,
    system_prompt: &str,
    request: &str,
) -> Result<Outcome> {
    let started = Instant::now();
    let computer = settings.computer;
    let notification = if computer.notify {
        notify("Working on it", &one_line(request, 200), None)
    } else {
        None
    };
    log!(
        "INFO",
        "agent_started",
        "model={} max_steps={} request={request:?}",
        settings.model,
        computer.max_steps
    );

    let mut toolbox = Toolbox::new(computer, settings.memory);
    let tools = toolbox.definitions();
    let system = format!("{system_prompt}{}", environment().await);
    let mut messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": request }),
    ];
    let mut actions = Vec::new();
    let mut summary = None;

    for step in 1..=computer.max_steps {
        prune_screenshots(&mut messages);
        let response = client
            .post(settings.chat_url)
            .bearer_auth(settings.api_key)
            .timeout(settings.timeout * 2)
            .json(&json!({
                "model": settings.model,
                "messages": messages,
                "tools": tools,
                "tool_choice": "auto",
                "temperature": 0.2,
                "reasoning_effort": "low",
                "include_reasoning": false,
                "max_tokens": 4096,
            }))
            .send()
            .await
            .context("request to IsoQuant failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("IsoQuant returned {status}: {}", response.text().await?);
        }
        let choice = response
            .json::<ChatResponse>()
            .await
            .context("invalid IsoQuant chat response")?
            .choices
            .into_iter()
            .next()
            .context("IsoQuant returned no choices")?;
        let message = choice.message;
        let tool_calls = message.tool_calls.unwrap_or_default();
        log!(
            "INFO",
            "agent_step",
            "step={step} finish_reason={:?} tool_calls={} text={:?}",
            choice.finish_reason,
            tool_calls.len(),
            message.content.as_deref().unwrap_or("")
        );
        if tool_calls.is_empty() {
            summary = Some(
                message
                    .content
                    .map(|content| content.trim().to_owned())
                    .filter(|content| !content.is_empty())
                    .unwrap_or_else(|| "Done.".to_owned()),
            );
            break;
        }

        messages.push(json!({
            "role": "assistant",
            "content": message.content.unwrap_or_default(),
            "tool_calls": tool_calls.iter().map(|call| json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.function.name, "arguments": call.function.arguments },
            })).collect::<Vec<_>>(),
        }));
        for call in &tool_calls {
            let name = call.function.name.as_str();
            actions.push(one_line(
                &format!("{name} {}", call.function.arguments),
                MAX_ACTION,
            ));
            let tool_started = Instant::now();
            let result = match serde_json::from_str::<Value>(&call.function.arguments) {
                Ok(arguments) => toolbox.execute(name, &arguments).await,
                Err(error) => Err(anyhow::anyhow!("arguments are not valid JSON: {error}")),
            };
            let (content, logged) = match result {
                Ok(ToolResult::Text(text)) => (json!(text), one_line(&text, 500)),
                Ok(ToolResult::Image(description, shot)) => (
                    json!([
                        { "type": "text", "text": description },
                        { "type": "image_url", "image_url": { "url": shot.data_url() } },
                    ]),
                    description,
                ),
                Err(error) => {
                    let text = format!("error: {error:#}");
                    (json!(text), one_line(&text, 500))
                }
            };
            log!(
                "INFO",
                "agent_tool",
                "step={step} tool={name} duration_ms={} arguments={} result={logged:?}",
                tool_started.elapsed().as_millis(),
                one_line(&call.function.arguments, 500),
            );
            messages.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
        }
    }

    let summary = summary.unwrap_or_else(|| {
        format!(
            "Stopped after {} steps without finishing.",
            computer.max_steps
        )
    });
    log!(
        "INFO",
        "agent_completed",
        "tool_calls={} duration_ms={} summary={summary:?}",
        actions.len(),
        started.elapsed().as_millis()
    );
    if computer.notify {
        notify("Done", &summary, notification.as_deref());
    }
    Ok(Outcome { summary, actions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_screenshots_are_kept() {
        let image = || json!({ "role": "tool", "content": [{ "type": "image_url" }] });
        let mut messages = vec![
            image(),
            image(),
            json!({ "role": "tool", "content": "x" }),
            image(),
        ];
        prune_screenshots(&mut messages);
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][0]["type"], "image_url");
        assert_eq!(messages[3]["content"][0]["type"], "image_url");
    }
}
