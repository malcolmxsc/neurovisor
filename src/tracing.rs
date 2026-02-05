//! OpenTelemetry tracing for distributed observability
//!
//! This module provides integration with the Grafana observability stack:
//! - Traces are sent to Tempo via OTLP
//! - Logs are enriched with trace_id for correlation in Loki
//! - Metrics exemplars link to traces
//!
//! # Architecture
//!
//! ```text
//! neurovisor → OTLP (gRPC) → OTel Collector → Tempo (traces)
//!                                          → Loki (logs)
//!                                          → Prometheus (metrics)
//! ```

use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, trace as sdktrace, Resource};
use opentelemetry::KeyValue;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Default OTLP endpoint (OTel collector)
const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4316";

/// Initialize the tracing subsystem with OpenTelemetry export
///
/// This sets up:
/// - Console logging with trace context
/// - OpenTelemetry trace export to OTLP endpoint
///
/// # Arguments
/// * `service_name` - Name for the service in traces
/// * `otlp_endpoint` - Optional OTLP endpoint URL (defaults to localhost:4316)
///
/// # Example
/// ```ignore
/// init_tracing("neurovisor", None)?;
/// ```
pub fn init_tracing(
    service_name: &str,
    otlp_endpoint: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = otlp_endpoint.unwrap_or(DEFAULT_OTLP_ENDPOINT);

    // Create OTLP exporter
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint);

    // Create tracer with batching - install_batch returns the Tracer directly
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(
            sdktrace::Config::default()
                .with_resource(Resource::new(vec![
                    KeyValue::new("service.name", service_name.to_string()),
                    KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                ])),
        )
        .install_batch(runtime::Tokio)?;

    // Create OpenTelemetry layer for tracing-subscriber
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Create console/JSON logging layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    // Environment filter for log levels
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,neurovisor=debug"));

    // Combine layers
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    tracing::info!(
        service = service_name,
        endpoint = endpoint,
        "OpenTelemetry tracing initialized"
    );

    Ok(())
}

/// Shutdown the tracing subsystem gracefully
///
/// Flushes any pending spans to the collector
pub fn shutdown_tracing() {
    opentelemetry::global::shutdown_tracer_provider();
    tracing::info!("OpenTelemetry tracing shutdown complete");
}

/// Create a span with the given trace_id
///
/// This is useful when you have an existing trace_id (e.g., from a gRPC header)
/// and want to create spans that belong to that trace.
#[macro_export]
macro_rules! span_with_trace {
    ($level:expr, $name:expr, $trace_id:expr) => {
        tracing::span!($level, $name, trace_id = %$trace_id)
    };
}

/// Log with trace context
///
/// Automatically includes trace_id in structured log output
#[macro_export]
macro_rules! trace_log {
    ($level:ident, $trace_id:expr, $($arg:tt)*) => {
        tracing::$level!(trace_id = %$trace_id, $($arg)*)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_endpoint() {
        assert_eq!(DEFAULT_OTLP_ENDPOINT, "http://localhost:4316");
    }
}
