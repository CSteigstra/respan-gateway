//! Google Vertex AI: Gemini's `generateContent` and Claude's Messages at a
//! project and location, with a Google access token; Gemini at a tuned
//! model's endpoint too. And its express mode ([`EXPRESS`]): Gemini's
//! `generateContent` under no project, with an API key.

use respan_gateway_conversation::{Content, Effort, Input, Output, Reasoning};
use respan_gateway_conversation_http::Schema;
use respan_gateway_conversation_http::anthropic::AnthropicMessages;
use respan_gateway_conversation_http::gemini::GeminiGenerate;
use respan_gateway_core::Fault;
use respan_gateway_http::bytes::Bytes;
use respan_gateway_http::http::request::Parts as RequestHead;
use respan_gateway_http::path::set_path;
use serde_json::{Map, Value};

use respan_gateway_vendor_kit::{
    Ask, Body, BoxFuture, Dialect, Dialected, KeyIn, Model, Models, Rewrite, Vendor,
    VendorProvider, built_wrong, http, json as body_json, located, models,
};

/// A project's location, under which each of its APIs is: Google's models
/// under `/publishers/google`, Claude under `/publishers/anthropic`, a
/// tuned model at `/endpoints/<id>`.
const LOCATION: &str = "https://{GOOGLE_CLOUD_LOCATION}-aiplatform.googleapis.com/v1/projects/{GOOGLE_CLOUD_PROJECT}/locations/{GOOGLE_CLOUD_LOCATION}";

/// Where express mode's Gemini is: under no project, at the global host.
const EXPRESS_API: &str = "https://aiplatform.googleapis.com/v1/publishers/google";

/// Where Google's models are listed, after its origin, a page of at most
/// 300 at a time.
const MODELS: &str = "/v1beta1/publishers/google/models?pageSize=300";

/// The Messages version Vertex's Claude takes in the body, as it takes no
/// `anthropic-version` header.
const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

/// Google Vertex AI, at a project's location (its base URL, such as
/// `https://aiplatform.eu.rep.googleapis.com/v1/projects/<p>/locations/eu`).
/// Gemini goes to `generateContent`, Claude to Messages through
/// `rawPredict`, whatever the client speaks. The key is a Google access
/// token, which expires within the hour: give the provider `auth = { type =
/// "google", credentials = ".." }` and the gateway mints and refreshes it
/// from a service account's key; a token in `GOOGLE_VERTEX_ACCESS_TOKEN`
/// is sent as it is. The `global` location is at the host of no region, the
/// `eu` and `us` multi-regions at their own (`aiplatform.eu.rep..`).
pub const VENDOR: Vendor = Vendor {
    id: "vertex",
    key_env: &["GOOGLE_VERTEX_ACCESS_TOKEN", "CLOUDSDK_AUTH_ACCESS_TOKEN"],
    formats: &[
        (&Dialected::new(GeminiGenerate, Vertex), Some(LOCATION)),
        (
            &Dialected::new(AnthropicMessages, VertexClaude),
            Some(LOCATION),
        ),
    ],
    models: list_models,
    format_for: Some(format_for),
    locate,
};

/// Vertex AI in express mode: Gemini's `generateContent` under no project,
/// with an API key in `x-goog-api-key`, as Gemini's API takes it. It lists
/// no models, as its API serves `generateContent`, `streamGenerateContent`
/// and `countTokens` alone.
pub const EXPRESS: Vendor = Vendor {
    id: "vertex-express",
    key_env: &["GOOGLE_VERTEX_API_KEY"],
    formats: &[(&GeminiGenerate, Some(EXPRESS_API))],
    models: models::none,
    ..Vendor::DEFAULT
};

/// `url` located, at the host Google gives its location: the `global`
/// location at the host of no region, `aiplatform.googleapis.com`, and a
/// multi-region (`eu`, `us`) at `aiplatform.<it>.rep.googleapis.com`. No
/// host is `global-aiplatform..`, and `eu-aiplatform..` answers 404.
fn locate(url: &str) -> Result<String, &str> {
    let url = located(url)?;
    if let Some(rest) = url.strip_prefix("https://global-aiplatform.") {
        return Ok(format!("https://aiplatform.{rest}"));
    }
    for region in ["eu", "us"] {
        let multi = format!("https://{region}-aiplatform.googleapis.com/");
        if let Some(rest) = url.strip_prefix(&multi) {
            return Ok(format!(
                "https://aiplatform.{region}.rep.googleapis.com/{rest}"
            ));
        }
    }
    Ok(url)
}

/// Claude (`claude-…`) goes to Messages, whatever the client speaks; any
/// other model to `generateContent`.
fn format_for(ask: &Ask, model: &Model) -> Option<&'static str> {
    if ask.kind != "chat" {
        return None;
    }
    Some(match model.id.starts_with("claude") {
        true => AnthropicMessages::NAME,
        false => GeminiGenerate::NAME,
    })
}

/// Vertex's `generateContent`: its key a bearer token, not Gemini's
/// `x-goog-api-key`; Google's models under `/publishers/google`, and a
/// tuned model, named by its endpoint's id (all digits), at that endpoint.
/// Vertex takes no display name on media, so a file's name is left out.
#[derive(Debug, Clone, Copy)]
pub struct Vertex;

impl Dialect for Vertex {
    type Input = Input;
    type Output = Output;

    fn key(&self) -> Option<KeyIn> {
        Some(KeyIn::Bearer)
    }

    fn inputs(&self) -> Option<Box<dyn respan_gateway_core::Filter<Input>>> {
        Some(Box::new(|input: &mut Input| {
            unnamed(input);
            budgeted(input)
        }))
    }

    fn request(&self, head: &mut RequestHead, _: Option<&Bytes>) -> Result<Option<Rewrite>, Fault> {
        let path = head.uri.path().to_owned();
        let call = path
            .strip_prefix("/models/")
            .and_then(|c| c.split_once(':'));
        let Some((model, method)) = call else {
            return Ok(None);
        };
        let at = match tuned(model) {
            true => format!("/endpoints/{model}:{method}"),
            false => format!("/publishers/google/models/{model}:{method}"),
        };
        set_path(head, &at)?;
        Ok(None)
    }
}

/// Whether `model` names a tuned model's endpoint: its id, all digits.
fn tuned(model: &str) -> bool {
    !model.is_empty() && model.bytes().all(|b| b.is_ascii_digit())
}

/// `input` with no file name on its media, which Vertex's Gemini refuses
/// to be told and Gemini's format cannot leave out by itself.
fn unnamed(input: &mut Input) {
    for content in input.messages.iter_mut().flat_map(|m| m.content.iter_mut()) {
        if let Content::Image(media) | Content::Audio(media) | Content::Document(media) = content {
            media.name = None;
        }
    }
}

/// The field a client of another format, which has no thinking budget of its
/// own, names one in, such as Chat Completions' `extra_body`.
const THINKING_BUDGET: &str = "thinking_budget";

/// The thinking budget per effort of a tuned model, as Gemini's format has
/// them for Gemini 2.5, whose tuned models refuse a thinking level. `Off` is
/// a budget of 0, which the format writes itself.
fn tuned_budget(effort: Effort) -> Option<u64> {
    match effort {
        Effort::Off => None,
        Effort::Minimal => Some(1),
        Effort::Low => Some(1024),
        Effort::Medium => Some(2048),
        _ => Some(4096),
    }
}

/// `input`'s thinking in the budget its client named, exact, whatever the
/// model; and, for a tuned model, in the budget of the effort its client
/// named, as such a model takes no level. Its thoughts are asked for as they
/// are for an effort a client names.
fn budgeted(input: &mut Input) -> Result<(), Fault> {
    let named = input
        .native
        .as_mut()
        .and_then(|n| n.fields.shift_remove(THINKING_BUDGET));
    if let Some(named) = named {
        let tokens = named.as_u64().ok_or_else(|| {
            Fault::bad_request(format!("'{THINKING_BUDGET}' is a number of tokens"))
        })?;
        input.options.reasoning = Some(match tokens {
            0 => Reasoning::effort(Effort::Off),
            tokens => Reasoning::budget(tokens),
        });
    } else if !input.model.as_deref().is_some_and(tuned) {
        return Ok(());
    }
    let Some(reasoning) = input.options.reasoning.as_mut() else {
        return Ok(());
    };
    if let Some(budget) = reasoning.effort.and_then(tuned_budget) {
        reasoning.effort = None;
        reasoning.budget_tokens = Some(budget);
    }
    let thinks = reasoning.effort != Some(Effort::Off);
    reasoning.summary.get_or_insert(thinks);
    Ok(())
}

/// Claude's Messages as Vertex serves them: the model in the path, not the
/// body (`/publishers/anthropic/models/<model>:rawPredict`, or
/// `:streamRawPredict` for a stream), and the API's version in the body.
#[derive(Debug, Clone, Copy)]
pub struct VertexClaude;

impl Dialect for VertexClaude {
    type Input = Input;
    type Output = Output;

    fn key(&self) -> Option<KeyIn> {
        Some(KeyIn::Bearer)
    }

    fn request(
        &self,
        head: &mut RequestHead,
        body: Option<&Bytes>,
    ) -> Result<Option<Rewrite>, Fault> {
        let body = body.ok_or_else(|| Fault::internal("a Messages call has a whole body"))?;
        let call: Map<String, Value> = serde_json::from_slice(body)
            .map_err(|e| Fault::internal("a Messages call is not JSON").with_source(e))?;
        let model = call
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !claude_id(model) {
            return Err(Fault::bad_request(
                "a Claude call on Vertex needs a model, such as claude-haiku-4-5@20251001",
            ));
        }
        let method = match call.get("stream") == Some(&Value::Bool(true)) {
            true => "streamRawPredict",
            false => "rawPredict",
        };
        set_path(
            head,
            &format!("/publishers/anthropic/models/{model}:{method}"),
        )?;
        head.headers.remove("anthropic-version");
        Ok(Some(Box::new(move |_| {
            let mut call = call;
            call.remove("model");
            call.entry("anthropic_version")
                .or_insert_with(|| ANTHROPIC_VERSION.into());
            serde_json::to_vec(&call)
                .map(Bytes::from)
                .map_err(|e| Fault::internal("a Messages call could not be written").with_source(e))
        })))
    }
}

/// Whether `model` is a Vertex model id that is one path segment as it is:
/// letters, digits, `-`, `.`, `_` and the `@` before a version.
fn claude_id(model: &str) -> bool {
    let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'@');
    !model.is_empty() && model.bytes().all(ok)
}

/// Its models: Google's list ([`MODELS`]) at its API's origin (its base
/// before `/v1/`), as the list is under no project; as OpenAI lists them
/// for a provider given no base of `generateContent`. A list says no kind,
/// limits or prices, so a model has its id and maker alone.
fn list_models(p: &VendorProvider) -> BoxFuture<'_, Models> {
    Box::pin(async move {
        let Some(base) = p.base_url_of(GeminiGenerate::NAME) else {
            return models::openai(p).await;
        };
        let origin = base.split_once("/v1/").map_or(base, |(origin, _)| origin);
        let client = p.client_at(origin)?;
        let (mut models, mut at) = (Vec::new(), MODELS.to_owned());
        loop {
            let call = http::Request::get(&at).body(Body::default());
            let (answer, _) = client.send(call.map_err(built_wrong)?).await?;
            let answer = body_json(answer).await?;
            models.extend(listed(&answer));
            match answer["nextPageToken"].as_str() {
                Some(next) if !next.is_empty() => at = format!("{MODELS}&pageToken={next}"),
                _ => return Ok(models),
            }
        }
    })
}

/// The models of Google's list that a call reaches: those Vertex gives a
/// resource, none that must be deployed first (`deploy`), and none of its
/// open checkpoints, which are notebooks.
fn listed(answer: &Value) -> Vec<Model> {
    let models = answer["publisherModels"].as_array().into_iter().flatten();
    let model = |m: &Value| {
        let actions = m["supportedActions"].as_object();
        let deploy = actions.is_some_and(|a| a.contains_key("deploy"));
        let open = m["openSourceCategory"].as_str().unwrap_or_default();
        let resource = m.get("publisherModelTemplate").is_some();
        let served = resource && !deploy && !open.contains("OSS");
        let name = m["name"].as_str().filter(|_| served)?;
        let id = name
            .strip_prefix("publishers/google/models/")
            .unwrap_or(name);
        let mut model = Model::new(id);
        model.maker = Some("google".to_owned());
        Some(model)
    };
    models.filter_map(model).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_global_location_is_at_the_host_of_no_region() {
        let url = "https://global-aiplatform.googleapis.com/v1/projects/p/locations/global";
        assert_eq!(
            locate(url).as_deref(),
            Ok("https://aiplatform.googleapis.com/v1/projects/p/locations/global")
        );
        let regional = "https://us-east5-aiplatform.googleapis.com/v1";
        assert_eq!(locate(regional).as_deref(), Ok(regional));
    }

    #[test]
    fn a_multi_region_is_at_its_own_host() {
        let url = "https://eu-aiplatform.googleapis.com/v1/projects/p/locations/eu";
        assert_eq!(
            locate(url).as_deref(),
            Ok("https://aiplatform.eu.rep.googleapis.com/v1/projects/p/locations/eu")
        );
        let us = "https://us-aiplatform.googleapis.com/v1/projects/p/locations/us";
        assert!(
            locate(us).is_ok_and(|u| u.starts_with("https://aiplatform.us.rep.googleapis.com/"))
        );
        // A region that starts like one is its own.
        let west = "https://europe-west4-aiplatform.googleapis.com/v1";
        assert_eq!(locate(west).as_deref(), Ok(west));
    }

    fn head(uri: &'static str) -> RequestHead {
        let call = http::Request::post(uri).body(()).expect("a call");
        call.into_parts().0
    }

    #[test]
    fn gemini_is_under_google_and_a_tuned_model_at_its_endpoint() {
        let mut call = head("/models/gemini-3.8-flash:streamGenerateContent?alt=sse");
        Vertex.request(&mut call, None).expect("a path");
        assert_eq!(
            call.uri,
            "/publishers/google/models/gemini-3.8-flash:streamGenerateContent?alt=sse"
        );
        let mut call = head("/models/1234567890123456789:generateContent");
        Vertex.request(&mut call, None).expect("a path");
        assert_eq!(call.uri, "/endpoints/1234567890123456789:generateContent");
    }

    #[test]
    fn claude_is_named_in_the_path_and_versioned_in_the_body() {
        let body = Bytes::from_static(
            br#"{"model":"claude-haiku-5-5@20261001","max_tokens":8,"stream":true,"messages":[]}"#,
        );
        let mut call = head("/v1/messages");
        call.headers.insert(
            "anthropic-version",
            http::HeaderValue::from_static("2023-06-01"),
        );
        let rewrite = VertexClaude
            .request(&mut call, Some(&body))
            .expect("a path")
            .expect("a rewrite");
        assert_eq!(
            call.uri,
            "/publishers/anthropic/models/claude-haiku-5-5@20261001:streamRawPredict"
        );
        assert!(!call.headers.contains_key("anthropic-version"));
        let sent: Value = serde_json::from_slice(&rewrite(body).expect("a body")).expect("JSON");
        assert_eq!(
            sent,
            json!({"max_tokens": 8, "stream": true, "messages": [], "anthropic_version": ANTHROPIC_VERSION})
        );
        let unary = Bytes::from_static(br#"{"model":"claude-sonnet-5-5","messages":[]}"#);
        let mut call = head("/v1/messages");
        VertexClaude
            .request(&mut call, Some(&unary))
            .expect("a path");
        assert_eq!(
            call.uri,
            "/publishers/anthropic/models/claude-sonnet-5-5:rawPredict"
        );
        let odd = Bytes::from_static(br#"{"model":"../x","messages":[]}"#);
        assert!(
            VertexClaude
                .request(&mut head("/v1/messages"), Some(&odd))
                .is_err()
        );
    }

    /// The thinking `input` is left with, its model and client's fields given.
    fn thinking(
        model: &str,
        reasoning: Option<Reasoning>,
        budget: Option<Value>,
    ) -> Result<Option<Reasoning>, Fault> {
        let mut input = Input::default();
        input.model = Some(model.to_owned());
        input.options.reasoning = reasoning;
        if let Some(budget) = budget {
            let mut fields = Map::new();
            fields.insert(THINKING_BUDGET.to_owned(), budget);
            input.native = Some(respan_gateway_core::NativeFields::new(
                "openai-chat",
                fields,
            ));
        }
        budgeted(&mut input)?;
        assert!(
            input
                .native
                .is_none_or(|n| !n.fields.contains_key(THINKING_BUDGET))
        );
        Ok(input.options.reasoning)
    }

    #[test]
    fn a_tuned_model_takes_an_effort_as_a_budget_and_another_model_keeps_it() {
        let tuned_model = "1234567890123456789";
        let low = Reasoning::effort(Effort::Low);
        let got = thinking(tuned_model, Some(low.clone()), None).expect("thinking");
        let mut want = Reasoning::budget(1024);
        want.summary = Some(true);
        assert_eq!(got, Some(want));
        let off =
            thinking(tuned_model, Some(Reasoning::effort(Effort::Off)), None).expect("thinking");
        let off = off.expect("thinking");
        assert_eq!(
            (off.effort, off.budget_tokens, off.summary),
            (Some(Effort::Off), None, Some(false))
        );
        assert_eq!(
            thinking("gemini-3.8-flash", Some(low.clone()), None).expect("thinking"),
            Some(low)
        );
        assert_eq!(thinking(tuned_model, None, None).expect("thinking"), None);
    }

    #[test]
    fn a_named_budget_is_exact_on_any_model_and_must_be_a_number() {
        let mut want = Reasoning::budget(1500);
        want.summary = Some(true);
        for model in ["1234567890123456789", "gemini-2.5-flash"] {
            let got = thinking(
                model,
                Some(Reasoning::effort(Effort::Low)),
                Some(json!(1500)),
            );
            assert_eq!(got.expect("thinking"), Some(want.clone()), "{model}");
        }
        let off = thinking("1234567890123456789", None, Some(json!(0))).expect("thinking");
        assert_eq!(off.and_then(|r| r.effort), Some(Effort::Off));
        assert!(thinking("gemini-2.5-flash", None, Some(json!("lots"))).is_err());
    }

    #[test]
    fn claude_goes_to_messages_and_gemini_to_generate_content() {
        let ask = Ask {
            kind: "chat",
            format: "openai-chat",
            model: None,
            client: None,
            tools: false,
        };
        let format = |id: &str| format_for(&ask, &Model::new(id));
        assert_eq!(format("claude-haiku-5-5"), Some(AnthropicMessages::NAME));
        assert_eq!(format("gemini-3.8-flash"), Some(GeminiGenerate::NAME));
        assert_eq!(format("1234567890123456789"), Some(GeminiGenerate::NAME));
    }

    #[test]
    fn its_models_are_those_a_call_reaches() {
        let answer = json!({"publisherModels": [
            {"name": "publishers/google/models/gemini-3.5-flash",
             "publisherModelTemplate": "projects/{project}/..",
             "supportedActions": {"openGenerationAiStudio": {}}},
            {"name": "publishers/google/models/gemma3",
             "supportedActions": {"deploy": {}, "multiDeployVertex": {}}},
            {"name": "publishers/google/models/occupancy-analytics",
             "supportedActions": {"createApplication": {}}},
            {"name": "publishers/google/models/weather-next-v2",
             "openSourceCategory": "PROPRIETARY",
             "supportedActions": {"requestAccess": {}}},
            {"name": "publishers/google/models/imageclassification-proprietary-vit",
             "openSourceCategory": "THIRD_PARTY_OWNED_OSS",
             "publisherModelTemplate": "projects/{project}/.."},
        ]});
        let ids: Vec<_> = listed(&answer)
            .into_iter()
            .map(|m| (m.id, m.maker))
            .collect();
        assert_eq!(ids, [("gemini-3.5-flash".into(), Some("google".into()))]);
    }
}
