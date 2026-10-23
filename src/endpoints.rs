//! Ordered chat endpoints and the retry walk across them.

use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

const MAX_ENDPOINTS: usize = 8;
const MAX_MODELS_PER_ENDPOINT: usize = 8;
const MAX_CHAT_ATTEMPTS: usize = 4;
const MAX_PROVIDER_CHARS: usize = 40;
const MAX_MODEL_CHARS: usize = 128;

pub struct Attempt {
    pub provider: String,
    pub model: String,
    pub url: String,
    pub api_key: String,
}

impl Attempt {
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

struct Endpoint {
    name: String,
    url: String,
    api_key: String,
    models: Vec<String>,
}

pub struct EndpointSet {
    endpoints: Vec<Endpoint>,
    default_model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointsFile {
    endpoints: Vec<EndpointConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointConfig {
    name: String,
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    models: Vec<String>,
}

impl EndpointSet {
    pub fn load(extra_models: Option<&str>) -> Result<Self, String> {
        if let Some(path) = configured_path()? {
            let mut set = Self::load_file(&path)?;
            set.select_default_from_env()?;
            return Ok(set);
        }
        let default_path = Path::new("endpoints.json");
        if default_path.is_file() {
            let mut set = Self::load_file(default_path)?;
            set.select_default_from_env()?;
            return Ok(set);
        }
        Self::from_process_env(extra_models)
    }

    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    pub fn default_label(&self) -> String {
        self.attempts(&self.default_model)
            .first()
            .map(Attempt::label)
            .unwrap_or_else(|| self.default_model.clone())
    }

    pub fn describe(&self) -> String {
        self.attempts(&self.default_model)
            .iter()
            .map(Attempt::label)
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn ensure_known_models(&self, models: &[String]) -> Result<(), String> {
        for model in models {
            if self.canonical_model(model).is_none() {
                return Err(format!(
                    "Forum model {model:?} is not listed on any endpoint."
                ));
            }
        }
        Ok(())
    }

    /// Preferred model first, on each endpoint that lists it, then the other
    /// configured models. At most four attempts. An unknown model is sent to
    /// each endpoint before those fallbacks.
    pub fn attempts(&self, model: &str) -> Vec<Attempt> {
        let mut attempts = Vec::new();
        for endpoint in &self.endpoints {
            for configured in &endpoint.models {
                if configured.eq_ignore_ascii_case(model) {
                    attempts.push(endpoint.attempt(configured));
                    if attempts.len() == MAX_CHAT_ATTEMPTS {
                        return attempts;
                    }
                }
            }
        }
        if attempts.is_empty() {
            for endpoint in &self.endpoints {
                attempts.push(Attempt {
                    provider: endpoint.name.clone(),
                    model: model.to_owned(),
                    url: endpoint.url.clone(),
                    api_key: endpoint.api_key.clone(),
                });
                if attempts.len() == MAX_CHAT_ATTEMPTS {
                    return attempts;
                }
            }
        }
        for endpoint in &self.endpoints {
            for configured in &endpoint.models {
                if configured.eq_ignore_ascii_case(model) {
                    continue;
                }
                attempts.push(endpoint.attempt(configured));
                if attempts.len() == MAX_CHAT_ATTEMPTS {
                    return attempts;
                }
            }
        }
        attempts
    }

    fn load_file(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|error| {
            format!(
                "Could not read endpoints config {}: {error}",
                path.display()
            )
        })?;
        let value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Endpoints config is not valid JSON (line {}, column {}).",
                error.line(),
                error.column()
            )
        })?;
        Self::parse(&value, |name| env::var(name).ok())
    }

    fn parse(
        value: &Value,
        mut env_lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let file: EndpointsFile = serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "Endpoints config is invalid (line {}, column {}): {error}",
                error.line(),
                error.column()
            )
        })?;
        if file.endpoints.is_empty() {
            return Err("Endpoints config needs at least one endpoint.".into());
        }
        if file.endpoints.len() > MAX_ENDPOINTS {
            return Err(format!("At most {MAX_ENDPOINTS} endpoints are supported."));
        }
        let mut endpoints = Vec::new();
        for config in file.endpoints {
            let endpoint = Endpoint::from_config(config, &mut env_lookup)?;
            if endpoints
                .iter()
                .any(|known: &Endpoint| known.name.eq_ignore_ascii_case(&endpoint.name))
            {
                return Err(format!("Duplicate endpoint name: {}.", endpoint.name));
            }
            endpoints.push(endpoint);
        }
        let default_model = endpoints[0].models[0].clone();
        Ok(Self {
            endpoints,
            default_model,
        })
    }

    fn from_process_env(extra_models: Option<&str>) -> Result<Self, String> {
        let api_key = env::var("OPENAI_API_KEY")
            .unwrap_or_default()
            .trim()
            .to_owned();
        if api_key.is_empty() {
            return Err("Missing OPENAI_API_KEY in the environment.".into());
        }
        let model = match env::var("OPENAI_MODEL") {
            Ok(value) if value.trim().is_empty() => {
                return Err("Missing OPENAI_MODEL in the environment.".into());
            }
            Ok(value) => value.trim().to_owned(),
            Err(_) => "gpt-4o-mini".to_owned(),
        };
        let base = env::var("OPENAI_API_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                env::var("OPENAI_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "https://api.openai.com/v1".into());
        let provider = env::var("OPENAI_PROVIDER")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Self::from_environment_values(
            &api_key,
            &model,
            base.trim(),
            provider.as_deref().map(str::trim),
            extra_models,
        )
    }

    fn from_environment_values(
        api_key: &str,
        model: &str,
        base_url: &str,
        provider: Option<&str>,
        extra_models: Option<&str>,
    ) -> Result<Self, String> {
        let name = match provider {
            Some(name) => {
                if !valid_provider_name(name) {
                    return Err(
                        "OPENAI_PROVIDER must contain 1 to 40 letters, digits, dots, underscores, or hyphens."
                            .into(),
                    );
                }
                name.to_owned()
            }
            None => provider_name_from_base_url(base_url)?,
        };
        let mut models = vec![validate_model_id(model)?];
        if let Some(extra_models) = extra_models
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            for extra in extra_models
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let extra = validate_model_id(extra)?;
                if !models
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&extra))
                {
                    models.push(extra);
                }
            }
        }
        let endpoint = Endpoint::from_config(
            EndpointConfig {
                name,
                base_url: base_url.to_owned(),
                api_key: Some(api_key.to_owned()),
                api_key_env: None,
                models,
            },
            &mut |_| None,
        )?;
        let default_model = endpoint.models[0].clone();
        Ok(Self {
            endpoints: vec![endpoint],
            default_model,
        })
    }

    fn select_default_from_env(&mut self) -> Result<(), String> {
        match env::var("OPENAI_MODEL") {
            Ok(value) if value.trim().is_empty() => {
                Err("Missing OPENAI_MODEL in the environment.".into())
            }
            Ok(value) => self.select_default(Some(value.trim())),
            Err(_) => Ok(()),
        }
    }

    fn select_default(&mut self, model: Option<&str>) -> Result<(), String> {
        let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        let Some(canonical) = self.canonical_model(model) else {
            return Err(format!(
                "OPENAI_MODEL {model} is not listed on any endpoint."
            ));
        };
        self.default_model = canonical;
        Ok(())
    }

    fn canonical_model(&self, model: &str) -> Option<String> {
        self.endpoints.iter().find_map(|endpoint| {
            endpoint
                .models
                .iter()
                .find(|candidate| candidate.eq_ignore_ascii_case(model))
                .cloned()
        })
    }
}

impl Endpoint {
    fn from_config(
        config: EndpointConfig,
        env_lookup: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let name = config.name.trim();
        if !valid_provider_name(name) {
            return Err(
                "Endpoint name must contain 1 to 40 letters, digits, dots, underscores, or hyphens."
                    .into(),
            );
        }
        let api_key = resolve_api_key(name, config.api_key, config.api_key_env, env_lookup)?;
        if config.models.is_empty() {
            return Err(format!("Endpoint {name} needs at least one model."));
        }
        if config.models.len() > MAX_MODELS_PER_ENDPOINT {
            return Err(format!(
                "Endpoint {name} lists more than {MAX_MODELS_PER_ENDPOINT} models."
            ));
        }
        let mut models = Vec::new();
        for model in config.models {
            let model = validate_model_id(model.trim())?;
            if models
                .iter()
                .any(|known: &String| known.eq_ignore_ascii_case(&model))
            {
                return Err(format!("Duplicate model {model} on endpoint {name}."));
            }
            models.push(model);
        }
        Ok(Self {
            name: name.to_owned(),
            url: chat_completions_url(name, config.base_url.trim())?,
            api_key,
            models,
        })
    }

    fn attempt(&self, model: &str) -> Attempt {
        Attempt {
            provider: self.name.clone(),
            model: model.to_owned(),
            url: self.url.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

/// Tries each attempt until one returns `Ok`.
/// The successful label is `provider/model`.
pub async fn retry_in_order<T, F, Fut>(
    attempts: Vec<Attempt>,
    mut call: F,
) -> Result<(T, String), String>
where
    F: FnMut(Attempt) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    if attempts.is_empty() {
        return Err("No chat endpoint is configured.".into());
    }
    let several = attempts.len() > 1;
    let mut last_error = String::new();
    for attempt in attempts {
        let label = attempt.label();
        match call(attempt).await {
            Ok(value) => return Ok((value, label)),
            Err(error) => {
                eprintln!("Chat attempt {label} failed: {error}");
                last_error = error;
            }
        }
    }
    if several {
        Err(format!(
            "Every configured endpoint failed. Last error: {last_error}"
        ))
    } else {
        Err(last_error)
    }
}

pub fn append_provider_label(text: &str, label: &str) -> String {
    let text = text.trim_end();
    if text.is_empty() {
        label.to_owned()
    } else {
        format!("{text}\n\n{label}")
    }
}

fn configured_path() -> Result<Option<PathBuf>, String> {
    match env::var("ENDPOINTS_CONFIG") {
        Ok(value) if value.trim().is_empty() => Err("ENDPOINTS_CONFIG is empty.".into()),
        Ok(value) => Ok(Some(PathBuf::from(value.trim()))),
        Err(_) => Ok(None),
    }
}

fn resolve_api_key(
    name: &str,
    api_key: Option<String>,
    api_key_env: Option<String>,
    env_lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<String, String> {
    let api_key = api_key
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let api_key_env = api_key_env
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    match (api_key, api_key_env) {
        (Some(_), Some(_)) => Err(format!(
            "Endpoint {name} must set only one of api_key or api_key_env."
        )),
        (None, None) => Err(format!("Endpoint {name} needs an api_key or api_key_env.")),
        (Some(api_key), None) => Ok(api_key),
        (None, Some(variable)) => {
            if !valid_env_name(&variable) {
                return Err(format!("Endpoint {name} has an invalid api_key_env name."));
            }
            let value = env_lookup(&variable)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!("Environment variable {variable} is missing for endpoint {name}.")
                })?;
            Ok(value)
        }
    }
}

fn chat_completions_url(name: &str, base: &str) -> Result<String, String> {
    let base = base.trim().trim_end_matches('/');
    let parsed =
        Url::parse(base).map_err(|_| format!("Endpoint {name} has an invalid base_url."))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("Endpoint {name} needs an http or https base_url."));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "Endpoint {name} base_url must not include credentials."
        ));
    }
    if base.ends_with("/chat/completions") {
        Ok(base.to_owned())
    } else {
        Ok(format!("{base}/chat/completions"))
    }
}

fn provider_name_from_base_url(base: &str) -> Result<String, String> {
    let parsed =
        Url::parse(base.trim()).map_err(|_| "OPENAI_API_BASE_URL is invalid.".to_owned())?;
    let Some(host) = parsed.host_str() else {
        return Err("OPENAI_API_BASE_URL is invalid.".into());
    };
    let name = if host
        .chars()
        .all(|character| character.is_ascii_digit() || matches!(character, '.' | ':'))
    {
        host.replace(['.', ':'], "-")
    } else {
        host.split('.')
            .find(|label| !matches!(*label, "api" | "www"))
            .unwrap_or(host)
            .to_owned()
    };
    if valid_provider_name(&name) {
        Ok(name)
    } else {
        Ok("openai".to_owned())
    }
}

fn validate_model_id(model: &str) -> Result<String, String> {
    if valid_model_id(model) {
        Ok(model.to_owned())
    } else {
        Err(format!(
            "Model ids must contain 1 to {MAX_MODEL_CHARS} visible ASCII characters without spaces: {model:?}"
        ))
    }
}

fn valid_provider_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_PROVIDER_CHARS).contains(&count)
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

fn valid_model_id(model: &str) -> bool {
    let count = model.chars().count();
    (1..=MAX_MODEL_CHARS).contains(&count)
        && model.chars().all(|character| {
            character.is_ascii()
                && !character.is_ascii_control()
                && !character.is_ascii_whitespace()
                && !matches!(character, '@' | '<' | '>' | '`')
        })
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() || first == '_' => {}
        _ => return false,
    }
    (1..=64).contains(&name.len())
        && chars.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sample_config() -> Value {
        json!({
            "endpoints": [
                {
                    "name": "primary",
                    "base_url": "https://primary.example/v1",
                    "api_key": "primary-key",
                    "models": ["claude-opus-5-5", "gpt-4o-mini"]
                },
                {
                    "name": "backup",
                    "base_url": "https://backup.example/v1/chat/completions",
                    "api_key_env": "BACKUP_API_KEY",
                    "models": ["claude-opus-5-5", "llama-backup"]
                }
            ]
        })
    }

    fn expect_err<T>(result: Result<T, String>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    fn sample_set() -> EndpointSet {
        EndpointSet::parse(&sample_config(), |name| {
            (name == "BACKUP_API_KEY").then(|| "backup-key".to_owned())
        })
        .unwrap()
    }

    #[test]
    fn orders_the_preferred_model_before_fallbacks() {
        let set = sample_set();
        let attempts = set.attempts("claude-opus-5-5");
        let labels: Vec<_> = attempts.iter().map(Attempt::label).collect();
        assert_eq!(
            labels,
            [
                "primary/claude-opus-5-5",
                "backup/claude-opus-5-5",
                "primary/gpt-4o-mini",
                "backup/llama-backup"
            ]
        );
        assert_eq!(
            attempts[0].url,
            "https://primary.example/v1/chat/completions"
        );
        assert_eq!(
            attempts[1].url,
            "https://backup.example/v1/chat/completions"
        );
        assert_eq!(attempts[0].api_key, "primary-key");
        assert_eq!(attempts[1].api_key, "backup-key");
        assert_eq!(set.default_label(), "primary/claude-opus-5-5");
    }

    #[test]
    fn unknown_model_is_tried_on_each_endpoint_first() {
        let set = sample_set();
        let labels: Vec<_> = set
            .attempts("CLAUDE-OPUS-5-5")
            .iter()
            .map(Attempt::label)
            .collect();
        assert_eq!(labels[0], "primary/claude-opus-5-5");
        let labels: Vec<_> = set
            .attempts("forum-only")
            .iter()
            .map(Attempt::label)
            .collect();
        assert_eq!(
            labels,
            [
                "primary/forum-only",
                "backup/forum-only",
                "primary/claude-opus-5-5",
                "primary/gpt-4o-mini"
            ]
        );
    }

    #[test]
    fn caps_attempts_and_rejects_bad_configs() {
        let mut wide = sample_config();
        wide["endpoints"][0]["models"] =
            json!(["one", "two", "three", "four", "five", "six", "seven", "eight"]);
        let set = EndpointSet::parse(&wide, |name| {
            (name == "BACKUP_API_KEY").then(|| "backup-key".to_owned())
        })
        .unwrap();
        assert_eq!(set.attempts("one").len(), MAX_CHAT_ATTEMPTS);

        let missing_key = json!({
            "endpoints": [{"name": "openai", "base_url": "https://api.openai.com/v1", "models": ["gpt-4o-mini"]}]
        });
        assert!(expect_err(EndpointSet::parse(&missing_key, |_| None)).contains("api_key"));
        let both_keys = json!({
            "endpoints": [{
                "name": "openai",
                "base_url": "https://api.openai.com/v1",
                "api_key": "inline",
                "api_key_env": "OPENAI_API_KEY",
                "models": ["gpt-4o-mini"]
            }]
        });
        assert!(
            expect_err(EndpointSet::parse(&both_keys, |_| Some("env-key".into())))
                .contains("only one")
        );
        let credentials = json!({
            "endpoints": [{
                "name": "openai",
                "base_url": "https://user:secret@api.openai.com/v1",
                "api_key": "inline",
                "models": ["gpt-4o-mini"]
            }]
        });
        assert!(expect_err(EndpointSet::parse(&credentials, |_| None)).contains("credentials"));
        let duplicate = json!({
            "endpoints": [
                {"name": "openai", "base_url": "https://api.openai.com/v1", "api_key": "a", "models": ["gpt-4o-mini"]},
                {"name": "OpenAI", "base_url": "https://example.com/v1", "api_key": "b", "models": ["gpt-4o-mini"]}
            ]
        });
        assert!(expect_err(EndpointSet::parse(&duplicate, |_| None)).contains("Duplicate endpoint"));
        let bad_model = json!({
            "endpoints": [{
                "name": "openai",
                "base_url": "https://api.openai.com/v1",
                "api_key": "inline",
                "models": ["bad model"]
            }]
        });
        assert!(EndpointSet::parse(&bad_model, |_| None).is_err());
        assert!(EndpointSet::parse(&json!([]), |_| None).is_err());
    }

    #[test]
    fn env_fallback_uses_the_host_as_the_provider_name() {
        let set = EndpointSet::from_environment_values(
            "test-key",
            "claude-opus-5-5",
            "https://api.aixingialaire.shop/v1/",
            None,
            Some("gpt-4o-mini, claude-opus-5-5"),
        )
        .unwrap();
        assert_eq!(set.default_model(), "claude-opus-5-5");
        let labels: Vec<_> = set
            .attempts(set.default_model())
            .iter()
            .map(Attempt::label)
            .collect();
        assert_eq!(
            labels,
            ["aixingialaire/claude-opus-5-5", "aixingialaire/gpt-4o-mini"]
        );
        assert_eq!(
            provider_name_from_base_url("https://api.openai.com/v1").unwrap(),
            "openai"
        );
        assert_eq!(
            provider_name_from_base_url("https://openrouter.ai/api/v1").unwrap(),
            "openrouter"
        );
        assert_eq!(
            provider_name_from_base_url("http://127.0.0.1:8080/v1").unwrap(),
            "127-0-0-1"
        );
        let named = EndpointSet::from_environment_values(
            "test-key",
            "gpt-4o-mini",
            "https://api.openai.com/v1",
            Some("gateway"),
            None,
        )
        .unwrap();
        assert_eq!(named.default_label(), "gateway/gpt-4o-mini");
        named.ensure_known_models(&["gpt-4o-mini".into()]).unwrap();
        assert!(named.ensure_known_models(&["missing".into()]).is_err());
    }

    #[test]
    fn default_model_must_be_listed_on_an_endpoint() {
        let mut set = sample_set();
        set.select_default(Some("LLAMA-BACKUP")).unwrap();
        assert_eq!(set.default_model(), "llama-backup");
        assert_eq!(
            set.describe().split(", ").next().unwrap(),
            "backup/llama-backup"
        );
        assert!(set.select_default(Some("missing-model")).is_err());
    }

    #[test]
    fn appends_the_provider_and_model_on_their_own_line() {
        assert_eq!(
            append_provider_label("Hello\n", "openai/gpt-4o-mini"),
            "Hello\n\nopenai/gpt-4o-mini"
        );
        assert_eq!(
            append_provider_label("  ", "openai/gpt-4o-mini"),
            "openai/gpt-4o-mini"
        );
    }

    #[test]
    fn reads_a_config_file_without_echoing_its_contents() {
        let path =
            std::env::temp_dir().join(format!("knowsphere-{}-endpoints.json", std::process::id()));
        fs::write(
            &path,
            "{ \"endpoints\": [ { \"name\": \"openai\", \"base_url\": \"https://api.openai.com/v1\", \"api_key\": \"file-key\", \"models\": [\"gpt-4o-mini\"] } ] }\n",
        )
        .unwrap();
        let loaded = EndpointSet::load_file(&path);
        let _ = fs::remove_file(&path);
        let set = loaded.unwrap();
        assert_eq!(set.default_label(), "openai/gpt-4o-mini");
        assert_eq!(set.attempts("gpt-4o-mini")[0].api_key, "file-key");
        let missing = expect_err(EndpointSet::load_file(Path::new(
            "endpoints-does-not-exist.json",
        )));
        assert!(missing.contains("Could not read endpoints config"));
        assert!(!missing.contains("file-key"));
    }

    #[tokio::test]
    async fn retries_until_an_attempt_succeeds() {
        let attempts = sample_set().attempts("claude-opus-5-5");
        let calls = AtomicUsize::new(0);
        let (value, label) = retry_in_order(attempts, |attempt| {
            let count = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if count == 0 {
                    Err("The AI endpoint timed out. Please try again.".into())
                } else {
                    Ok(attempt.label())
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(label, "backup/claude-opus-5-5");
        assert_eq!(value, label);

        let failure = retry_in_order(sample_set().attempts("gpt-4o-mini"), |attempt| {
            let label = attempt.label();
            async move { Err::<String, String>(format!("{label} unavailable")) }
        })
        .await
        .unwrap_err();
        assert!(failure.starts_with("Every configured endpoint failed."));
        assert!(failure.contains("Last error: backup/llama-backup unavailable"));

        let single = EndpointSet::from_environment_values(
            "test-key",
            "gpt-4o-mini",
            "https://api.openai.com/v1",
            None,
            None,
        )
        .unwrap();
        let failure = retry_in_order(single.attempts("gpt-4o-mini"), |_attempt| async {
            Err::<String, String>(
                "The AI endpoint returned HTTP 404. Check the endpoint URL and model name.".into(),
            )
        })
        .await
        .unwrap_err();
        assert_eq!(
            failure,
            "The AI endpoint returned HTTP 404. Check the endpoint URL and model name."
        );
    }
}
