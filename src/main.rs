mod api;
mod bulk;
mod data;
mod graph;
mod models;
mod search;
mod store;

use crate::api::Travelpayouts;
use crate::bulk::{BulkConfig, DiscoveryConfig};
use crate::data::StaticData;
use crate::graph::GraphCache;
use crate::models::{
    CacheStats, CityPoint, CountryPoint, DbStats, ErrorResponse, SearchRequest, SearchResponse,
};
use crate::store::Store;
use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::header::{HeaderValue, CACHE_CONTROL};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

struct AppState {
    data: Arc<StaticData>,
    api: Arc<Travelpayouts>,
    store: Store,
    graph: Arc<GraphCache>,
    search_lock: Arc<Semaphore>,
}

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct CitiesParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct CountriesParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct RegionParams {
    lat: f64,
    lon: f64,
    radius_km: f64,
    #[serde(default)]
    limit: Option<usize>,
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn cities(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CitiesParams>,
) -> Json<Vec<CityPoint>> {
    Json(
        state
            .data
            .search_cities(&params.q, params.limit.unwrap_or(20)),
    )
}

async fn countries(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CountriesParams>,
) -> Json<Vec<CountryPoint>> {
    Json(
        state
            .data
            .search_countries(&params.q, params.limit.unwrap_or(20)),
    )
}

async fn region(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RegionParams>,
) -> Result<Json<Vec<CityPoint>>, AppError> {
    if !params.lat.is_finite()
        || !params.lon.is_finite()
        || !params.radius_km.is_finite()
        || params.lat.abs() > 90.0
        || params.lon.abs() > 180.0
        || params.radius_km <= 0.0
    {
        return Err(AppError::bad_request("Некорректные координаты или радиус."));
    }
    Ok(Json(state.data.airports_in_radius(
        params.lat,
        params.lon,
        params.radius_km,
        params.limit.unwrap_or(200).clamp(1, 400),
    )))
}

async fn cache_stats(State(state): State<Arc<AppState>>) -> Json<CacheStats> {
    Json(state.api.stats().await)
}

async fn db_stats(State(state): State<Arc<AppState>>) -> Result<Json<DbStats>, AppError> {
    let mut stats = state
        .store
        .stats()
        .await
        .map_err(|error| AppError::internal(format!("БД: {error}")))?;
    let graph = state.graph.snapshot().await;
    stats.ram_offers = graph.offers.len() as i64;
    stats.ram_routes = graph.route_set.len() as i64;
    stats.ram_built_at = Some(graph.built_at.to_rfc3339());
    Ok(Json(stats))
}

async fn search(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, AppError> {
    let _permit = state
        .search_lock
        .acquire()
        .await
        .map_err(|_| AppError::internal("Очередь поиска недоступна."))?;
    let result = tokio::time::timeout(
        Duration::from_secs(150),
        search::run_search(
            state.data.clone(),
            state.api.clone(),
            state.store.clone(),
            state.graph.clone(),
            request,
        ),
    )
    .await;
    match result {
        Ok(Ok(response)) => Ok(Json(response)),
        Ok(Err(error)) => Err(AppError::bad_request(error)),
        Err(_) => Err(AppError::timeout(
            "Поиск занял слишком много времени. Уменьшите лимит маршрутов или количество городов.",
        )),
    }
}

fn app(state: Arc<AppState>) -> Router {
    let static_service = ServeDir::new("static").append_index_html_on_directories(true);
    Router::new()
        .route("/api/health", get(health))
        .route("/api/cities", get(cities))
        .route("/api/countries", get(countries))
        .route("/api/region", get(region))
        .route("/api/cache", get(cache_stats))
        .route("/api/db", get(db_stats))
        .route("/api/search", post(search))
        .fallback_service(static_service)
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let token = std::env::var("TRAVELPAYOUTS_TOKEN")
        .context("TRAVELPAYOUTS_TOKEN is not set (see .env)")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080);
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .context("build http client")?;
    let data = StaticData::load(&client, Path::new("data")).await?;
    tracing::info!(
        cities = data.cities.len(),
        airports = data.airports.len(),
        routes = data.topology.adj.len(),
        hubs = data.hubs.len(),
        "static data loaded"
    );
    if let Some(warning) = &data.topology_warning {
        tracing::warn!("{warning}");
    }

    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "data/pathfinder.db".to_string());
    let store = Store::open(Path::new(&db_path)).await?;
    store.sync_cities(&data).await?;
    tracing::info!(path = %db_path, "database ready");

    let db_ttl_hours: u64 = std::env::var("DB_TTL_HOURS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(18);
    let db_horizon_days: u32 = std::env::var("DB_HORIZON_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400);

    let api = Travelpayouts::new(
        token,
        data.clone(),
        Some(store.clone()),
        Duration::from_secs(db_ttl_hours * 3600),
        db_horizon_days,
    );
    let graph_currency = std::env::var("BULK_CURRENCY")
        .unwrap_or_else(|_| "rub".to_string())
        .to_lowercase();
    let graph = GraphCache::new(store.clone(), graph_currency, db_horizon_days).await;
    let bulk_config = BulkConfig::from_env();
    if bulk_config.enabled {
        tokio::spawn(bulk::run_scheduler(
            api.clone(),
            store.clone(),
            graph.clone(),
            data.clone(),
            bulk_config,
        ));
    }
    let discovery_config = DiscoveryConfig::from_env();
    if discovery_config.enabled {
        tokio::spawn(bulk::run_discovery_scheduler(
            api.clone(),
            store.clone(),
            data.clone(),
            discovery_config,
        ));
    }
    let restart_hours: u64 = std::env::var("STATIC_RESTART_HOURS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    if restart_hours > 0 {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(restart_hours.saturating_mul(3600)))
                    .await;
                tracing::info!("restarting to refresh static data");
                std::process::exit(0);
            }
        });
    }
    let state = Arc::new(AppState {
        data,
        api,
        store,
        graph,
        search_lock: Arc::new(Semaphore::new(1)),
    });
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!("listening on http://{host}:{port}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}
