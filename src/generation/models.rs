//! Asks the installed Codex CLI which models the signed-in account can use,
//! through the `model/list` request of `codex app-server`.

use super::codex::{CodexInvocation, configure_process_group};
use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Covers a cold CLI start plus a network refresh of the model catalog.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PAGES: usize = 16;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    /// The id passed as `codex exec -m`.
    pub model: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: String,
}

impl CodexModel {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.model
        } else {
            &self.display_name
        }
    }

    pub fn supports_effort(&self, effort: &str) -> bool {
        self.supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == effort)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    data: Vec<CodexModel>,
    next_cursor: Option<String>,
}

/// Blocks for up to [`LIST_TIMEOUT`]; call from a background thread.
pub(super) fn list_models(invocation: &CodexInvocation) -> Result<Vec<CodexModel>> {
    let mut command = invocation.command();
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut child = command.spawn().context("starting codex app-server")?;
    let result = child
        .stdin
        .take()
        .zip(child.stdout.take())
        .context("codex app-server has no stdio")
        .and_then(|(stdin, stdout)| {
            let (sender, lines) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            Session {
                stdin,
                lines,
                deadline: Instant::now() + LIST_TIMEOUT,
                next_id: 0,
            }
            .list_models()
        });
    let _ = child.kill();
    let _ = child.wait();
    result
}

struct Session {
    stdin: ChildStdin,
    lines: Receiver<String>,
    deadline: Instant,
    next_id: u64,
}

impl Session {
    fn list_models(&mut self) -> Result<Vec<CodexModel>> {
        self.request(
            "initialize",
            json!({ "clientInfo": { "name": "codex-image", "version": env!("CARGO_PKG_VERSION") } }),
        )?;
        self.send(json!({ "jsonrpc": "2.0", "method": "initialized" }))?;

        let mut models = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page: ModelPage = serde_json::from_value(self.request("model/list", params)?)
                .context("reading codex model list")?;
            models.extend(page.data.into_iter().filter(|model| !model.hidden));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(models)
    }

    fn send(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .and_then(|()| self.stdin.flush())
            .context("writing to codex app-server")
    }

    /// Sends one request and returns its result, skipping notifications and
    /// anything else the server streams in between.
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = self
                .lines
                .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
                .with_context(|| format!("codex app-server did not answer {method}"))?;
            let Ok(mut message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let reason = error["message"].as_str().unwrap_or("request failed");
                bail!("codex app-server {method}: {reason}");
            }
            return Ok(message["result"].take());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModelPage;

    #[test]
    fn model_page_reads_efforts_and_tolerates_missing_fields() {
        let page: ModelPage = serde_json::from_str(
            r#"{
                "data": [
                    {
                        "model": "gpt-6.1-sol",
                        "displayName": "GPT-6.1-Sol",
                        "isDefault": true,
                        "defaultReasoningEffort": "low",
                        "supportedReasoningEfforts": [
                            { "reasoningEffort": "low", "description": "Fast" },
                            { "reasoningEffort": "max" }
                        ],
                        "someFutureField": 1
                    },
                    { "model": "bare" }
                ],
                "nextCursor": null
            }"#,
        )
        .unwrap();
        let [sol, bare] = page.data.as_slice() else {
            panic!("expected two models");
        };
        assert!(sol.is_default && sol.supports_effort("max") && !sol.supports_effort("high"));
        assert_eq!(sol.default_reasoning_effort.as_deref(), Some("low"));
        assert_eq!(bare.label(), "bare");
        assert!(page.next_cursor.is_none());
    }
}
