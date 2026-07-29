//! Bounded synchronous transport for prepared OpenAI-compatible requests.
//!
//! Callers run this blocking boundary away from input forwarding and use the AI
//! lifecycle generation to discard a response that has lost authority. This
//! module follows no redirects and retains no raw client error or error body.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Number, Value};

use crate::ai_provider::{
    ChatRole, MAX_PROVIDER_REQUEST_BODY_BYTES, OpenAiChatBody, PreparedAiRequest,
    SanitizedProviderError,
};
use crate::config::ExtraRequestValue;

const MAX_TRANSPORT_WORKERS: usize = 32;
static ACTIVE_TRANSPORT_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// One bounded completion payload returned by a compatible provider.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderCompletion {
    content: String,
}

impl ProviderCompletion {
    /// Exact provider content for the later inert-output validator.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Consumes the response and returns its exact bounded content.
    #[must_use]
    pub fn into_content(self) -> String {
        self.content
    }
}

impl fmt::Debug for ProviderCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCompletion")
            .field("content_bytes", &self.content.len())
            .finish()
    }
}

/// Performs one prepared provider request with its local timeout and body
/// limits.
///
/// This function is blocking. The interactive runtime must execute it on owned
/// background work and continue to validate lifecycle authority afterward.
/// Redirects are never followed, including redirects back to the same host.
///
/// # Errors
///
/// Returns only a content-free classification. Raw transport failures, URLs,
/// authorization values, prompt data, and provider error bodies are discarded.
pub fn execute_openai_request(
    request: &PreparedAiRequest,
) -> Result<ProviderCompletion, SanitizedProviderError> {
    let started = Instant::now();
    if request.follow_redirects() {
        return Err(SanitizedProviderError::InvalidRequest);
    }
    let body = encode_body(request.body())?;
    let timeout = Duration::from_millis(request.timeout_ms());
    let deadline = started
        .checked_add(timeout)
        .ok_or(SanitizedProviderError::InvalidRequest)?;
    let worker_request = TransportRequest {
        request_url: request.endpoint().request_url().to_owned(),
        authorization: request
            .authorization()
            .map(|authorization| authorization.expose_bearer_token().to_owned()),
        body,
        deadline,
        response_body_limit: request.response_body_limit(),
        error_body_limit: request.error_body_limit(),
    };
    let permit = TransportWorkerPermit::acquire().ok_or(SanitizedProviderError::Timeout)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("argmax-ai-transport".to_owned())
        .spawn(move || {
            let _permit = permit;
            let result = execute_transport_request(worker_request);
            let _send_result = sender.send(result);
        })
        .map_err(|_| SanitizedProviderError::Connection)?;

    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(SanitizedProviderError::Timeout)?;
    let result = receiver
        .recv_timeout(remaining)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => SanitizedProviderError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => SanitizedProviderError::Connection,
        })?;
    if started.elapsed() >= timeout {
        return Err(SanitizedProviderError::Timeout);
    }
    result
}

struct TransportRequest {
    request_url: String,
    authorization: Option<String>,
    body: Vec<u8>,
    deadline: Instant,
    response_body_limit: usize,
    error_body_limit: usize,
}

struct TransportWorkerPermit;

impl TransportWorkerPermit {
    fn acquire() -> Option<Self> {
        ACTIVE_TRANSPORT_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_TRANSPORT_WORKERS).then_some(active + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for TransportWorkerPermit {
    fn drop(&mut self) {
        ACTIVE_TRANSPORT_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn execute_transport_request(
    request: TransportRequest,
) -> Result<ProviderCompletion, SanitizedProviderError> {
    let remaining = request.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(SanitizedProviderError::Timeout);
    }
    let agent: ureq::Agent = transport_config(remaining).into();

    let mut outgoing = agent
        .post(&request.request_url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json");
    if let Some(authorization) = request.authorization {
        let value = format!("Bearer {authorization}");
        outgoing = outgoing.header("Authorization", &value);
    }
    if Instant::now() >= request.deadline {
        return Err(SanitizedProviderError::Timeout);
    }

    let response = outgoing
        .send(&request.body)
        .map_err(classify_transport_error)?;
    drop(request.body);
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        if status == 429 {
            // Once the provider has identified rate limiting, an untrusted
            // error body must not suppress cooldown or consume the deadline.
            return Err(SanitizedProviderError::RateLimited);
        }
        read_and_discard(response, request.error_body_limit, request.deadline)?;
        return Err(SanitizedProviderError::HttpStatus(status));
    }

    let bytes = read_response(response, request.response_body_limit, request.deadline)?;
    let decoded: ProviderEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| SanitizedProviderError::InvalidResponse)?;
    let content = decoded
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .ok_or(SanitizedProviderError::InvalidResponse)?;
    Ok(ProviderCompletion { content })
}

fn transport_config(timeout: Duration) -> ureq::config::Config {
    ureq::Agent::config_builder()
        // Environment proxies can route a validated loopback request, bearer
        // credential, and prompt off-machine. Proxy use is not part of the
        // configured provider endpoint contract.
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
}

fn encode_body(body: &OpenAiChatBody) -> Result<Vec<u8>, SanitizedProviderError> {
    let mut object = Map::new();
    object.insert("model".to_owned(), Value::String(body.model().to_owned()));
    object.insert("stream".to_owned(), Value::Bool(body.stream()));
    object.insert(
        "messages".to_owned(),
        Value::Array(
            body.messages()
                .iter()
                .map(|message| {
                    let mut encoded = Map::new();
                    let role = match message.role() {
                        ChatRole::System => "system",
                        ChatRole::User => "user",
                    };
                    encoded.insert("role".to_owned(), Value::String(role.to_owned()));
                    encoded.insert(
                        "content".to_owned(),
                        Value::String(message.content().to_owned()),
                    );
                    Value::Object(encoded)
                })
                .collect(),
        ),
    );
    for (name, value) in body.extra_request_body() {
        object.insert(name.clone(), encode_extra_value(value)?);
    }

    let encoded = serde_json::to_vec(&Value::Object(object))
        .map_err(|_| SanitizedProviderError::InvalidRequest)?;
    if encoded.len() > body.encoded_body_upper_bound()
        || encoded.len() > MAX_PROVIDER_REQUEST_BODY_BYTES
    {
        return Err(SanitizedProviderError::InvalidRequest);
    }
    Ok(encoded)
}

fn encode_extra_value(value: &ExtraRequestValue) -> Result<Value, SanitizedProviderError> {
    match value {
        ExtraRequestValue::String(value) => Ok(Value::String(value.clone())),
        ExtraRequestValue::Integer(value) => Ok(Value::Number((*value).into())),
        ExtraRequestValue::Float(value) => Number::from_f64(*value)
            .map(Value::Number)
            .ok_or(SanitizedProviderError::InvalidRequest),
        ExtraRequestValue::Boolean(value) => Ok(Value::Bool(*value)),
        ExtraRequestValue::Array(values) => values
            .iter()
            .map(encode_extra_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ExtraRequestValue::Table(values) => encode_extra_table(values).map(Value::Object),
    }
}

fn encode_extra_table(
    values: &BTreeMap<String, ExtraRequestValue>,
) -> Result<Map<String, Value>, SanitizedProviderError> {
    values
        .iter()
        .map(|(name, value)| Ok((name.clone(), encode_extra_value(value)?)))
        .collect()
}

fn read_response(
    response: ureq::http::Response<ureq::Body>,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>, SanitizedProviderError> {
    read_bounded(response, limit, deadline).map_err(|error| match error {
        BodyReadError::TooLarge => SanitizedProviderError::ResponseBodyTooLarge,
        BodyReadError::Timeout => SanitizedProviderError::Timeout,
        BodyReadError::Connection => SanitizedProviderError::Connection,
    })
}

fn read_and_discard(
    response: ureq::http::Response<ureq::Body>,
    limit: usize,
    deadline: Instant,
) -> Result<(), SanitizedProviderError> {
    read_bounded(response, limit, deadline)
        .map(|_| ())
        .map_err(|error| match error {
            BodyReadError::TooLarge => SanitizedProviderError::ErrorBodyTooLarge,
            BodyReadError::Timeout => SanitizedProviderError::Timeout,
            BodyReadError::Connection => SanitizedProviderError::Connection,
        })
}

enum BodyReadError {
    TooLarge,
    Timeout,
    Connection,
}

fn read_bounded(
    response: ureq::http::Response<ureq::Body>,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>, BodyReadError> {
    let read_limit = limit.checked_add(1).ok_or(BodyReadError::TooLarge)?;
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::with_capacity(read_limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(BodyReadError::Timeout);
        }
        let remaining = read_limit - bytes.len();
        if remaining == 0 {
            return Err(BodyReadError::TooLarge);
        }
        let chunk_limit = remaining.min(buffer.len());
        let read = reader
            .read(&mut buffer[..chunk_limit])
            .map_err(|error| classify_body_io_error(&error))?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > limit {
            return Err(BodyReadError::TooLarge);
        }
    }
}

fn classify_body_io_error(error: &std::io::Error) -> BodyReadError {
    if error.kind() == std::io::ErrorKind::TimedOut
        || matches!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<ureq::Error>()),
            Some(ureq::Error::Timeout(_))
        )
    {
        BodyReadError::Timeout
    } else {
        BodyReadError::Connection
    }
}

fn classify_transport_error(error: ureq::Error) -> SanitizedProviderError {
    match error {
        ureq::Error::Timeout(_) => SanitizedProviderError::Timeout,
        ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            SanitizedProviderError::Timeout
        }
        ureq::Error::StatusCode(429) => SanitizedProviderError::RateLimited,
        ureq::Error::StatusCode(status) => SanitizedProviderError::HttpStatus(status),
        ureq::Error::BodyExceedsLimit(_) => SanitizedProviderError::ResponseBodyTooLarge,
        _ => SanitizedProviderError::Connection,
    }
}

#[derive(Deserialize)]
struct ProviderEnvelope {
    choices: Vec<ProviderChoice>,
}

#[derive(Deserialize)]
struct ProviderChoice {
    message: ProviderMessage,
}

#[derive(Deserialize)]
struct ProviderMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use crate::ai_prompt::{GatheredPromptContext, build_prompt};
    use crate::ai_provider::{PlainHttpPolicy, prepare_openai_request};
    use crate::config::{AiContextLevel, AiProvider, Credential};

    use super::*;

    const MAX_TEST_REQUEST_BYTES: usize = 512 * 1024;

    struct TestServer {
        endpoint: String,
        request: mpsc::Receiver<Vec<u8>>,
        worker: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn respond(status: u16, body: Vec<u8>, extra_headers: &[(&str, &str)]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let endpoint = format!("http://{address}/v1");
            let (sender, request) = mpsc::sync_channel(1);
            let headers = extra_headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect::<Vec<_>>();
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let captured = read_request(&mut stream);
                sender.send(captured).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                )
                .unwrap();
                for (name, value) in headers {
                    write!(stream, "{name}: {value}\r\n").unwrap();
                }
                stream.write_all(b"\r\n").unwrap();
                stream.write_all(&body).unwrap();
            });
            Self {
                endpoint,
                request,
                worker,
            }
        }

        fn finish(self) -> Vec<u8> {
            let request = self.request.recv().unwrap();
            self.worker.join().unwrap();
            request
        }
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                return bytes;
            }
            bytes.extend_from_slice(&buffer[..read]);
            assert!(bytes.len() <= MAX_TEST_REQUEST_BYTES);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            assert!(bytes.len() <= MAX_TEST_REQUEST_BYTES);
        }
        bytes
    }

    fn prepared(endpoint: &str, timeout_ms: u64) -> PreparedAiRequest {
        let prompt = build_prompt(
            AiContextLevel::Minimal,
            &GatheredPromptContext {
                input: "git che".to_owned(),
                shell: "zsh".to_owned(),
                operating_system: "test".to_owned(),
                ..GatheredPromptContext::default()
            },
        )
        .unwrap();
        let provider = AiProvider {
            endpoint: Some(endpoint.to_owned()),
            api_key: Some(Credential::new("DeanBearerSecret")),
            model: Some("greendale-model".to_owned()),
            timeout_ms,
            ..AiProvider::default()
        };
        prepare_openai_request(&provider, &prompt, PlainHttpPolicy::LoopbackOnly, |_| None).unwrap()
    }

    #[test]
    fn sends_exact_bounded_json_and_returns_only_message_content() {
        let server = TestServer::respond(
            200,
            br#"{"choices":[{"message":{"content":"git checkout main"}}]}"#.to_vec(),
            &[],
        );
        let request = prepared(&server.endpoint, 1_000);
        let completion = execute_openai_request(&request).unwrap();
        assert_eq!(completion.content(), "git checkout main");
        assert!(!format!("{completion:?}").contains("checkout"));

        let captured = String::from_utf8(server.finish()).unwrap();
        assert!(captured.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(
            captured
                .to_ascii_lowercase()
                .contains("authorization: bearer deanbearersecret\r\n")
        );
        let body = captured.split_once("\r\n\r\n").unwrap().1;
        let value: Value = serde_json::from_str(body).unwrap();
        assert_eq!(value["model"], "greendale-model");
        assert_eq!(value["stream"], false);
        assert_eq!(value["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn classifies_statuses_after_boundedly_discarding_error_bodies() {
        let server = TestServer::respond(429, b"slow down".to_vec(), &[]);
        let request = prepared(&server.endpoint, 1_000);
        assert_eq!(
            execute_openai_request(&request),
            Err(SanitizedProviderError::RateLimited)
        );
        server.finish();

        let server = TestServer::respond(429, vec![b'x'; 8 * 1024 + 1], &[]);
        let request = prepared(&server.endpoint, 1_000);
        assert_eq!(
            execute_openai_request(&request),
            Err(SanitizedProviderError::RateLimited)
        );
        server.finish();

        let server = TestServer::respond(503, vec![b'x'; 8 * 1024 + 1], &[]);
        let request = prepared(&server.endpoint, 1_000);
        assert_eq!(
            execute_openai_request(&request),
            Err(SanitizedProviderError::ErrorBodyTooLarge)
        );
        server.finish();
    }

    #[test]
    fn rejects_oversized_malformed_empty_and_redirect_responses() {
        for (status, body, want) in [
            (
                200,
                vec![b'x'; 64 * 1024 + 1],
                SanitizedProviderError::ResponseBodyTooLarge,
            ),
            (
                200,
                br#"{"choices":[]}"#.to_vec(),
                SanitizedProviderError::InvalidResponse,
            ),
            (307, Vec::new(), SanitizedProviderError::HttpStatus(307)),
        ] {
            let server = TestServer::respond(status, body, &[("Location", "http://127.0.0.1:9/")]);
            let request = prepared(&server.endpoint, 1_000);
            assert_eq!(execute_openai_request(&request), Err(want));
            server.finish();
        }
    }

    #[test]
    fn enforces_the_prepared_global_timeout_without_retaining_client_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let accept_started = Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if accept_started.elapsed() >= Duration::from_millis(50) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept fake provider connection: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let _ = read_request(&mut stream);
            thread::sleep(Duration::from_millis(150));
            let body = br#"{"choices":[{"message":{"content":"too late"}}]}"#;
            let header = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if header.is_ok() {
                let _write_result = stream.write_all(body);
            }
        });
        let request = prepared(&endpoint, 1);
        let started = Instant::now();
        assert_eq!(
            execute_openai_request(&request),
            Err(SanitizedProviderError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        worker.join().unwrap();
        let debug = format!("{:?}", execute_openai_request(&request).unwrap_err());
        assert!(!debug.contains("DeanBearerSecret"));
        assert!(!debug.contains(&endpoint));
    }

    #[test]
    fn transport_configuration_does_not_inherit_environment_proxies() {
        let config = transport_config(Duration::from_secs(1));
        assert!(config.proxy().is_none());
    }
}
