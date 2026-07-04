use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const PRIORITY: &[&str] = &["gpt-5", "claude-sonnet-4", "big-pickle", "gemini-3-pro"];

pub fn list(config: &Value) -> Value {
    let all = filtered_catalog(config);
    let connected = connected(config);
    let mut providers = all.clone();
    for (id, provider) in &connected {
        providers.insert(id.clone(), provider.clone());
    }
    json!({
        "all": providers.values().cloned().collect::<Vec<_>>(),
        "default": defaults(&providers),
        "connected": connected.keys().cloned().collect::<Vec<_>>(),
    })
}

pub fn config_providers(config: &Value) -> Value {
    let providers = connected(config);
    json!({
        "providers": providers.values().cloned().collect::<Vec<_>>(),
        "default": defaults(&providers),
    })
}

pub fn auth_methods(config: &Value) -> Value {
    let methods = connected(config)
        .into_iter()
        .map(|(id, provider)| {
            (
                id,
                json!({
                    "methods": api_key_methods(&provider),
                }),
            )
        })
        .collect::<Map<_, _>>();
    Value::Object(methods)
}

fn filtered_catalog(config: &Value) -> BTreeMap<String, Value> {
    let disabled = string_set(config.get("disabled_providers"));
    let enabled = optional_string_set(config.get("enabled_providers"));
    catalog()
        .into_iter()
        .filter(|(id, _)| {
            enabled.as_ref().is_none_or(|items| items.contains(id)) && !disabled.contains(id)
        })
        .collect()
}

fn connected(config: &Value) -> BTreeMap<String, Value> {
    let disabled = string_set(config.get("disabled_providers"));
    let mut providers = BTreeMap::new();
    let catalog = catalog();

    for (id, provider) in configured(config, &catalog) {
        if !disabled.contains(&id) {
            providers.insert(id, provider);
        }
    }

    if !disabled.contains("opencode") {
        if let Some(provider) = opencode_provider(&catalog) {
            providers.insert("opencode".into(), provider);
        }
    }

    for (id, mut provider) in catalog {
        if disabled.contains(&id) {
            continue;
        }
        let Some(key) = provider
            .get("env")
            .and_then(Value::as_array)
            .and_then(|env| {
                env.iter()
                    .filter_map(Value::as_str)
                    .find_map(|name| std::env::var(name).ok())
            })
        else {
            continue;
        };
        provider["source"] = Value::String("env".into());
        if provider
            .get("env")
            .and_then(Value::as_array)
            .is_some_and(|env| env.len() == 1)
        {
            provider["key"] = Value::String(key);
        }
        providers.insert(id, provider);
    }

    providers
}

fn opencode_provider(catalog: &BTreeMap<String, Value>) -> Option<Value> {
    let mut provider = catalog.get("opencode")?.clone();
    let keep = [
        "mimo-v2.5-free",
        "nemotron-3-ultra-free",
        "deepseek-v4-flash-free",
        "north-mini-code-free",
        "big-pickle",
    ];
    let models = keep
        .into_iter()
        .filter_map(|id| {
            provider
                .get("models")
                .and_then(Value::as_object)
                .and_then(|models| models.get(id))
                .cloned()
                .map(|model| (id.to_string(), model))
        })
        .collect::<Map<_, _>>();
    provider["models"] = Value::Object(models);
    provider["options"] = json!({ "apiKey": "public" });
    Some(provider)
}

fn configured(config: &Value, catalog: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let Some(entries) = config.get("provider").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    entries
        .iter()
        .map(|(id, value)| {
            let base = catalog.get(id);
            let mut provider = json!({
                "id": id,
                "name": value.get("name").or_else(|| base.and_then(|item| item.get("name"))).and_then(Value::as_str).unwrap_or(id),
                "source": "config",
                "env": value.get("env").or_else(|| base.and_then(|item| item.get("env"))).cloned().unwrap_or_else(|| Value::Array(vec![])),
                "options": crate::config::merge(base.and_then(|item| item.get("options")).cloned().unwrap_or_else(object), value.get("options").cloned().unwrap_or_else(object)),
                "models": base.and_then(|item| item.get("models")).cloned().unwrap_or_else(object),
            });
            if let Some(models) = value.get("models").and_then(Value::as_object) {
                for (model_id, model) in models {
                    provider["models"][model_id] = configured_model(id, model_id, model, base);
                }
            }
            (id.clone(), provider)
        })
        .collect()
}

fn configured_model(
    provider_id: &str,
    model_id: &str,
    model: &Value,
    base: Option<&Value>,
) -> Value {
    let existing = base
        .and_then(|provider| provider.get("models"))
        .and_then(|models| models.get(model.get("id").and_then(Value::as_str).unwrap_or(model_id)));
    let api_id = model
        .get("id")
        .or_else(|| {
            existing
                .and_then(|item| item.get("api"))
                .and_then(|api| api.get("id"))
        })
        .and_then(Value::as_str)
        .unwrap_or(model_id);
    let api_npm = model
        .get("provider")
        .and_then(|item| item.get("npm"))
        .or_else(|| {
            existing
                .and_then(|item| item.get("api"))
                .and_then(|api| api.get("npm"))
        })
        .and_then(Value::as_str)
        .unwrap_or("@ai-sdk/openai-compatible");
    json!({
        "id": model_id,
        "providerID": provider_id,
        "name": model.get("name").and_then(Value::as_str).unwrap_or(model_id),
        "api": {
            "id": api_id,
            "url": model.get("provider").and_then(|item| item.get("api")).or_else(|| existing.and_then(|item| item.get("api")).and_then(|api| api.get("url"))).and_then(Value::as_str).unwrap_or(""),
            "npm": api_npm,
        },
        "status": model.get("status").and_then(Value::as_str).unwrap_or("active"),
        "headers": model.get("headers").cloned().unwrap_or_else(object),
        "options": model.get("options").cloned().unwrap_or_else(object),
        "cost": {
            "input": model.get("cost").and_then(|cost| cost.get("input")).and_then(Value::as_f64).unwrap_or(0.0),
            "output": model.get("cost").and_then(|cost| cost.get("output")).and_then(Value::as_f64).unwrap_or(0.0),
            "cache": {
                "read": model.get("cost").and_then(|cost| cost.get("cache_read")).and_then(Value::as_f64).unwrap_or(0.0),
                "write": model.get("cost").and_then(|cost| cost.get("cache_write")).and_then(Value::as_f64).unwrap_or(0.0),
            }
        },
        "limit": {
            "context": model.get("limit").and_then(|limit| limit.get("context")).and_then(Value::as_f64).unwrap_or(0.0),
            "output": model.get("limit").and_then(|limit| limit.get("output")).and_then(Value::as_f64).unwrap_or(0.0),
        },
        "capabilities": {
            "temperature": model.get("temperature").and_then(Value::as_bool).unwrap_or(false),
            "reasoning": model.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
            "attachment": model.get("attachment").and_then(Value::as_bool).unwrap_or(false),
            "toolcall": model.get("tool_call").and_then(Value::as_bool).unwrap_or(true),
            "input": modalities(model, "input", true),
            "output": modalities(model, "output", true),
            "interleaved": model.get("interleaved").cloned().unwrap_or(Value::Bool(false)),
        },
        "release_date": model.get("release_date").and_then(Value::as_str).unwrap_or(""),
        "family": model.get("family").and_then(Value::as_str).unwrap_or(""),
        "variants": object(),
    })
}

fn catalog() -> BTreeMap<String, Value> {
    std::fs::read_to_string(models_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|(id, provider)| (id, from_models_dev_provider(&provider)))
        .collect()
}

fn models_path() -> String {
    std::env::var("OPENCODE_MODELS_PATH").unwrap_or_else(|_| {
        Path::new(&crate::config::paths().cache)
            .join("models.json")
            .to_string_lossy()
            .into_owned()
    })
}

fn from_models_dev_provider(provider: &Value) -> Value {
    let provider_id = provider
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let models = provider
        .get("models")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .flat_map(|(key, model)| {
                    let mut values = vec![(
                        key.clone(),
                        from_models_dev_model(
                            provider,
                            model,
                            model.get("id").and_then(Value::as_str).unwrap_or(key),
                        ),
                    )];
                    if let Some(modes) = model
                        .get("experimental")
                        .and_then(|item| item.get("modes"))
                        .and_then(Value::as_object)
                    {
                        values.extend(modes.iter().map(|(mode, opts)| {
                            let id = format!(
                                "{}-{mode}",
                                model.get("id").and_then(Value::as_str).unwrap_or(key)
                            );
                            let mut value = from_models_dev_model(provider, model, &id);
                            value["name"] = Value::String(format!(
                                "{} {}{}",
                                model.get("name").and_then(Value::as_str).unwrap_or(key),
                                mode.chars().next().unwrap_or_default().to_ascii_uppercase(),
                                mode.get(1..).unwrap_or_default()
                            ));
                            if let Some(cost) = opts.get("cost") {
                                value["cost"] = model_cost(Some(cost));
                            }
                            if let Some(body) =
                                opts.get("provider").and_then(|item| item.get("body"))
                            {
                                value["options"] = camelize_object(body);
                            }
                            if let Some(headers) =
                                opts.get("provider").and_then(|item| item.get("headers"))
                            {
                                value["headers"] = headers.clone();
                            }
                            (id, value)
                        }));
                    }
                    values
                })
                .collect::<Map<_, _>>()
        })
        .unwrap_or_default();
    json!({
        "id": provider_id,
        "source": "custom",
        "name": provider.get("name").and_then(Value::as_str).unwrap_or(provider_id),
        "env": provider.get("env").cloned().unwrap_or_else(|| Value::Array(vec![])),
        "options": object(),
        "models": models,
    })
}

fn from_models_dev_model(provider: &Value, model: &Value, id: &str) -> Value {
    let provider_id = provider
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "id": id,
        "providerID": provider_id,
        "name": model.get("name").and_then(Value::as_str).unwrap_or(id),
        "family": model.get("family").cloned().unwrap_or(Value::Null),
        "api": {
            "id": model.get("id").and_then(Value::as_str).unwrap_or(id),
            "url": model.get("provider").and_then(|item| item.get("api")).or_else(|| provider.get("api")).and_then(Value::as_str).unwrap_or(""),
            "npm": model.get("provider").and_then(|item| item.get("npm")).or_else(|| provider.get("npm")).and_then(Value::as_str).unwrap_or("@ai-sdk/openai-compatible"),
        },
        "status": model.get("status").and_then(Value::as_str).unwrap_or("active"),
        "headers": object(),
        "options": object(),
        "cost": model_cost(model.get("cost")),
        "limit": {
            "context": model.get("limit").and_then(|limit| limit.get("context")).cloned().unwrap_or(Value::Number(0.into())),
            "input": model.get("limit").and_then(|limit| limit.get("input")).cloned().unwrap_or(Value::Null),
            "output": model.get("limit").and_then(|limit| limit.get("output")).cloned().unwrap_or(Value::Number(0.into())),
        },
        "capabilities": {
            "temperature": model.get("temperature").and_then(Value::as_bool).unwrap_or(false),
            "reasoning": model.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
            "attachment": model.get("attachment").and_then(Value::as_bool).unwrap_or(false),
            "toolcall": model.get("tool_call").and_then(Value::as_bool).unwrap_or(true),
            "input": modalities(model, "input", false),
            "output": modalities(model, "output", false),
            "interleaved": model.get("interleaved").cloned().unwrap_or(Value::Bool(false)),
        },
        "release_date": model.get("release_date").and_then(Value::as_str).unwrap_or(""),
        "variants": object(),
    })
}

fn model_cost(cost: Option<&Value>) -> Value {
    let cost = cost.unwrap_or(&Value::Null);
    let mut result = json!({
        "input": cost.get("input").cloned().unwrap_or(Value::Number(0.into())),
        "output": cost.get("output").cloned().unwrap_or(Value::Number(0.into())),
        "cache": {
            "read": cost.get("cache_read").cloned().unwrap_or(Value::Number(0.into())),
            "write": cost.get("cache_write").cloned().unwrap_or(Value::Number(0.into())),
        },
    });
    if let Some(tiers) = cost.get("tiers") {
        result["tiers"] = tiers.clone();
    }
    if let Some(over) = cost.get("context_over_200k") {
        result["experimentalOver200K"] = json!({
            "input": over.get("input").cloned().unwrap_or(Value::Number(0.into())),
            "output": over.get("output").cloned().unwrap_or(Value::Number(0.into())),
            "cache": {
                "read": over.get("cache_read").cloned().unwrap_or(Value::Number(0.into())),
                "write": over.get("cache_write").cloned().unwrap_or(Value::Number(0.into())),
            },
        });
    }
    result
}

fn modalities(model: &Value, side: &str, default: bool) -> Value {
    let has = |name: &str| {
        model
            .get("modalities")
            .and_then(|item| item.get(side))
            .and_then(Value::as_array)
            .map(|items| items.iter().any(|item| item.as_str() == Some(name)))
            .unwrap_or(default)
    };
    json!({
        "text": has("text"),
        "audio": has("audio"),
        "image": has("image"),
        "video": has("video"),
        "pdf": has("pdf"),
    })
}

fn defaults(providers: &BTreeMap<String, Value>) -> Value {
    Value::Object(
        providers
            .iter()
            .filter_map(|(id, provider)| {
                let models = provider.get("models")?.as_object()?;
                let best = sort_models(models).first()?.clone();
                Some((id.clone(), Value::String(best)))
            })
            .collect(),
    )
}

fn sort_models(models: &Map<String, Value>) -> Vec<String> {
    let mut ids = models.keys().cloned().collect::<Vec<_>>();
    ids.sort_by(|a, b| {
        let rank = |id: &String| {
            PRIORITY
                .iter()
                .position(|filter| id.contains(filter))
                .map(|index| index as isize)
                .unwrap_or(-1)
        };
        let latest = |id: &String| if id.contains("latest") { 0 } else { 1 };
        rank(b)
            .cmp(&rank(a))
            .then_with(|| latest(a).cmp(&latest(b)))
            .then_with(|| b.cmp(a))
    });
    ids
}

fn string_set(value: Option<&Value>) -> std::collections::BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn optional_string_set(value: Option<&Value>) -> Option<std::collections::BTreeSet<String>> {
    value.map(|value| string_set(Some(value)))
}

fn api_key_methods(provider: &Value) -> Vec<Value> {
    provider
        .get("env")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|name| json!({ "type": "api", "key": name }))
                .collect()
        })
        .unwrap_or_default()
}

fn camelize_object(value: &Value) -> Value {
    Value::Object(
        value
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (camelize(key), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn camelize(key: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for ch in key.chars() {
        if ch == '_' {
            upper = true;
            continue;
        }
        if upper {
            result.push(ch.to_ascii_uppercase());
            upper = false;
            continue;
        }
        result.push(ch);
    }
    result
}

fn object() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use super::sort_models;
    use serde_json::{json, Map};

    #[test]
    fn defaults_follow_provider_priority() {
        let models: Map<String, serde_json::Value> = serde_json::from_value(json!({
            "abc": {},
            "gpt-5-chat-latest": {},
            "gpt-5": {}
        }))
        .expect("models");
        assert_eq!(sort_models(&models)[0], "gpt-5-chat-latest");
    }
}
