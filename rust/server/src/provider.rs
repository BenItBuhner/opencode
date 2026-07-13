use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const PRIORITY: &[&str] = &["gpt-5", "claude-sonnet-4", "big-pickle", "gemini-3-pro"];
const OPENCODE_FREE_MODELS: &[&str] = &[
    "mimo-v2.5-free",
    "nemotron-3-ultra-free",
    "deepseek-v4-flash-free",
    "north-mini-code-free",
    "hy3-free",
    "big-pickle",
];

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
    let models = OPENCODE_FREE_MODELS
        .iter()
        .copied()
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
    models_dev_catalog()
        .into_iter()
        .map(|(id, provider)| (id, from_models_dev_provider(&provider)))
        .collect()
}

fn models_dev_catalog() -> BTreeMap<String, Value> {
    std::fs::read_to_string(models_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
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

pub fn official_providers(config: &Value) -> Vec<Value> {
    official_active_catalog(config)
        .into_values()
        .map(|record| record.provider)
        .collect()
}

pub fn official_provider(config: &Value, provider_id: &str) -> Option<Value> {
    official_catalog(config)
        .remove(provider_id)
        .map(|record| record.provider)
}

pub fn official_models(config: &Value) -> Vec<Value> {
    let mut items = official_active_catalog(config)
        .into_values()
        .flat_map(|record| {
            record
                .models
                .into_values()
                .map(move |model| project_model(model, &record.provider))
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .pointer("/time/released")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(
                &left
                    .pointer("/time/released")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            )
    });
    items
}

struct OfficialRecord {
    provider: Value,
    models: Map<String, Value>,
}

fn official_active_catalog(config: &Value) -> BTreeMap<String, OfficialRecord> {
    let active = connected(config);
    official_catalog(config)
        .into_iter()
        .filter(|(id, _)| active.contains_key(id))
        .collect()
}

fn official_catalog(config: &Value) -> BTreeMap<String, OfficialRecord> {
    let disabled = string_set(config.get("disabled_providers"));
    let enabled = optional_string_set(config.get("enabled_providers"));
    let mut result = models_dev_catalog()
        .into_iter()
        .filter(|(id, _)| {
            enabled.as_ref().is_none_or(|items| items.contains(id)) && !disabled.contains(id)
        })
        .map(|(id, provider)| {
            let mut record = official_record_from_models_dev(&provider);
            if id == "opencode" && std::env::var("OPENCODE_API_KEY").is_err() {
                record.models.retain(|model_id, _| {
                    model_id.ends_with("-free")
                        || matches!(model_id.as_str(), "big-pickle" | "grok-code")
                });
                record.provider["request"]["body"]["apiKey"] = Value::String("public".into());
            }
            (id, record)
        })
        .collect::<BTreeMap<_, _>>();
    apply_configured_providers(&mut result, config.get("providers"));
    apply_configured_providers(&mut result, config.get("provider"));
    result
}

fn official_record_from_models_dev(provider: &Value) -> OfficialRecord {
    let provider_id = provider
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    OfficialRecord {
        provider: json!({
            "id": provider_id,
            "name": provider.get("name").and_then(Value::as_str).unwrap_or(provider_id),
            "api": official_api(provider.get("npm"), provider.get("api"), None),
            "request": { "headers": {}, "body": {} },
        }),
        models: provider
            .get("models")
            .and_then(Value::as_object)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|(key, model)| official_model_entries(provider, key, model))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn official_model_entries(provider: &Value, key: &str, model: &Value) -> Vec<(String, Value)> {
    let base_id = model.get("id").and_then(Value::as_str).unwrap_or(key);
    let mut items = vec![(
        key.to_string(),
        official_model_from_models_dev(
            provider,
            model,
            base_id,
            model.get("name").and_then(Value::as_str).unwrap_or(key),
            model.get("cost"),
            None,
        ),
    )];
    if let Some(modes) = model
        .get("experimental")
        .and_then(|item| item.get("modes"))
        .and_then(Value::as_object)
    {
        items.extend(modes.iter().map(|(mode, opts)| {
            let id = format!("{base_id}-{mode}");
            (
                id.clone(),
                official_model_from_models_dev(
                    provider,
                    model,
                    &id,
                    &format!(
                        "{} {}{}",
                        model.get("name").and_then(Value::as_str).unwrap_or(key),
                        mode.chars().next().unwrap_or_default().to_ascii_uppercase(),
                        mode.get(1..).unwrap_or_default()
                    ),
                    opts.get("cost").or_else(|| model.get("cost")),
                    opts.get("provider"),
                ),
            )
        }));
    }
    items
}

fn official_model_from_models_dev(
    provider: &Value,
    model: &Value,
    id: &str,
    name: &str,
    cost: Option<&Value>,
    request: Option<&Value>,
) -> Value {
    let provider_id = provider
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model_provider = model.get("provider");
    let mut result = json!({
        "id": id,
        "providerID": provider_id,
        "family": model.get("family").cloned().unwrap_or(Value::Null),
        "name": name,
        "api": official_api(
            model_provider.and_then(|item| item.get("npm")).or_else(|| provider.get("npm")),
            model_provider.and_then(|item| item.get("api")).or_else(|| provider.get("api")),
            Some(model.get("id").and_then(Value::as_str).unwrap_or(id)),
        ),
        "capabilities": {
            "tools": model.get("tool_call").and_then(Value::as_bool).unwrap_or(true),
            "input": model.pointer("/modalities/input").cloned().unwrap_or_else(|| Value::Array(vec![])),
            "output": model.pointer("/modalities/output").cloned().unwrap_or_else(|| Value::Array(vec![])),
        },
        "request": {
            "headers": request.and_then(|item| item.get("headers")).cloned().unwrap_or_else(object),
            "body": request.and_then(|item| item.get("body")).map(camelize_object).unwrap_or_else(object),
        },
        "variants": [],
        "time": { "released": model.get("release_date").and_then(Value::as_str).map(released).unwrap_or(0) },
        "cost": official_cost(cost),
        "status": model.get("status").and_then(Value::as_str).unwrap_or("active"),
        "enabled": true,
        "limit": {
            "context": model.pointer("/limit/context").cloned().unwrap_or(Value::Number(0.into())),
            "input": model.pointer("/limit/input").cloned().unwrap_or(Value::Null),
            "output": model.pointer("/limit/output").cloned().unwrap_or(Value::Number(0.into())),
        },
    });
    if result["limit"]["input"].is_null() {
        result["limit"]
            .as_object_mut()
            .expect("limit object")
            .remove("input");
    }
    result
}

fn apply_configured_providers(
    records: &mut BTreeMap<String, OfficialRecord>,
    value: Option<&Value>,
) {
    let Some(providers) = value.and_then(Value::as_object) else {
        return;
    };
    for (provider_id, provider) in providers {
        let record = records
            .entry(provider_id.clone())
            .or_insert_with(|| empty_official_record(provider_id));
        if let Some(name) = provider.get("name").and_then(Value::as_str) {
            record.provider["name"] = Value::String(name.into());
        }
        if let Some(api) = provider.get("api").filter(|value| value.is_object()) {
            record.provider["api"] = api.clone();
        }
        if let Some(request) = provider.get("request") {
            merge_request(&mut record.provider["request"], request);
        }
        if let Some(models) = provider.get("models").and_then(Value::as_object) {
            for (model_id, model) in models {
                let current = record
                    .models
                    .entry(model_id.clone())
                    .or_insert_with(|| empty_official_model(provider_id, model_id));
                apply_configured_model(current, model);
            }
        }
    }
}

fn apply_configured_model(target: &mut Value, model: &Value) {
    for field in ["family", "name", "capabilities", "variants"] {
        if let Some(value) = model.get(field) {
            target[field] = value.clone();
        }
    }
    if let Some(api) = model.get("api").filter(|value| value.is_object()) {
        target["api"] = crate::config::merge(target["api"].clone(), api.clone());
    }
    if let Some(request) = model.get("request") {
        merge_request(&mut target["request"], request);
        if let Some(variant) = request.get("variant") {
            target["request"]["variant"] = variant.clone();
        }
    }
    if let Some(cost) = model.get("cost") {
        target["cost"] = if cost.is_array() {
            cost.clone()
        } else {
            Value::Array(vec![cost.clone()])
        };
    }
    if let Some(disabled) = model.get("disabled").and_then(Value::as_bool) {
        target["enabled"] = Value::Bool(!disabled);
    }
    if let Some(limit) = model.get("limit") {
        target["limit"] = crate::config::merge(target["limit"].clone(), limit.clone());
    }
}

fn empty_official_record(provider_id: &str) -> OfficialRecord {
    OfficialRecord {
        provider: json!({
            "id": provider_id,
            "name": provider_id,
            "api": { "type": "native", "settings": {} },
            "request": { "headers": {}, "body": {} },
        }),
        models: Map::new(),
    }
}

fn empty_official_model(provider_id: &str, model_id: &str) -> Value {
    json!({
        "id": model_id,
        "providerID": provider_id,
        "name": model_id,
        "api": { "id": model_id, "type": "native", "settings": {} },
        "capabilities": { "tools": false, "input": [], "output": [] },
        "request": { "headers": {}, "body": {} },
        "variants": [],
        "time": { "released": 0 },
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": { "context": 0, "output": 0 },
    })
}

fn official_api(npm: Option<&Value>, api: Option<&Value>, id: Option<&str>) -> Value {
    let mut result = Map::new();
    if let Some(id) = id {
        result.insert("id".into(), Value::String(id.into()));
    }
    if let Some(package) = npm.and_then(Value::as_str) {
        result.insert("type".into(), Value::String("aisdk".into()));
        result.insert("package".into(), Value::String(package.into()));
        if let Some(url) = api
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            result.insert("url".into(), Value::String(url.into()));
        }
        if package == "@ai-sdk/anthropic" {
            result.insert("settings".into(), object());
        }
        return Value::Object(result);
    }
    result.insert("type".into(), Value::String("native".into()));
    if let Some(url) = api
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        result.insert("url".into(), Value::String(url.into()));
    }
    result.insert("settings".into(), object());
    Value::Object(result)
}

fn official_cost(cost: Option<&Value>) -> Value {
    let cost = cost.unwrap_or(&Value::Null);
    let base = json!({
        "input": cost.get("input").cloned().unwrap_or(Value::Number(0.into())),
        "output": cost.get("output").cloned().unwrap_or(Value::Number(0.into())),
        "cache": {
            "read": cost.get("cache_read").cloned().unwrap_or(Value::Number(0.into())),
            "write": cost.get("cache_write").cloned().unwrap_or(Value::Number(0.into())),
        },
    });
    let tiers = cost
        .get("tiers")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(official_cost_tier).collect::<Vec<_>>())
        .unwrap_or_default();
    let over = cost
        .get("context_over_200k")
        .map(|item| official_cost_tier_with_context(item, 200_000));
    Value::Array(std::iter::once(base).chain(tiers).chain(over).collect())
}

fn official_cost_tier(item: &Value) -> Value {
    json!({
        "tier": item.get("tier").cloned().unwrap_or(Value::Null),
        "input": item.get("input").cloned().unwrap_or(Value::Number(0.into())),
        "output": item.get("output").cloned().unwrap_or(Value::Number(0.into())),
        "cache": {
            "read": item.get("cache_read").cloned().unwrap_or(Value::Number(0.into())),
            "write": item.get("cache_write").cloned().unwrap_or(Value::Number(0.into())),
        },
    })
}

fn official_cost_tier_with_context(item: &Value, size: i64) -> Value {
    json!({
        "tier": { "type": "context", "size": size },
        "input": item.get("input").cloned().unwrap_or(Value::Number(0.into())),
        "output": item.get("output").cloned().unwrap_or(Value::Number(0.into())),
        "cache": {
            "read": item.get("cache_read").cloned().unwrap_or(Value::Number(0.into())),
            "write": item.get("cache_write").cloned().unwrap_or(Value::Number(0.into())),
        },
    })
}

fn project_model(model: Value, provider: &Value) -> Value {
    let api = match (model.get("api"), provider.get("api")) {
        (Some(model_api), Some(provider_api))
            if model_api.get("type").and_then(Value::as_str) == Some("native")
                && model_api.get("url").is_none()
                && model_api
                    .get("settings")
                    .and_then(Value::as_object)
                    .is_some_and(Map::is_empty) =>
        {
            let mut next = provider_api.clone();
            next["id"] = model_api.get("id").cloned().unwrap_or(Value::Null);
            next
        }
        (Some(model_api), Some(provider_api))
            if model_api.get("type").and_then(Value::as_str) == Some("aisdk")
                && provider_api.get("type").and_then(Value::as_str) == Some("aisdk")
                && model_api.get("url").is_none() =>
        {
            let mut next = model_api.clone();
            if let Some(url) = provider_api.get("url") {
                next["url"] = url.clone();
            }
            next
        }
        (Some(model_api), _) => model_api.clone(),
        _ => Value::Null,
    };
    let variant = model.pointer("/request/variant").cloned();
    let mut projected = model;
    projected["api"] = api;
    projected["request"] = json!({
        "headers": crate::config::merge(
            provider.pointer("/request/headers").cloned().unwrap_or_else(object),
            projected.pointer("/request/headers").cloned().unwrap_or_else(object),
        ),
        "body": crate::config::merge(
            provider.pointer("/request/body").cloned().unwrap_or_else(object),
            projected.pointer("/request/body").cloned().unwrap_or_else(object),
        ),
    });
    if let Some(variant) = variant {
        projected["request"]["variant"] = variant;
    }
    projected
}

fn merge_request(target: &mut Value, source: &Value) {
    if let Some(headers) = source.get("headers") {
        target["headers"] = crate::config::merge(target["headers"].clone(), headers.clone());
    }
    if let Some(body) = source.get("body") {
        target["body"] = crate::config::merge(target["body"].clone(), body.clone());
    }
}

fn released(date: &str) -> i64 {
    let parts = date
        .split('-')
        .filter_map(|part| part.parse::<i64>().ok())
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return 0;
    }
    let year = parts[0] - (parts[1] <= 2) as i64;
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = parts[1] + if parts[1] > 2 { -3 } else { 9 };
    let doy = (153 * month + 2) / 5 + parts[2] - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400_000
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
    use super::{official_models, official_providers, sort_models, OPENCODE_FREE_MODELS};
    use serde_json::{json, Map};
    use std::sync::{Mutex, OnceLock};

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

    #[test]
    fn free_catalog_tracks_current_opencode_models() {
        assert!(OPENCODE_FREE_MODELS.contains(&"hy3-free"));
        assert!(OPENCODE_FREE_MODELS.contains(&"big-pickle"));
    }

    #[test]
    fn official_catalog_projects_current_protocol_shapes() {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().expect("lock");
        let root = std::env::temp_dir().join(format!(
            "opencode-provider-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(
            root.join("models.json"),
            serde_json::to_string(&json!({
                "acme": {
                    "id": "acme",
                    "name": "Acme",
                    "npm": "@ai-sdk/acme",
                    "api": "https://api.acme.test",
                    "env": [],
                    "models": {
                        "foo": {
                            "id": "foo-api",
                            "name": "Foo",
                            "family": "foo",
                            "release_date": "2024-01-02",
                            "attachment": false,
                            "reasoning": false,
                            "temperature": true,
                            "tool_call": true,
                            "modalities": { "input": ["text"], "output": ["text"] },
                            "cost": {
                                "input": 1.0,
                                "output": 2.0,
                                "cache_read": 0.1,
                                "cache_write": 0.2
                            },
                            "limit": { "context": 1000, "output": 100 }
                        }
                    }
                }
            }))
            .expect("json"),
        )
        .expect("write");
        std::env::set_var("OPENCODE_MODELS_PATH", root.join("models.json"));

        let config = json!({
            "provider": {
                "acme": {
                    "request": {
                        "headers": { "x-test": "yes" },
                        "body": { "baseURL": "https://override.test" }
                    },
                    "models": {
                        "foo": {
                            "request": { "body": { "temperature": 0.2 } }
                        }
                    }
                }
            }
        });
        let providers = official_providers(&config);
        let models = official_models(&config);

        assert_eq!(providers[0]["id"], "acme");
        assert_eq!(providers[0]["api"]["type"], "aisdk");
        assert_eq!(providers[0]["api"]["package"], "@ai-sdk/acme");
        assert_eq!(models[0]["providerID"], "acme");
        assert_eq!(models[0]["api"]["id"], "foo-api");
        assert_eq!(models[0]["capabilities"]["tools"], true);
        assert_eq!(models[0]["capabilities"]["input"], json!(["text"]));
        assert_eq!(models[0]["request"]["headers"]["x-test"], "yes");
        assert_eq!(models[0]["request"]["body"]["temperature"], 0.2);
        assert!(models[0]["time"]["released"].as_i64().unwrap_or_default() > 0);
        assert_eq!(models[0]["cost"][0]["cache"]["read"], 0.1);

        std::env::remove_var("OPENCODE_MODELS_PATH");
        let _ = std::fs::remove_dir_all(root);
    }
}
