//! HTTP client for the OpenCode v2 (/api) surface. All calls are blocking and
//! run on the worker thread, never on the render thread.

use serde_json::{json, Value};
use std::time::Duration;

pub struct Api {
    pub base: String,
    pub authorization: Option<String>,
}

impl Api {
    fn apply_auth(&self, request: ureq::Request) -> ureq::Request {
        if let Some(authorization) = &self.authorization {
            request.set("Authorization", authorization)
        } else {
            request
        }
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let response = self
            .apply_auth(ureq::get(&format!("{}{path}", self.base)))
            .timeout(Duration::from_secs(30))
            .call()
            .map_err(|error| error.to_string())?;
        let body = response.into_string().map_err(|error| error.to_string())?;
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        let response = self
            .apply_auth(ureq::post(&format!("{}{path}", self.base)))
            .timeout(Duration::from_secs(30))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|error| match error {
                ureq::Error::Status(code, response) => format!(
                    "HTTP {code}: {}",
                    response.into_string().unwrap_or_default()
                ),
                other => other.to_string(),
            })?;
        if response.status() == 204 {
            return Ok(Value::Null);
        }
        let body = response.into_string().map_err(|error| error.to_string())?;
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }

    pub fn sessions(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .get("/api/session?limit=50")?
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn session(&self, session_id: &str) -> Result<Value, String> {
        Ok(self
            .get(&format!("/api/session/{session_id}"))?
            .get("data")
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn create_session(&self, directory: &str) -> Result<Value, String> {
        self.create_session_with(directory, "build", "opencode", "big-pickle")
    }

    /// First-prompt session creation: bind the selected provider/model at
    /// birth so the initial turn runs against the user's Home selections.
    pub fn create_session_with(
        &self,
        directory: &str,
        agent: &str,
        provider: &str,
        model: &str,
    ) -> Result<Value, String> {
        let body = json!({
            "location": { "directory": directory },
            "agent": agent,
            "model": { "id": model, "providerID": provider },
        });
        Ok(self
            .post("/api/session", &body)?
            .get("data")
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Value>, String> {
        Ok(self
            .get(&format!(
                "/api/session/{session_id}/message?order=asc&limit=200"
            ))?
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn active(&self) -> Result<Vec<String>, String> {
        Ok(self
            .get("/api/session/active")?
            .get("data")
            .and_then(Value::as_object)
            .map(|data| data.keys().cloned().collect())
            .unwrap_or_default())
    }

    pub fn prompt(&self, session_id: &str, text: &str) -> Result<(), String> {
        self.post(
            &format!("/api/session/{session_id}/prompt"),
            &json!({ "prompt": { "text": text } }),
        )
        .map(|_| ())
    }

    pub fn interrupt(&self, session_id: &str) -> Result<(), String> {
        self.post(&format!("/api/session/{session_id}/interrupt"), &json!({}))
            .map(|_| ())
    }

    pub fn switch_agent(&self, session_id: &str, agent: &str) -> Result<(), String> {
        self.post(
            &format!("/api/session/{session_id}/agent"),
            &json!({ "agent": agent }),
        )
        .map(|_| ())
    }

    pub fn switch_model(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), String> {
        self.post(
            &format!("/api/session/{session_id}/model"),
            &json!({ "model": { "id": model, "providerID": provider } }),
        )
        .map(|_| ())
    }

    /// Goal state lives in v1 session metadata (`session.metadata.goal`).
    pub fn goal(&self, session_id: &str) -> Result<Option<Value>, String> {
        Ok(self
            .get(&format!("/session/{session_id}"))?
            .get("metadata")
            .and_then(|metadata| metadata.get("goal"))
            .cloned())
    }

    pub fn todos(&self, session_id: &str) -> Result<Vec<Value>, String> {
        // Todos live on the v1 surface (`/session/:id/todo`).
        Ok(self
            .get(&format!("/session/{session_id}/todo"))?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Fork out-of-workspace toggle: PATCH the v1 session permission ruleset.
    pub fn set_external_permission(&self, session_id: &str, allow: bool) -> Result<(), String> {
        let request = self
            .apply_auth(ureq::request(
                "PATCH",
                &format!("{}/session/{session_id}", self.base),
            ))
            .timeout(Duration::from_secs(30))
            .set("Content-Type", "application/json")
            .send_string(
                &json!({
                    "permission": [{
                        "permission": "external_directory",
                        "pattern": "*",
                        "action": if allow { "allow" } else { "ask" },
                    }]
                })
                .to_string(),
            );
        request.map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn agents(&self) -> Result<Vec<Value>, String> {
        // The v1 /agent list carries name/description/mode for every agent.
        let agents = self.get("/agent")?;
        Ok(agents
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|agent| {
                agent.get("hidden").and_then(Value::as_bool) != Some(true)
                    && agent.get("mode").and_then(Value::as_str) != Some("subagent")
            })
            .collect())
    }

    pub fn models(&self) -> Result<Vec<(String, String)>, String> {
        let providers = self.get("/provider")?;
        let mut models: Vec<(String, String)> = vec![];
        if let Some(all) = providers.get("all").and_then(Value::as_array) {
            for provider in all {
                let provider_id = provider
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let connected = providers
                    .get("connected")
                    .and_then(|connected| connected.get(&provider_id))
                    .is_some()
                    || provider_id == "opencode";
                if !connected {
                    continue;
                }
                if let Some(items) = provider.get("models").and_then(Value::as_object) {
                    for id in items.keys() {
                        models.push((provider_id.clone(), id.clone()));
                    }
                }
            }
        }
        models.sort();
        Ok(models)
    }
}
