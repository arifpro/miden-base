use std::time::Duration;

use opentelemetry::{trace::TracerProvider as _, KeyValue};
use opentelemetry_sdk::{
    runtime,
    trace::{RandomIdGenerator, Sampler, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::{
    resource::{SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use pingora::{http::ResponseHeader, Error, ErrorType};
use pingora_proxy::Session;
use tonic::transport::Channel;
use tonic_health::pb::health_client::HealthClient;
use tracing_subscriber::{layer::SubscriberExt, Registry};

use crate::{error::TxProverServiceError, proxy::metrics::QUEUE_DROP_COUNT};

pub const MIDEN_PROVING_SERVICE: &str = "miden-proving-service";

const RESOURCE_EXHAUSTED_CODE: u16 = 8;

/// Name of the configuration file
pub const PROVING_SERVICE_CONFIG_FILE_NAME: &str = "miden-proving-service.toml";

/// Initializes and configures the global tracing and telemetry system for the CLI, worker and
/// proxy services.
///
/// This function sets up a tracing pipeline that includes:
///
/// - An OpenTelemetry (OTLP) exporter, which sends span data to an OTLP endpoint using gRPC.
/// - A [TracerProvider] configured with a [Sampler::ParentBased] sampler at a `1.0` sampling ratio,
///   ensuring that all traces are recorded.
/// - A resource containing the service name and version extracted from the crate's metadata.
/// - A `tracing` subscriber that integrates the configured [TracerProvider] with the Rust `tracing`
///   ecosystem, applying filters from the environment and enabling formatted console logs.
///
/// **Process:**
/// 1. **OTLP Exporter**:   Creates an OTLP span exporter that sends trace data to a collector
///    endpoint. If it fails to create the exporter, returns an error describing the failure.
///
/// 2. **Resource Setup**:   Creates a [Resource] containing service metadata (name and version),
///    which is attached to all emitted telemetry data to identify the originating service.
///
/// 3. **TracerProvider and Sampler**:   Builds a [TracerProvider] using a [Sampler::ParentBased]
///    sampler layered over a [Sampler::TraceIdRatioBased] sampler set to `1.0`, ensuring all traces
///    are recorded. A random ID generator is used to produce trace and span IDs. The tracer is
///    retrieved from this provider, which can then be used by the OpenTelemetry layer of `tracing`.
///
/// 4. **Telemetry Integration with tracing**:   Creates a telemetry layer from
///    `tracing_opentelemetry` and combines it with a `Registry` subscriber and a formatting layer.
///    This results in a subscriber stack that:
///    - Sends telemetry to the OTLP exporter.
///    - Filters logs/spans based on environment variables.
///    - Pretty-prints formatted logs to stdout.
///
/// 5. **Global Subscriber**:   Finally, sets this composite subscriber as the global default. If
///    this fails (e.g., if a global subscriber is already set), an error will be returned.
///
/// **Returns:**
/// - `Ok(())` if the global subscriber is successfully set up.
/// - `Err(String)` describing the failure if any step (creating the exporter or setting the
///   subscriber) fails.
pub(crate) fn setup_tracing() -> Result<(), String> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .map_err(|e| format!("Failed to create OTLP exporter: {:?}", e))?;

    let resource = Resource::from_schema_url(
        [
            KeyValue::new(SERVICE_NAME, env!("CARGO_PKG_NAME")),
            KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        ],
        SCHEMA_URL,
    );

    let provider = TracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(1.0))))
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .with_batch_exporter(exporter, runtime::Tokio)
        .build();

    let tracer = provider.tracer(MIDEN_PROVING_SERVICE);

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let subscriber = Registry::default()
        .with(telemetry)
        .with(tracing_subscriber::filter::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer());

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| format!("Failed to set subscriber: {:?}", e))
}

/// Create a 503 response for a full queue
pub(crate) async fn create_queue_full_response(
    session: &mut Session,
) -> pingora_core::Result<bool> {
    // Set grpc-message header to "Too many requests in the queue"
    // This is meant to be used by a Tonic interceptor to return a gRPC error
    let mut header = ResponseHeader::build(503, None)?;
    header.insert_header("grpc-message", "Too many requests in the queue".to_string())?;
    header.insert_header("grpc-status", RESOURCE_EXHAUSTED_CODE)?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header.clone()), true).await?;

    let mut error = Error::new(ErrorType::HTTPStatus(503))
        .more_context("Too many requests in the queue")
        .into_in();
    error.set_cause("Too many requests in the queue");

    session.write_response_header(Box::new(header), false).await?;

    // Increment the queue drop count metric
    QUEUE_DROP_COUNT.inc();

    Err(error)
}

/// Create a 429 response for too many requests
pub async fn create_too_many_requests_response(
    session: &mut Session,
    max_request_per_second: isize,
) -> pingora_core::Result<bool> {
    // Rate limited, return 429
    let mut header = ResponseHeader::build(429, None)?;
    header.insert_header("X-Rate-Limit-Limit", max_request_per_second.to_string())?;
    header.insert_header("X-Rate-Limit-Remaining", "0")?;
    header.insert_header("X-Rate-Limit-Reset", "1")?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header), true).await?;
    Ok(true)
}

/// Create a 200 response for updated workers
///
/// It will set the X-Worker-Count header to the number of workers.
pub async fn create_workers_updated_response(
    session: &mut Session,
    workers: usize,
) -> pingora_core::Result<bool> {
    let mut header = ResponseHeader::build(200, None)?;
    header.insert_header("X-Worker-Count", workers.to_string())?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header), true).await?;
    Ok(true)
}

/// Create a 400 response with an error message
///
/// It will set the X-Error-Message header to the error message.
pub async fn create_response_with_error_message(
    session: &mut Session,
    error_msg: String,
) -> pingora_core::Result<bool> {
    let mut header = ResponseHeader::build(400, None)?;
    header.insert_header("X-Error-Message", error_msg)?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header), true).await?;
    Ok(true)
}

/// Create a gRPC [HealthClient] for the given worker address.
///
/// # Errors
/// - [TxProverServiceError::InvalidURI] if the worker address is invalid.
/// - [TxProverServiceError::ConnectionFailed] if the connection to the worker fails.
pub async fn create_health_check_client(
    address: String,
    connection_timeout: Duration,
    total_timeout: Duration,
) -> Result<HealthClient<Channel>, TxProverServiceError> {
    let channel = Channel::from_shared(format!("http://{}", address))
        .map_err(|err| TxProverServiceError::InvalidURI(err, address.clone()))?
        .connect_timeout(connection_timeout)
        .timeout(total_timeout)
        .connect()
        .await
        .map_err(|err| TxProverServiceError::ConnectionFailed(err, address))?;

    Ok(HealthClient::new(channel))
}
