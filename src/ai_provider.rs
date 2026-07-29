//! Pure preparation for OpenAI-compatible provider requests.
//!
//! This module performs no environment access, DNS lookup, serialization, or
//! network I/O. It validates a provider at the last boundary before transport
//! and returns an owned request whose safety controls cannot be replaced by
//! provider-specific body fields.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::ai_prompt::AiPrompt;
use crate::config::{
    AiProvider, Credential, ExtraRequestValue, MAX_AI_PROVIDER_TIMEOUT_MS,
    MIN_AI_PROVIDER_TIMEOUT_MS, ProviderProtocol,
};

/// Longest configured endpoint accepted by the request boundary.
pub const MAX_ENDPOINT_BYTES: usize = 4 * 1024;
/// Longest model identifier accepted by the request boundary.
pub const MAX_MODEL_BYTES: usize = 256;
/// Longest credential environment-variable name accepted by the boundary.
pub const MAX_CREDENTIAL_ENV_BYTES: usize = 128;
/// Longest credential accepted for one authorization header.
pub const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
/// Maximum encoded provider-specific data merged into a request body.
pub const MAX_EXTRA_REQUEST_BODY_BYTES: usize = 32 * 1024;
/// Maximum number of scalar, array, and table values in provider-specific data.
pub const MAX_EXTRA_REQUEST_VALUES: usize = 512;
/// Maximum nesting depth in provider-specific request data.
pub const MAX_EXTRA_REQUEST_DEPTH: usize = 8;
/// Maximum number of entries in one provider-specific array or table.
pub const MAX_EXTRA_CONTAINER_ITEMS: usize = 128;
/// Maximum length of a provider-specific field name.
pub const MAX_EXTRA_KEY_BYTES: usize = 128;
/// Maximum length of one provider-specific string value.
pub const MAX_EXTRA_STRING_BYTES: usize = 8 * 1024;
/// Maximum encoded request body a transport may send.
pub const MAX_PROVIDER_REQUEST_BODY_BYTES: usize = 256 * 1024;
/// Maximum successful response body a transport may read.
pub const MAX_PROVIDER_RESPONSE_BODY_BYTES: usize = 64 * 1024;
/// Maximum error body a transport may read for sanitized diagnostics.
pub const MAX_PROVIDER_ERROR_BODY_BYTES: usize = 8 * 1024;

const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
const RESERVED_EXTRA_FIELDS: [&str; 7] = [
    "messages",
    "model",
    "stream",
    "timeout",
    "timeout_ms",
    "max_response_bytes",
    "max_response_body_bytes",
];

/// Authority granted to a configured plain-HTTP endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlainHttpPolicy {
    /// Permit HTTP only when the URL names a canonical loopback host.
    #[default]
    LoopbackOnly,
    /// Permit remote HTTP because the user explicitly enabled an insecure override.
    ExplicitInsecureOverride,
}

/// Effective transport protection after endpoint validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportSecurity {
    /// HTTPS protects the request in transit.
    Tls,
    /// Plain HTTP is confined to a canonical loopback host.
    PlainLoopback,
    /// Plain remote HTTP was admitted by an explicit insecure override.
    PlainInsecureOverride,
}

/// A canonical chat-completions endpoint safe for the selected policy.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedEndpoint {
    request_url: String,
    display_url: String,
    security: TransportSecurity,
}

impl ValidatedEndpoint {
    /// Complete URL that the HTTP transport should request.
    ///
    /// This may include a configured query string. Diagnostics should use this
    /// type's [`fmt::Display`] implementation instead.
    #[must_use]
    pub fn request_url(&self) -> &str {
        &self.request_url
    }

    /// Effective transport protection for disclosure and diagnostics.
    #[must_use]
    pub const fn security(&self) -> TransportSecurity {
        self.security
    }
}

impl fmt::Display for ValidatedEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_url)
    }
}

impl fmt::Debug for ValidatedEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedEndpoint")
            .field("url", &self.display_url)
            .field("security", &self.security)
            .finish_non_exhaustive()
    }
}

/// Why a provider endpoint was rejected before any network work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointError {
    /// No endpoint was configured.
    Empty,
    /// The endpoint exceeded [`MAX_ENDPOINT_BYTES`].
    TooLong,
    /// The URL contained whitespace, non-ASCII text, or a backslash.
    InvalidCharacter,
    /// Only `https` and explicitly governed `http` URLs are supported.
    UnsupportedScheme,
    /// The URL did not contain a network authority.
    MissingAuthority,
    /// Credentials embedded in a URL could leak through diagnostics or redirects.
    EmbeddedCredentials,
    /// The host was malformed or ambiguous.
    InvalidHost,
    /// An explicit port was not in the range 1 through 65535.
    InvalidPort,
    /// URL fragments are not sent to servers and are not valid endpoint data.
    FragmentNotAllowed,
    /// The path or query contained malformed percent encoding.
    InvalidPathOrQuery,
    /// Remote plain HTTP requires an explicit insecure override.
    InsecureRemoteHttp,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "AI provider endpoint is empty",
            Self::TooLong => "AI provider endpoint exceeds the size limit",
            Self::InvalidCharacter => "AI provider endpoint contains an invalid character",
            Self::UnsupportedScheme => "AI provider endpoint must use HTTPS or governed HTTP",
            Self::MissingAuthority => "AI provider endpoint has no network authority",
            Self::EmbeddedCredentials => {
                "AI provider endpoint must not contain embedded credentials"
            }
            Self::InvalidHost => "AI provider endpoint has an invalid host",
            Self::InvalidPort => "AI provider endpoint has an invalid port",
            Self::FragmentNotAllowed => "AI provider endpoint must not contain a fragment",
            Self::InvalidPathOrQuery => "AI provider endpoint has an invalid path or query",
            Self::InsecureRemoteHttp => {
                "plain HTTP AI endpoints require loopback or an explicit insecure override"
            }
        })
    }
}

impl Error for EndpointError {}

/// Validates and canonicalizes a provider base or chat-completion URL.
///
/// A base URL receives a trailing `/chat/completions`. A URL already ending in
/// that path is retained. Redirects remain forbidden by [`PreparedAiRequest`],
/// so a transport cannot use a redirect to bypass this policy.
///
/// # Errors
///
/// Returns a bounded, content-free reason for malformed or unauthorized URLs.
pub fn validate_endpoint(
    endpoint: &str,
    plain_http_policy: PlainHttpPolicy,
) -> Result<ValidatedEndpoint, EndpointError> {
    if endpoint.is_empty() {
        return Err(EndpointError::Empty);
    }
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(EndpointError::TooLong);
    }
    if !endpoint.is_ascii()
        || endpoint
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || byte == b'\\')
    {
        return Err(EndpointError::InvalidCharacter);
    }
    if endpoint.contains('#') {
        return Err(EndpointError::FragmentNotAllowed);
    }

    let (raw_scheme, remainder) = endpoint
        .split_once("://")
        .ok_or(EndpointError::UnsupportedScheme)?;
    let scheme = if raw_scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if raw_scheme.eq_ignore_ascii_case("http") {
        "http"
    } else {
        return Err(EndpointError::UnsupportedScheme);
    };

    let authority_end = remainder.find(['/', '?']).unwrap_or(remainder.len());
    let raw_authority = &remainder[..authority_end];
    if raw_authority.is_empty() {
        return Err(EndpointError::MissingAuthority);
    }
    if raw_authority.contains('@') {
        return Err(EndpointError::EmbeddedCredentials);
    }
    let authority = parse_authority(raw_authority)?;
    let tail = &remainder[authority_end..];
    validate_url_tail(tail)?;

    let (raw_path, query) = tail
        .split_once('?')
        .map_or((tail, None), |(path, query)| (path, Some(query)));
    let path = completion_path(raw_path);
    let base_url = format!("{scheme}://{}{path}", authority.rendered);
    let mut request_url = base_url.clone();
    if let Some(query) = query {
        if !query.is_empty() {
            request_url.push('?');
            request_url.push_str(query);
        }
    }

    let security = if scheme == "https" {
        TransportSecurity::Tls
    } else if authority.loopback {
        TransportSecurity::PlainLoopback
    } else if plain_http_policy == PlainHttpPolicy::ExplicitInsecureOverride {
        TransportSecurity::PlainInsecureOverride
    } else {
        return Err(EndpointError::InsecureRemoteHttp);
    };
    let display_url = if query.is_some_and(|value| !value.is_empty()) {
        format!("{base_url}?<redacted>")
    } else {
        request_url.clone()
    };

    Ok(ValidatedEndpoint {
        request_url,
        display_url,
        security,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedAuthority {
    rendered: String,
    loopback: bool,
}

fn parse_authority(authority: &str) -> Result<ParsedAuthority, EndpointError> {
    if let Some(remainder) = authority.strip_prefix('[') {
        let (host, port) = remainder
            .split_once(']')
            .ok_or(EndpointError::InvalidHost)?;
        let address = host
            .parse::<Ipv6Addr>()
            .map_err(|_| EndpointError::InvalidHost)?;
        let port = parse_port_suffix(port)?;
        return Ok(ParsedAuthority {
            rendered: render_authority(&format!("[{address}]"), port),
            loopback: address.is_loopback(),
        });
    }
    if authority.contains(['[', ']']) || authority.matches(':').count() > 1 {
        return Err(EndpointError::InvalidHost);
    }

    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| {
            (host, Some(parse_port(port)))
        });
    let port = match port {
        Some(result) => Some(result?),
        None => None,
    };
    if host.is_empty() {
        return Err(EndpointError::InvalidHost);
    }

    if let Ok(address) = host.parse::<Ipv4Addr>() {
        return Ok(ParsedAuthority {
            rendered: render_authority(&address.to_string(), port),
            loopback: address.is_loopback(),
        });
    }
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err(EndpointError::InvalidHost);
    }

    let normalized_host = validate_dns_host(host)?;
    Ok(ParsedAuthority {
        loopback: normalized_host == "localhost",
        rendered: render_authority(&normalized_host, port),
    })
}

fn parse_port_suffix(suffix: &str) -> Result<Option<u16>, EndpointError> {
    if suffix.is_empty() {
        return Ok(None);
    }
    let port = suffix.strip_prefix(':').ok_or(EndpointError::InvalidHost)?;
    parse_port(port).map(Some)
}

fn parse_port(port: &str) -> Result<u16, EndpointError> {
    let port = port
        .parse::<u16>()
        .map_err(|_| EndpointError::InvalidPort)?;
    if port == 0 {
        return Err(EndpointError::InvalidPort);
    }
    Ok(port)
}

fn render_authority(host: &str, port: Option<u16>) -> String {
    port.map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"))
}

fn validate_dns_host(host: &str) -> Result<String, EndpointError> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return Err(EndpointError::InvalidHost);
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(EndpointError::InvalidHost);
        }
    }
    Ok(host.to_ascii_lowercase())
}

fn validate_url_tail(tail: &str) -> Result<(), EndpointError> {
    if !tail.is_empty() && !tail.starts_with(['/', '?']) {
        return Err(EndpointError::InvalidPathOrQuery);
    }
    let bytes = tail.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(EndpointError::InvalidPathOrQuery);
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn completion_path(raw_path: &str) -> String {
    let base = raw_path.trim_end_matches('/');
    if base.ends_with(CHAT_COMPLETIONS_PATH) {
        base.to_owned()
    } else if base.is_empty() {
        CHAT_COMPLETIONS_PATH.to_owned()
    } else {
        format!("{base}{CHAT_COMPLETIONS_PATH}")
    }
}

/// Where the selected bearer credential came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialOrigin {
    /// A configured environment lookup supplied the credential.
    Environment,
    /// The discouraged plaintext compatibility field supplied the credential.
    InlineCompatibility,
}

/// Optional bearer authorization for one prepared request.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderAuthorization {
    credential: Credential,
    origin: CredentialOrigin,
}

impl ProviderAuthorization {
    /// Exposes the token only to the HTTP authorization-header boundary.
    #[must_use]
    pub fn expose_bearer_token(&self) -> &str {
        self.credential.expose_secret()
    }

    /// Reports whether environment lookup or compatibility config supplied it.
    #[must_use]
    pub const fn origin(&self) -> CredentialOrigin {
        self.origin
    }
}

impl fmt::Debug for ProviderAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAuthorization")
            .field("credential", &"<redacted>")
            .field("origin", &self.origin)
            .finish()
    }
}

/// OpenAI-compatible chat role fixed by the local prompt contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatRole {
    /// Immutable local completion rules.
    System,
    /// Delimited, untrusted prompt context.
    User,
}

/// One message in an OpenAI-compatible chat-completion body.
#[derive(Clone, Eq, PartialEq)]
pub struct ChatMessage {
    role: ChatRole,
    content: String,
}

impl ChatMessage {
    /// Fixed role assigned by the local prompt builder.
    #[must_use]
    pub const fn role(&self) -> ChatRole {
        self.role
    }

    /// Exact bounded prompt message to serialize.
    #[must_use]
    pub(crate) fn content(&self) -> &str {
        &self.content
    }
}

impl fmt::Debug for ChatMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatMessage")
            .field("role", &self.role)
            .field("content", &"<redacted>")
            .finish()
    }
}

/// Body fields for one OpenAI-compatible chat-completion request.
#[derive(Clone, PartialEq)]
pub struct OpenAiChatBody {
    model: String,
    messages: [ChatMessage; 2],
    extra_request_body: BTreeMap<String, ExtraRequestValue>,
    encoded_body_upper_bound: usize,
}

impl OpenAiChatBody {
    /// Provider model identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Local system and user messages, in required provider order.
    #[must_use]
    pub const fn messages(&self) -> &[ChatMessage; 2] {
        &self.messages
    }

    /// Validated provider-specific fields to merge at the top level.
    #[must_use]
    pub const fn extra_request_body(&self) -> &BTreeMap<String, ExtraRequestValue> {
        &self.extra_request_body
    }

    /// Streaming is locally disabled and cannot be replaced by extra fields.
    #[must_use]
    pub const fn stream(&self) -> bool {
        false
    }

    /// Conservative bound for compact JSON serialization of this body.
    #[must_use]
    pub const fn encoded_body_upper_bound(&self) -> usize {
        self.encoded_body_upper_bound
    }
}

impl fmt::Debug for OpenAiChatBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiChatBody")
            .field("model", &self.model)
            .field("messages", &"<redacted>")
            .field("extra_request_body", &"<redacted>")
            .field("encoded_body_upper_bound", &self.encoded_body_upper_bound)
            .finish()
    }
}

/// Fully validated, owned request ready for an asynchronous HTTP transport.
///
/// The transport must enforce [`Self::follow_redirects`],
/// [`Self::response_body_limit`], and [`Self::error_body_limit`] while reading
/// decoded bytes. It must map client failures into [`SanitizedProviderError`]
/// rather than retaining a raw client error, request URL, or request body.
#[derive(Clone, PartialEq)]
pub struct PreparedAiRequest {
    endpoint: ValidatedEndpoint,
    timeout_ms: u64,
    authorization: Option<ProviderAuthorization>,
    body: OpenAiChatBody,
}

impl PreparedAiRequest {
    /// Validated chat-completions endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &ValidatedEndpoint {
        &self.endpoint
    }

    /// Locally enforced transport timeout.
    #[must_use]
    pub const fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Optional bearer authorization selected without exposing it to diagnostics.
    #[must_use]
    pub const fn authorization(&self) -> Option<&ProviderAuthorization> {
        self.authorization.as_ref()
    }

    /// Validated request body.
    #[must_use]
    pub const fn body(&self) -> &OpenAiChatBody {
        &self.body
    }

    /// Redirect following is forbidden so redirects cannot bypass endpoint policy.
    #[must_use]
    pub const fn follow_redirects(&self) -> bool {
        false
    }

    /// Maximum successful response bytes to read before aborting.
    #[must_use]
    pub const fn response_body_limit(&self) -> usize {
        MAX_PROVIDER_RESPONSE_BODY_BYTES
    }

    /// Maximum error bytes to read before sanitizing diagnostics.
    #[must_use]
    pub const fn error_body_limit(&self) -> usize {
        MAX_PROVIDER_ERROR_BODY_BYTES
    }
}

impl fmt::Debug for PreparedAiRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedAiRequest")
            .field("endpoint", &self.endpoint)
            .field("timeout_ms", &self.timeout_ms)
            .field("authorization", &self.authorization)
            .field("body", &"<redacted>")
            .field("follow_redirects", &false)
            .field("response_body_limit", &MAX_PROVIDER_RESPONSE_BODY_BYTES)
            .field("error_body_limit", &MAX_PROVIDER_ERROR_BODY_BYTES)
            .finish()
    }
}

/// Content-free provider failure suitable for debug logging.
///
/// The HTTP boundary must discard raw client errors and provider bodies after
/// classifying them. Those values can contain the configured URL query, bearer
/// credential, or disclosed prompt context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SanitizedProviderError {
    /// The local transport deadline elapsed.
    Timeout,
    /// DNS, TLS, connection, or other transport setup failed.
    Connection,
    /// The provider returned a rate-limit response.
    RateLimited,
    /// The provider returned another unsuccessful HTTP status.
    HttpStatus(u16),
    /// A successful decoded response exceeded the fixed read limit.
    ResponseBodyTooLarge,
    /// An unsuccessful decoded response exceeded the fixed diagnostic limit.
    ErrorBodyTooLarge,
    /// A bounded successful body did not match the provider protocol.
    InvalidResponse,
}

impl fmt::Display for SanitizedProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => formatter.write_str("AI provider request timed out"),
            Self::Connection => formatter.write_str("AI provider connection failed"),
            Self::RateLimited => formatter.write_str("AI provider rate limited the request"),
            Self::HttpStatus(status) => {
                write!(formatter, "AI provider returned HTTP status {status}")
            }
            Self::ResponseBodyTooLarge => {
                formatter.write_str("AI provider response exceeded the size limit")
            }
            Self::ErrorBodyTooLarge => {
                formatter.write_str("AI provider error response exceeded the size limit")
            }
            Self::InvalidResponse => formatter.write_str("AI provider response was invalid"),
        }
    }
}

impl Error for SanitizedProviderError {}

/// Why a configured provider could not become a safe transport request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestBuildError {
    /// No endpoint was configured.
    MissingEndpoint,
    /// Endpoint safety validation failed.
    Endpoint(EndpointError),
    /// No model was configured.
    MissingModel,
    /// The model was blank, oversized, or contained a control character.
    InvalidModel,
    /// The provider timeout was outside the configured safety range.
    InvalidTimeout,
    /// A credential environment-variable name was malformed.
    InvalidCredentialEnvironment,
    /// A configured credential source produced no usable value.
    MissingCredential,
    /// A credential was blank, oversized, non-ASCII, or unsafe for a header.
    InvalidCredential,
    /// An extra top-level field attempted to replace a local safety field.
    ReservedExtraField,
    /// An extra field name was empty, oversized, or contained a control character.
    InvalidExtraField,
    /// Provider-specific data nested more deeply than permitted.
    ExtraBodyTooDeep,
    /// A provider-specific array or table contained too many entries.
    ExtraContainerTooLarge,
    /// Provider-specific data contained too many aggregate values.
    TooManyExtraValues,
    /// One provider-specific string exceeded its per-value bound.
    ExtraStringTooLong,
    /// A provider-specific float was NaN or infinite.
    NonFiniteExtraFloat,
    /// Provider-specific data exceeded its encoded size bound.
    ExtraBodyTooLarge,
    /// The full encoded request could exceed the transport request bound.
    RequestBodyTooLarge,
}

impl fmt::Display for RequestBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Endpoint(error) = self {
            return write!(formatter, "{error}");
        }
        formatter.write_str(match self {
            Self::MissingEndpoint => "AI provider endpoint is required",
            Self::Endpoint(_) => "AI provider endpoint is invalid",
            Self::MissingModel => "AI provider model is required",
            Self::InvalidModel => "AI provider model is invalid",
            Self::InvalidTimeout => "AI provider timeout is outside the safety range",
            Self::InvalidCredentialEnvironment => {
                "AI credential environment-variable name is invalid"
            }
            Self::MissingCredential => "configured AI credential is unavailable",
            Self::InvalidCredential => "configured AI credential is invalid",
            Self::ReservedExtraField => {
                "AI extra request data attempted to replace a local safety field"
            }
            Self::InvalidExtraField => "AI extra request data has an invalid field name",
            Self::ExtraBodyTooDeep => "AI extra request data exceeds the nesting limit",
            Self::ExtraContainerTooLarge => {
                "AI extra request data contains an oversized array or table"
            }
            Self::TooManyExtraValues => "AI extra request data contains too many values",
            Self::ExtraStringTooLong => "AI extra request data contains an oversized string",
            Self::NonFiniteExtraFloat => "AI extra request data contains a non-finite number",
            Self::ExtraBodyTooLarge => "AI extra request data exceeds the size limit",
            Self::RequestBodyTooLarge => "AI provider request exceeds the size limit",
        })
    }
}

impl Error for RequestBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Endpoint(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EndpointError> for RequestBuildError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

/// Prepares one OpenAI-compatible request without performing external work.
///
/// `environment` is supplied by the caller so this boundary remains pure and
/// testable. A nonblank environment credential takes precedence over the
/// compatibility plaintext field. The latter is used only when the configured
/// environment variable is unavailable.
///
/// # Errors
///
/// Returns a content-free reason when provider configuration, credentials,
/// extra fields, or total request size violate a safety invariant.
pub fn prepare_openai_request<F>(
    provider: &AiProvider,
    prompt: &AiPrompt,
    plain_http_policy: PlainHttpPolicy,
    mut environment: F,
) -> Result<PreparedAiRequest, RequestBuildError>
where
    F: FnMut(&str) -> Option<String>,
{
    match provider.inherited_from {
        ProviderProtocol::OpenAi => {}
    }
    let endpoint = provider
        .endpoint
        .as_deref()
        .ok_or(RequestBuildError::MissingEndpoint)
        .and_then(|value| validate_endpoint(value, plain_http_policy).map_err(Into::into))?;
    if !(MIN_AI_PROVIDER_TIMEOUT_MS..=MAX_AI_PROVIDER_TIMEOUT_MS).contains(&provider.timeout_ms) {
        return Err(RequestBuildError::InvalidTimeout);
    }
    let model = validate_model(
        provider
            .model
            .as_deref()
            .ok_or(RequestBuildError::MissingModel)?,
    )?;
    let authorization = select_authorization(provider, &mut environment)?;
    let extra_encoded_bytes = validate_extra_request_body(&provider.extra_request_body)?;

    let messages = [
        ChatMessage {
            role: ChatRole::System,
            content: prompt.system_message().to_owned(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: prompt.user_message().to_owned(),
        },
    ];
    let encoded_body_upper_bound =
        base_body_upper_bound(&model, &messages).saturating_add(extra_encoded_bytes);
    if encoded_body_upper_bound > MAX_PROVIDER_REQUEST_BODY_BYTES {
        return Err(RequestBuildError::RequestBodyTooLarge);
    }

    Ok(PreparedAiRequest {
        endpoint,
        timeout_ms: provider.timeout_ms,
        authorization,
        body: OpenAiChatBody {
            model,
            messages,
            extra_request_body: provider.extra_request_body.clone(),
            encoded_body_upper_bound,
        },
    })
}

fn validate_model(model: &str) -> Result<String, RequestBuildError> {
    if model.is_empty()
        || model.len() > MAX_MODEL_BYTES
        || model.trim() != model
        || model.chars().any(char::is_control)
    {
        return Err(RequestBuildError::InvalidModel);
    }
    Ok(model.to_owned())
}

fn select_authorization<F>(
    provider: &AiProvider,
    environment: &mut F,
) -> Result<Option<ProviderAuthorization>, RequestBuildError>
where
    F: FnMut(&str) -> Option<String>,
{
    if let Some(name) = provider.api_key_env.as_deref() {
        if !valid_environment_name(name) {
            return Err(RequestBuildError::InvalidCredentialEnvironment);
        }
        if let Some(value) = environment(name) {
            if !value.trim().is_empty() {
                return authorization(value, CredentialOrigin::Environment).map(Some);
            }
        }
        if let Some(credential) = provider.api_key.as_ref() {
            return authorization(
                credential.expose_secret().to_owned(),
                CredentialOrigin::InlineCompatibility,
            )
            .map(Some);
        }
        return Err(RequestBuildError::MissingCredential);
    }

    provider.api_key.as_ref().map_or(Ok(None), |credential| {
        authorization(
            credential.expose_secret().to_owned(),
            CredentialOrigin::InlineCompatibility,
        )
        .map(Some)
    })
}

fn valid_environment_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_CREDENTIAL_ENV_BYTES {
        return false;
    }
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn authorization(
    value: String,
    origin: CredentialOrigin,
) -> Result<ProviderAuthorization, RequestBuildError> {
    if value.is_empty()
        || value.len() > MAX_CREDENTIAL_BYTES
        || !value.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
    {
        return Err(RequestBuildError::InvalidCredential);
    }
    Ok(ProviderAuthorization {
        credential: Credential::new(value),
        origin,
    })
}

#[derive(Default)]
struct ExtraBudget {
    values: usize,
}

fn validate_extra_request_body(
    body: &BTreeMap<String, ExtraRequestValue>,
) -> Result<usize, RequestBuildError> {
    if body.len() > MAX_EXTRA_CONTAINER_ITEMS {
        return Err(RequestBuildError::ExtraContainerTooLarge);
    }
    let mut budget = ExtraBudget::default();
    let mut encoded_bytes = 0_usize;
    for (key, value) in body {
        validate_extra_key(key)?;
        if RESERVED_EXTRA_FIELDS.contains(&key.as_str()) {
            return Err(RequestBuildError::ReservedExtraField);
        }
        encoded_bytes = encoded_bytes
            .saturating_add(1)
            .saturating_add(json_string_upper_bound(key))
            .saturating_add(1)
            .saturating_add(validate_extra_value(value, 1, &mut budget)?);
        if encoded_bytes > MAX_EXTRA_REQUEST_BODY_BYTES {
            return Err(RequestBuildError::ExtraBodyTooLarge);
        }
    }
    Ok(encoded_bytes)
}

fn validate_extra_value(
    value: &ExtraRequestValue,
    depth: usize,
    budget: &mut ExtraBudget,
) -> Result<usize, RequestBuildError> {
    if depth > MAX_EXTRA_REQUEST_DEPTH {
        return Err(RequestBuildError::ExtraBodyTooDeep);
    }
    budget.values = budget.values.saturating_add(1);
    if budget.values > MAX_EXTRA_REQUEST_VALUES {
        return Err(RequestBuildError::TooManyExtraValues);
    }

    match value {
        ExtraRequestValue::String(value) => {
            if value.len() > MAX_EXTRA_STRING_BYTES {
                return Err(RequestBuildError::ExtraStringTooLong);
            }
            Ok(json_string_upper_bound(value))
        }
        ExtraRequestValue::Integer(value) => Ok(value.to_string().len()),
        ExtraRequestValue::Float(value) => {
            if !value.is_finite() {
                return Err(RequestBuildError::NonFiniteExtraFloat);
            }
            Ok(32)
        }
        ExtraRequestValue::Boolean(value) => Ok(if *value { 4 } else { 5 }),
        ExtraRequestValue::Array(values) => {
            if values.len() > MAX_EXTRA_CONTAINER_ITEMS {
                return Err(RequestBuildError::ExtraContainerTooLarge);
            }
            let mut encoded = 2_usize.saturating_add(values.len().saturating_sub(1));
            for value in values {
                encoded = encoded.saturating_add(validate_extra_value(value, depth + 1, budget)?);
                if encoded > MAX_EXTRA_REQUEST_BODY_BYTES {
                    return Err(RequestBuildError::ExtraBodyTooLarge);
                }
            }
            Ok(encoded)
        }
        ExtraRequestValue::Table(values) => {
            if values.len() > MAX_EXTRA_CONTAINER_ITEMS {
                return Err(RequestBuildError::ExtraContainerTooLarge);
            }
            let mut encoded = 2_usize.saturating_add(values.len().saturating_sub(1));
            for (key, value) in values {
                validate_extra_key(key)?;
                encoded = encoded
                    .saturating_add(json_string_upper_bound(key))
                    .saturating_add(1)
                    .saturating_add(validate_extra_value(value, depth + 1, budget)?);
                if encoded > MAX_EXTRA_REQUEST_BODY_BYTES {
                    return Err(RequestBuildError::ExtraBodyTooLarge);
                }
            }
            Ok(encoded)
        }
    }
}

fn validate_extra_key(key: &str) -> Result<(), RequestBuildError> {
    if key.is_empty() || key.len() > MAX_EXTRA_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(RequestBuildError::InvalidExtraField);
    }
    Ok(())
}

fn base_body_upper_bound(model: &str, messages: &[ChatMessage; 2]) -> usize {
    // Compact JSON punctuation, fixed keys, roles, and `stream: false`.
    112_usize
        .saturating_add(json_string_upper_bound(model))
        .saturating_add(json_string_upper_bound(messages[0].content()))
        .saturating_add(json_string_upper_bound(messages[1].content()))
}

fn json_string_upper_bound(value: &str) -> usize {
    value.chars().fold(2_usize, |length, character| {
        let encoded = match character {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            character if character.is_control() => 6,
            character => character.len_utf8(),
        };
        length.saturating_add(encoded)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_prompt::{GatheredPromptContext, build_prompt};
    use crate::config::AiContextLevel;

    fn prompt() -> AiPrompt {
        build_prompt(
            AiContextLevel::Minimal,
            &GatheredPromptContext {
                input: "git status".to_owned(),
                shell: "zsh".to_owned(),
                operating_system: "macos-aarch64".to_owned(),
                ..GatheredPromptContext::default()
            },
        )
        .unwrap()
    }

    fn provider(endpoint: &str) -> AiProvider {
        AiProvider {
            endpoint: Some(endpoint.to_owned()),
            model: Some("dean-v2".to_owned()),
            ..AiProvider::default()
        }
    }

    #[test]
    fn canonicalizes_cloud_base_and_completion_endpoints() {
        let cases = BTreeMap::from([
            (
                "base",
                (
                    "HTTPS://API.GROQ.COM/openai/v1",
                    "https://api.groq.com/openai/v1/chat/completions",
                ),
            ),
            (
                "completion",
                (
                    "https://example.com/v1/chat/completions/",
                    "https://example.com/v1/chat/completions",
                ),
            ),
            (
                "root",
                (
                    "https://example.com",
                    "https://example.com/chat/completions",
                ),
            ),
        ]);

        for (label, (endpoint, want)) in cases {
            let got = validate_endpoint(endpoint, PlainHttpPolicy::LoopbackOnly).unwrap();
            assert_eq!(got.request_url(), want, "{label}");
            assert_eq!(got.security(), TransportSecurity::Tls, "{label}");
        }
    }

    #[test]
    fn admits_only_canonical_loopback_hosts_over_plain_http_by_default() {
        let allowed = [
            "http://localhost:11434/v1",
            "http://localhost./v1",
            "http://127.0.0.1/v1",
            "http://127.42.7.9/v1",
            "http://[::1]:11434/v1",
        ];
        for endpoint in allowed {
            let got = validate_endpoint(endpoint, PlainHttpPolicy::LoopbackOnly).unwrap();
            assert_eq!(
                got.security(),
                TransportSecurity::PlainLoopback,
                "{endpoint}"
            );
        }

        let rejected = [
            "http://example.com/v1",
            "http://localhost.example/v1",
            "http://127.0.0.1.example/v1",
            "http://[::ffff:127.0.0.1]/v1",
        ];
        for endpoint in rejected {
            assert_eq!(
                validate_endpoint(endpoint, PlainHttpPolicy::LoopbackOnly),
                Err(EndpointError::InsecureRemoteHttp),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn requires_an_explicit_policy_value_for_remote_plain_http() {
        let endpoint = validate_endpoint(
            "http://ollama.greendale.test:11434/v1",
            PlainHttpPolicy::ExplicitInsecureOverride,
        )
        .unwrap();

        assert_eq!(
            endpoint.request_url(),
            "http://ollama.greendale.test:11434/v1/chat/completions"
        );
        assert_eq!(
            endpoint.security(),
            TransportSecurity::PlainInsecureOverride
        );
    }

    #[test]
    fn rejects_ambiguous_or_malformed_endpoints_without_echoing_them() {
        let cases = BTreeMap::from([
            (
                "embedded credentials",
                (
                    "https://dean:secret@example.com/v1",
                    EndpointError::EmbeddedCredentials,
                ),
            ),
            (
                "fragment",
                (
                    "https://example.com/v1#secret",
                    EndpointError::FragmentNotAllowed,
                ),
            ),
            (
                "invalid host",
                ("https://127.0.0.999/v1", EndpointError::InvalidHost),
            ),
            (
                "invalid percent",
                (
                    "https://example.com/v1/%xx",
                    EndpointError::InvalidPathOrQuery,
                ),
            ),
            (
                "invalid port",
                ("https://example.com:0/v1", EndpointError::InvalidPort),
            ),
            (
                "missing authority",
                ("https:///v1", EndpointError::MissingAuthority),
            ),
            (
                "raw whitespace",
                ("https://example.com/a b", EndpointError::InvalidCharacter),
            ),
            (
                "relative",
                ("example.com/v1", EndpointError::UnsupportedScheme),
            ),
            (
                "unbracketed ipv6",
                ("https://::1/v1", EndpointError::InvalidHost),
            ),
            (
                "unsupported scheme",
                ("file:///tmp/secret", EndpointError::UnsupportedScheme),
            ),
        ]);

        for (label, (endpoint, want)) in cases {
            let error = validate_endpoint(endpoint, PlainHttpPolicy::LoopbackOnly).unwrap_err();
            assert_eq!(error, want, "{label}");
            assert!(!error.to_string().contains(endpoint), "{label}");
        }
    }

    #[test]
    fn preserves_query_for_transport_but_redacts_it_for_display() {
        let endpoint = validate_endpoint(
            "https://example.com/v1?api-key=chang-loves-security",
            PlainHttpPolicy::LoopbackOnly,
        )
        .unwrap();

        assert_eq!(
            endpoint.request_url(),
            "https://example.com/v1/chat/completions?api-key=chang-loves-security"
        );
        assert_eq!(
            endpoint.to_string(),
            "https://example.com/v1/chat/completions?<redacted>"
        );
        assert!(!format!("{endpoint:?}").contains("chang-loves-security"));
    }

    #[test]
    fn prepares_fixed_messages_timeout_and_transport_limits() {
        let request = prepare_openai_request(
            &provider("https://api.openai.test/v1"),
            &prompt(),
            PlainHttpPolicy::LoopbackOnly,
            |_| None,
        )
        .unwrap();

        assert_eq!(request.body().model(), "dean-v2");
        assert_eq!(request.body().messages()[0].role(), ChatRole::System);
        assert_eq!(request.body().messages()[1].role(), ChatRole::User);
        assert!(
            request.body().messages()[1]
                .content()
                .contains("git status")
        );
        assert!(!request.body().stream());
        assert!(!request.follow_redirects());
        assert_eq!(request.timeout_ms(), 2_000);
        assert_eq!(
            request.response_body_limit(),
            MAX_PROVIDER_RESPONSE_BODY_BYTES
        );
        assert_eq!(request.error_body_limit(), MAX_PROVIDER_ERROR_BODY_BYTES);
        assert!(request.body().encoded_body_upper_bound() < MAX_PROVIDER_REQUEST_BODY_BYTES);
    }

    #[test]
    fn environment_credentials_take_precedence_with_inline_fallback() {
        let mut configured = provider("https://api.openai.test/v1");
        configured.api_key_env = Some("GREENDALE_API_KEY".to_owned());
        configured.api_key = Some(Credential::new("inline-secret"));

        let from_environment = prepare_openai_request(
            &configured,
            &prompt(),
            PlainHttpPolicy::LoopbackOnly,
            |name| (name == "GREENDALE_API_KEY").then(|| "environment-secret".to_owned()),
        )
        .unwrap();
        let authorization = from_environment.authorization().unwrap();
        assert_eq!(authorization.expose_bearer_token(), "environment-secret");
        assert_eq!(authorization.origin(), CredentialOrigin::Environment);

        let from_inline = prepare_openai_request(
            &configured,
            &prompt(),
            PlainHttpPolicy::LoopbackOnly,
            |_| None,
        )
        .unwrap();
        let authorization = from_inline.authorization().unwrap();
        assert_eq!(authorization.expose_bearer_token(), "inline-secret");
        assert_eq!(
            authorization.origin(),
            CredentialOrigin::InlineCompatibility
        );
    }

    #[test]
    fn credential_failures_never_include_credential_content() {
        let mut configured = provider("https://api.openai.test/v1");
        configured.api_key_env = Some("GREENDALE_API_KEY".to_owned());
        let error = prepare_openai_request(
            &configured,
            &prompt(),
            PlainHttpPolicy::LoopbackOnly,
            |_| Some("secret\r\ninjected: value".to_owned()),
        )
        .unwrap_err();

        assert_eq!(error, RequestBuildError::InvalidCredential);
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn request_boundary_defensively_rejects_incomplete_provider_configuration() {
        enum Case {
            MissingEndpoint,
            MissingModel,
            InvalidModel,
            InvalidTimeout,
            InvalidCredentialEnvironment,
            MissingCredential,
        }

        let cases = BTreeMap::from([
            (
                "invalid credential environment",
                (
                    Case::InvalidCredentialEnvironment,
                    RequestBuildError::InvalidCredentialEnvironment,
                ),
            ),
            (
                "invalid model",
                (Case::InvalidModel, RequestBuildError::InvalidModel),
            ),
            (
                "invalid timeout",
                (Case::InvalidTimeout, RequestBuildError::InvalidTimeout),
            ),
            (
                "missing credential",
                (
                    Case::MissingCredential,
                    RequestBuildError::MissingCredential,
                ),
            ),
            (
                "missing endpoint",
                (Case::MissingEndpoint, RequestBuildError::MissingEndpoint),
            ),
            (
                "missing model",
                (Case::MissingModel, RequestBuildError::MissingModel),
            ),
        ]);

        for (label, (case, want)) in cases {
            let mut configured = provider("https://api.openai.test/v1");
            match case {
                Case::MissingEndpoint => configured.endpoint = None,
                Case::MissingModel => configured.model = None,
                Case::InvalidModel => configured.model = Some("dean\nsecret".to_owned()),
                Case::InvalidTimeout => configured.timeout_ms = 0,
                Case::InvalidCredentialEnvironment => {
                    configured.api_key_env = Some("1_INVALID".to_owned());
                }
                Case::MissingCredential => {
                    configured.api_key_env = Some("GREENDALE_API_KEY".to_owned());
                }
            }

            let got = prepare_openai_request(
                &configured,
                &prompt(),
                PlainHttpPolicy::LoopbackOnly,
                |_| None,
            )
            .unwrap_err();
            assert_eq!(got, want, "{label}");
        }
    }

    #[test]
    fn provider_diagnostics_retain_only_a_safe_failure_classification() {
        let errors = [
            SanitizedProviderError::Timeout,
            SanitizedProviderError::Connection,
            SanitizedProviderError::RateLimited,
            SanitizedProviderError::HttpStatus(503),
            SanitizedProviderError::ResponseBodyTooLarge,
            SanitizedProviderError::ErrorBodyTooLarge,
            SanitizedProviderError::InvalidResponse,
        ];

        for error in errors {
            assert!(error.to_string().starts_with("AI provider"));
            assert!(!format!("{error:?}").contains("Troy Barnes"));
        }
    }

    #[test]
    fn debug_output_redacts_prompt_credential_extra_data_and_query() {
        let mut configured = provider("https://api.openai.test/v1?api-key=query-secret");
        configured.api_key = Some(Credential::new("header-secret"));
        configured.extra_request_body.insert(
            "metadata".to_owned(),
            ExtraRequestValue::String("extra-secret".to_owned()),
        );
        let request = prepare_openai_request(
            &configured,
            &prompt(),
            PlainHttpPolicy::LoopbackOnly,
            |_| None,
        )
        .unwrap();

        let debug = format!("{request:?}");
        for secret in [
            "git status",
            "query-secret",
            "header-secret",
            "extra-secret",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn extra_fields_cannot_replace_locally_enforced_fields() {
        for field in RESERVED_EXTRA_FIELDS {
            let mut configured = provider("https://api.openai.test/v1");
            configured
                .extra_request_body
                .insert(field.to_owned(), ExtraRequestValue::Boolean(true));

            assert_eq!(
                prepare_openai_request(
                    &configured,
                    &prompt(),
                    PlainHttpPolicy::LoopbackOnly,
                    |_| None,
                ),
                Err(RequestBuildError::ReservedExtraField),
                "{field}"
            );
        }
    }

    #[test]
    fn bounds_and_validates_nested_provider_specific_data() {
        let cases = BTreeMap::from([
            (
                "non-finite float",
                (
                    ExtraRequestValue::Float(f64::NAN),
                    RequestBuildError::NonFiniteExtraFloat,
                ),
            ),
            (
                "oversized string",
                (
                    ExtraRequestValue::String("x".repeat(MAX_EXTRA_STRING_BYTES + 1)),
                    RequestBuildError::ExtraStringTooLong,
                ),
            ),
            (
                "oversized array",
                (
                    ExtraRequestValue::Array(vec![
                        ExtraRequestValue::Boolean(true);
                        MAX_EXTRA_CONTAINER_ITEMS + 1
                    ]),
                    RequestBuildError::ExtraContainerTooLarge,
                ),
            ),
        ]);

        for (label, (value, want)) in cases {
            let mut configured = provider("https://api.openai.test/v1");
            configured
                .extra_request_body
                .insert("option".to_owned(), value);
            let error = prepare_openai_request(
                &configured,
                &prompt(),
                PlainHttpPolicy::LoopbackOnly,
                |_| None,
            )
            .unwrap_err();
            assert_eq!(error, want, "{label}");
        }

        let mut nested = ExtraRequestValue::Boolean(true);
        for _ in 0..MAX_EXTRA_REQUEST_DEPTH {
            nested = ExtraRequestValue::Array(vec![nested]);
        }
        let mut configured = provider("https://api.openai.test/v1");
        configured
            .extra_request_body
            .insert("option".to_owned(), nested);
        assert_eq!(
            prepare_openai_request(
                &configured,
                &prompt(),
                PlainHttpPolicy::LoopbackOnly,
                |_| None,
            ),
            Err(RequestBuildError::ExtraBodyTooDeep)
        );
    }
}
