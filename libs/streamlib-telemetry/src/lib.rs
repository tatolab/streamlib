// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified OpenTelemetry observability for StreamLib.

#[cfg(feature = "broker")]
pub mod broker_telemetry_log_exporter;
#[cfg(feature = "broker")]
pub mod broker_telemetry_span_exporter;
#[cfg(feature = "broker")]
pub mod proto;

pub mod sqlite_telemetry_database;
pub mod sqlite_telemetry_log_exporter;
pub mod sqlite_telemetry_span_exporter;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::prelude::*;

use opentelemetry::logs::AnyValue;
#[cfg(feature = "otlp")]
use opentelemetry_otlp::WithExportConfig;

use sqlite_telemetry_database::{default_telemetry_database_path, SqliteTelemetryDatabase};
use sqlite_telemetry_log_exporter::SqliteTelemetryLogExporter;
use sqlite_telemetry_span_exporter::SqliteTelemetrySpanExporter;

/// Extract a plain string from an OTel AnyValue (avoids Debug wrapping like "String(Owned(...))").
pub fn format_any_value(v: &AnyValue) -> String {
    match v {
        AnyValue::String(s) => s.to_string(),
        AnyValue::Int(i) => i.to_string(),
        AnyValue::Double(d) => d.to_string(),
        AnyValue::Boolean(b) => b.to_string(),
        other => format!("{:?}", other),
    }
}

/// Configuration for telemetry initialization.
pub struct TelemetryConfig {
    pub service_name: String,
    pub resource_attributes: Vec<(String, String)>,
    /// Optional file log path for backward compatibility with existing file logs.
    pub file_log_path: Option<PathBuf>,
    /// Whether to emit logs to stdout.
    pub stdout_logging: bool,
    /// Optional OTLP endpoint (e.g. "http://localhost:4317") for Jaeger export.
    pub otlp_endpoint: Option<String>,
    /// SQLite database path. Defaults to ~/.streamlib/telemetry.db.
    /// Only used when broker_endpoint is None (i.e., this process is the collector).
    pub sqlite_database_path: Option<PathBuf>,
    /// Broker gRPC endpoint for telemetry ingestion (e.g. "http://127.0.0.1:50052").
    /// When set (and `broker` feature enabled), telemetry is routed to the broker
    /// instead of writing to SQLite directly.
    pub broker_endpoint: Option<String>,
}

/// Holds OTel providers and guards that must live for the duration of telemetry collection.
pub struct TelemetryGuard {
    _tracer_provider: SdkTracerProvider,
    _logger_provider: SdkLoggerProvider,
    _file_log_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // Shutdown is handled by the providers' Drop impls
    }
}

/// Initialize the telemetry pipeline.
///
/// When `broker_endpoint` is set (and `broker` feature enabled), spans and logs
/// are sent to the broker via gRPC. The broker is the single SQLite writer.
/// When `broker_endpoint` is None, spans and logs are written to SQLite directly
/// (used by the broker itself).
///
/// Safe to call multiple times — only the first call initializes the tracing
/// subscriber. Subsequent calls return a no-op guard (the original guard
/// keeps the pipeline alive).
pub fn init_telemetry(config: TelemetryConfig) -> Result<TelemetryGuard> {
    static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    if INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        // Already initialized — return a dummy guard.
        // The original guard (held by whoever called first) keeps providers alive.
        let dummy_tracer = SdkTracerProvider::builder().build();
        let dummy_logger = SdkLoggerProvider::builder().build();
        return Ok(TelemetryGuard {
            _tracer_provider: dummy_tracer,
            _logger_provider: dummy_logger,
            _file_log_guard: None,
        });
    }

    let use_broker = {
        #[cfg(feature = "broker")]
        {
            config.broker_endpoint.is_some()
        }
        #[cfg(not(feature = "broker"))]
        {
            false
        }
    };

    let (tracer_provider, logger_provider) = if use_broker {
        #[cfg(feature = "broker")]
        {
            build_broker_providers(&config)?
        }
        #[cfg(not(feature = "broker"))]
        {
            anyhow::bail!("broker_endpoint requires the 'broker' feature on streamlib-telemetry");
        }
    } else {
        build_sqlite_providers(&config)?
    };

    let tracer = tracer_provider.tracer(config.service_name.clone());

    // -- Tracing subscriber layers --
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".parse().unwrap());

    let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let otel_log_layer =
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&logger_provider);

    let stdout_layer = config.stdout_logging.then(tracing_subscriber::fmt::layer);

    let (file_layer, file_guard) = if let Some(ref log_path) = config.file_log_path {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file_name = log_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "streamlib.log".to_string());
        let dir = log_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let file_appender = tracing_appender::rolling::never(dir, file_name);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false);
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    Ok(TelemetryGuard {
        _tracer_provider: tracer_provider,
        _logger_provider: logger_provider,
        _file_log_guard: file_guard,
    })
}

/// Build providers that write to SQLite directly (used by the broker).
fn build_sqlite_providers(
    config: &TelemetryConfig,
) -> Result<(SdkTracerProvider, SdkLoggerProvider)> {
    let db_path = config
        .sqlite_database_path
        .clone()
        .or_else(|| default_telemetry_database_path().ok())
        .context("Failed to determine telemetry database path")?;

    let database =
        Arc::new(SqliteTelemetryDatabase::open(&db_path).with_context(|| {
            format!("Failed to open telemetry database: {}", db_path.display())
        })?);

    let sqlite_span_exporter =
        SqliteTelemetrySpanExporter::new(database.clone(), config.service_name.clone());

    #[allow(unused_mut)]
    let mut tracer_provider_builder =
        SdkTracerProvider::builder().with_simple_exporter(sqlite_span_exporter);

    #[cfg(feature = "otlp")]
    if let Some(ref endpoint) = config.otlp_endpoint {
        let otlp_span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("Failed to create OTLP span exporter")?;
        tracer_provider_builder = tracer_provider_builder.with_batch_exporter(otlp_span_exporter);
    }

    let tracer_provider = tracer_provider_builder.build();

    let sqlite_log_exporter =
        SqliteTelemetryLogExporter::new(database, config.service_name.clone());

    #[allow(unused_mut)]
    let mut logger_provider_builder =
        SdkLoggerProvider::builder().with_simple_exporter(sqlite_log_exporter);

    #[cfg(feature = "otlp")]
    if let Some(ref endpoint) = config.otlp_endpoint {
        let otlp_log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("Failed to create OTLP log exporter")?;
        logger_provider_builder = logger_provider_builder.with_batch_exporter(otlp_log_exporter);
    }

    let logger_provider = logger_provider_builder.build();

    Ok((tracer_provider, logger_provider))
}

/// Build providers that send telemetry to the broker via gRPC.
#[cfg(feature = "broker")]
fn build_broker_providers(
    config: &TelemetryConfig,
) -> Result<(SdkTracerProvider, SdkLoggerProvider)> {
    let endpoint = config.broker_endpoint.as_ref().unwrap().clone();

    let broker_span_exporter = broker_telemetry_span_exporter::BrokerTelemetrySpanExporter::new(
        endpoint.clone(),
        config.service_name.clone(),
    );

    #[allow(unused_mut)]
    let mut tracer_provider_builder =
        SdkTracerProvider::builder().with_batch_exporter(broker_span_exporter);

    #[cfg(feature = "otlp")]
    if let Some(ref otlp_endpoint) = config.otlp_endpoint {
        let otlp_span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint)
            .build()
            .context("Failed to create OTLP span exporter")?;
        tracer_provider_builder = tracer_provider_builder.with_batch_exporter(otlp_span_exporter);
    }

    let tracer_provider = tracer_provider_builder.build();

    let broker_log_exporter = broker_telemetry_log_exporter::BrokerTelemetryLogExporter::new(
        endpoint,
        config.service_name.clone(),
    );

    #[allow(unused_mut)]
    let mut logger_provider_builder =
        SdkLoggerProvider::builder().with_batch_exporter(broker_log_exporter);

    #[cfg(feature = "otlp")]
    if let Some(ref otlp_endpoint) = config.otlp_endpoint {
        let otlp_log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint)
            .build()
            .context("Failed to create OTLP log exporter")?;
        logger_provider_builder = logger_provider_builder.with_batch_exporter(otlp_log_exporter);
    }

    let logger_provider = logger_provider_builder.build();

    Ok((tracer_provider, logger_provider))
}

/// Delete telemetry records older than `retain_days` days. Returns count of deleted rows.
pub fn prune_old_telemetry(retain_days: u32) -> Result<u64> {
    let db_path = default_telemetry_database_path()?;
    if !db_path.exists() {
        return Ok(0);
    }
    let database = SqliteTelemetryDatabase::open(&db_path)?;
    database.prune_older_than_days(retain_days)
}
