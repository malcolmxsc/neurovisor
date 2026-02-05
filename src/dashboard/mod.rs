//! Web dashboard for NeuroVisor monitoring
//!
//! Provides a simple web interface for monitoring:
//! - VM pool status
//! - Agent task history
//! - System metrics

use std::sync::Arc;

use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::metrics;
use crate::vm::VMPool;

/// Dashboard application state
#[derive(Clone)]
pub struct DashboardState {
    pub pool: Arc<VMPool>,
}

/// VM pool status response
#[derive(Serialize)]
pub struct PoolStatus {
    pub warm_count: usize,
    pub active_count: usize,
    pub target_warm_size: usize,
    pub max_pool_size: usize,
}

/// System metrics response
#[derive(Serialize)]
pub struct SystemMetrics {
    pub requests_total: f64,
    pub requests_in_flight: f64,
    pub errors_total: f64,
    pub avg_inference_duration_ms: f64,
}

/// Create the dashboard router
pub fn create_router(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(dashboard_page))
        .route("/api/status", get(pool_status))
        .route("/api/metrics", get(system_metrics))
        .with_state(state)
}

/// Serve the HTML dashboard page
async fn dashboard_page() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

/// Get VM pool status
async fn pool_status(State(state): State<DashboardState>) -> impl IntoResponse {
    let stats = state.pool.stats().await;
    Json(PoolStatus {
        warm_count: stats.warm_count,
        active_count: stats.active_count,
        target_warm_size: stats.target_warm_size,
        max_pool_size: stats.max_pool_size,
    })
}

/// Get system metrics
async fn system_metrics() -> impl IntoResponse {
    // Read from Prometheus metrics
    let requests_total = metrics::REQUESTS_TOTAL
        .get_metric_with_label_values(&["llama3.2"])
        .map(|m| m.get())
        .unwrap_or(0.0)
        + metrics::REQUESTS_TOTAL
            .get_metric_with_label_values(&["qwen3"])
            .map(|m| m.get())
            .unwrap_or(0.0);

    let requests_in_flight = metrics::REQUESTS_IN_FLIGHT.get();
    let errors_total = metrics::ERRORS_TOTAL
        .get_metric_with_label_values(&["ollama_error"])
        .map(|m| m.get())
        .unwrap_or(0.0);

    // Calculate average inference duration
    let inference_count = metrics::INFERENCE_DURATION.get_sample_count();
    let inference_sum = metrics::INFERENCE_DURATION.get_sample_sum();
    let avg_inference_duration_ms = if inference_count > 0 {
        (inference_sum / inference_count as f64) * 1000.0
    } else {
        0.0
    };

    Json(SystemMetrics {
        requests_total,
        requests_in_flight,
        errors_total,
        avg_inference_duration_ms,
    })
}

/// HTML dashboard page
const DASHBOARD_HTML: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>NeuroVisor Dashboard</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #0a0a0a;
            color: #e0e0e0;
            min-height: 100vh;
            padding: 2rem;
        }
        .header {
            text-align: center;
            margin-bottom: 2rem;
            border-bottom: 1px solid #333;
            padding-bottom: 1rem;
        }
        .header h1 {
            color: #00ff88;
            font-size: 2rem;
            margin-bottom: 0.5rem;
        }
        .header p { color: #888; }
        .grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
            gap: 1.5rem;
            max-width: 1200px;
            margin: 0 auto;
        }
        .card {
            background: #1a1a1a;
            border-radius: 12px;
            padding: 1.5rem;
            border: 1px solid #333;
        }
        .card h2 {
            color: #00ff88;
            font-size: 1rem;
            margin-bottom: 1rem;
            text-transform: uppercase;
            letter-spacing: 1px;
        }
        .stat {
            display: flex;
            justify-content: space-between;
            padding: 0.5rem 0;
            border-bottom: 1px solid #222;
        }
        .stat:last-child { border-bottom: none; }
        .stat-label { color: #888; }
        .stat-value { color: #fff; font-weight: bold; }
        .stat-value.good { color: #00ff88; }
        .stat-value.warn { color: #ffaa00; }
        .stat-value.error { color: #ff4444; }
        .refresh-note {
            text-align: center;
            color: #666;
            margin-top: 2rem;
            font-size: 0.875rem;
        }
    </style>
</head>
<body>
    <div class="header">
        <h1>NeuroVisor</h1>
        <p>AI Agent Sandbox Dashboard</p>
    </div>

    <div class="grid">
        <div class="card">
            <h2>VM Pool</h2>
            <div id="pool-stats">Loading...</div>
        </div>

        <div class="card">
            <h2>System Metrics</h2>
            <div id="system-stats">Loading...</div>
        </div>
    </div>

    <p class="refresh-note">Auto-refreshes every 2 seconds</p>

    <script>
        async function fetchData() {
            try {
                // Fetch pool status
                const poolRes = await fetch('/api/status');
                const pool = await poolRes.json();
                document.getElementById('pool-stats').innerHTML = `
                    <div class="stat">
                        <span class="stat-label">Warm VMs</span>
                        <span class="stat-value ${pool.warm_count > 0 ? 'good' : 'warn'}">${pool.warm_count}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">Active VMs</span>
                        <span class="stat-value">${pool.active_count}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">Target Warm</span>
                        <span class="stat-value">${pool.target_warm_size}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">Max Pool Size</span>
                        <span class="stat-value">${pool.max_pool_size}</span>
                    </div>
                `;

                // Fetch system metrics
                const metricsRes = await fetch('/api/metrics');
                const metrics = await metricsRes.json();
                document.getElementById('system-stats').innerHTML = `
                    <div class="stat">
                        <span class="stat-label">Total Requests</span>
                        <span class="stat-value">${Math.round(metrics.requests_total)}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">In Flight</span>
                        <span class="stat-value ${metrics.requests_in_flight > 0 ? 'good' : ''}">${metrics.requests_in_flight}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">Errors</span>
                        <span class="stat-value ${metrics.errors_total > 0 ? 'error' : 'good'}">${Math.round(metrics.errors_total)}</span>
                    </div>
                    <div class="stat">
                        <span class="stat-label">Avg Inference</span>
                        <span class="stat-value">${metrics.avg_inference_duration_ms.toFixed(0)}ms</span>
                    </div>
                `;
            } catch (err) {
                console.error('Error fetching data:', err);
            }
        }

        // Initial fetch and auto-refresh
        fetchData();
        setInterval(fetchData, 2000);
    </script>
</body>
</html>
"#;
