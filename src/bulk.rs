use crate::api::Travelpayouts;
use crate::data::StaticData;
use crate::graph::GraphCache;
use crate::models::Offer;
use crate::store::Store;
use chrono::{DateTime, LocalResult, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use futures::StreamExt;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub enabled: bool,
    pub on_start: bool,
    pub interval_hours: u64,
    pub concurrency: usize,
}

impl DiscoveryConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("DISCOVERY_ENABLED", true),
            on_start: env_bool("DISCOVERY_ON_START", true),
            interval_hours: env_usize("DISCOVERY_INTERVAL_HOURS", 12).clamp(1, 168) as u64,
            concurrency: env_usize("DISCOVERY_CONCURRENCY", 8).clamp(1, 16),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BulkConfig {
    pub enabled: bool,
    pub currency: String,
    pub hours: Vec<u32>,
    pub timezone: Tz,
    pub batch_origins: usize,
    pub routes_per_origin: usize,
    pub discovery_limit: usize,
    pub concurrency: usize,
    pub countries: Vec<String>,
    pub origins: Vec<String>,
    pub run_on_start: bool,
    pub force_start: bool,
    pub horizon_days: u32,
    pub stale_days: u32,
}

impl BulkConfig {
    pub fn from_env() -> Self {
        let hours = parse_hours(std::env::var("BULK_HOURS").ok().as_deref());
        let timezone = std::env::var("BULK_TZ")
            .ok()
            .and_then(|value| value.parse::<Tz>().ok())
            .unwrap_or(chrono_tz::Europe::Moscow);
        Self {
            enabled: env_bool("BULK_ENABLED", true),
            currency: std::env::var("BULK_CURRENCY")
                .unwrap_or_else(|_| "rub".to_string())
                .to_lowercase(),
            hours,
            timezone,
            batch_origins: env_usize("BULK_BATCH_ORIGINS", 40).clamp(1, 2000),
            routes_per_origin: env_usize("BULK_ROUTES_PER_ORIGIN", 50).clamp(1, 500),
            discovery_limit: env_usize("BULK_DISCOVERY_LIMIT", 120).clamp(1, 200),
            concurrency: env_usize("BULK_CONCURRENCY", 8).clamp(1, 16),
            countries: env_list("BULK_COUNTRIES"),
            origins: env_list("BULK_ORIGINS"),
            run_on_start: env_bool("BULK_RUN_ON_START", true),
            force_start: env_bool("BULK_FORCE_START", false),
            horizon_days: env_usize("BULK_HORIZON_DAYS", 400).clamp(1, 400) as u32,
            stale_days: env_usize("BULK_STALE_DAYS", 120).clamp(1, 365) as u32,
        }
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim().to_uppercase())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_hours(value: Option<&str>) -> Vec<u32> {
    let mut hours: Vec<u32> = value
        .unwrap_or("2,14")
        .split(',')
        .filter_map(|item| item.trim().parse::<u32>().ok())
        .filter(|hour| *hour < 24)
        .collect();
    hours.sort_unstable();
    hours.dedup();
    if hours.is_empty() {
        vec![2, 14]
    } else {
        hours
    }
}

fn next_run(now: DateTime<Utc>, timezone: Tz, hours: &[u32]) -> DateTime<Utc> {
    let now_local = now.with_timezone(&timezone);
    let mut best: Option<DateTime<Utc>> = None;
    for day_offset in 0..3 {
        let date = now_local.date_naive() + chrono::Duration::days(day_offset);
        for &hour in hours {
            let Some(naive) = date.and_hms_opt(hour, 0, 0) else {
                continue;
            };
            let local = match timezone.from_local_datetime(&naive) {
                LocalResult::Single(value) => Some(value),
                LocalResult::Ambiguous(first, _) => Some(first),
                LocalResult::None => None,
            };
            if let Some(local) = local {
                let utc = local.with_timezone(&Utc);
                if utc > now + chrono::Duration::seconds(5)
                    && best.map(|current| utc < current).unwrap_or(true)
                {
                    best = Some(utc);
                }
            }
        }
    }
    best.unwrap_or_else(|| now + chrono::Duration::hours(12))
}

pub async fn run_scheduler(
    api: Arc<Travelpayouts>,
    store: Store,
    graph: Arc<GraphCache>,
    data: Arc<StaticData>,
    config: BulkConfig,
) {
    if !config.enabled {
        tracing::info!("bulk refresh disabled");
        return;
    }
    tracing::info!(
        hours = ?config.hours,
        timezone = %config.timezone,
        batch = config.batch_origins,
        routes_per_origin = config.routes_per_origin,
        countries = ?config.countries,
        "bulk scheduler started"
    );

    if config.run_on_start {
        let should_run = config.force_start
            || match store.stats().await {
            Ok(stats) => stats
                .last_bulk_finished
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|finished| {
                    Utc::now() - finished.with_timezone(&Utc) > chrono::Duration::hours(11)
                })
                .unwrap_or(true),
            Err(_) => true,
        };
        if should_run {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if let Err(error) = run_batch(&api, &store, &data, &config).await {
                tracing::error!("bulk refresh: {error}");
            }
            rebuild_graph(&graph).await;
        } else {
            tracing::info!("bulk start skipped: recent refresh");
        }
    }

    loop {
        let next = next_run(Utc::now(), config.timezone, &config.hours);
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(3600));
        tracing::info!(next_utc = %next, "next bulk refresh scheduled");
        tokio::time::sleep(wait).await;
        if let Err(error) = run_batch(&api, &store, &data, &config).await {
            tracing::error!("bulk refresh: {error}");
        }
        rebuild_graph(&graph).await;
    }
}

async fn rebuild_graph(graph: &GraphCache) {
    match graph.rebuild().await {
        Ok(count) => tracing::info!(offers = count, "ram graph rebuilt"),
        Err(error) => tracing::error!("ram graph rebuild: {error}"),
    }
}

async fn run_batch(
    api: &Travelpayouts,
    store: &Store,
    data: &StaticData,
    config: &BulkConfig,
) -> Result<(), String> {
    let log_id = store
        .start_bulk_log()
        .await
        .map_err(|error| format!("bulk log start: {error}"))?;
    let origins = store
        .next_origins(config.batch_origins, &config.origins, &config.countries)
        .await
        .map_err(|error| format!("bulk origins: {error}"))?;
    if origins.is_empty() {
        let _ = store
            .finish_bulk_log(log_id, 0, 0, 0, 0, "empty", "")
            .await;
        return Ok(());
    }

    tracing::info!(origins = ?origins, "bulk refresh batch started");
    let mut total_routes = 0usize;
    let mut total_offers = 0usize;
    let mut errors = 0usize;
    let mut details: Vec<String> = Vec::new();

    for origin in &origins {
        match refresh_origin(origin, api, store, data, config).await {
            Ok((routes, offers, route_errors)) => {
                total_routes += routes;
                total_offers += offers;
                errors += route_errors;
                tracing::info!(
                    origin,
                    routes,
                    offers,
                    route_errors,
                    "bulk origin refreshed"
                );
            }
            Err(error) => {
                errors += 1;
                if details.len() < 20 {
                    details.push(format!("{origin}: {error}"));
                }
                tracing::warn!(origin, error, "bulk origin failed");
            }
        }
    }

    if let Err(error) = store
        .purge(Utc::now(), config.horizon_days, config.stale_days)
        .await
    {
        tracing::warn!("bulk purge: {error}");
    }

    let status = if errors == 0 { "ok" } else { "partial" };
    let _ = store
        .finish_bulk_log(
            log_id,
            origins.len(),
            total_routes,
            total_offers,
            errors,
            status,
            &details.join("; "),
        )
        .await;
    tracing::info!(
        origins = origins.len(),
        routes = total_routes,
        offers = total_offers,
        errors,
        "bulk refresh batch finished"
    );
    Ok(())
}

async fn refresh_origin(
    origin: &str,
    api: &Travelpayouts,
    store: &Store,
    data: &StaticData,
    config: &BulkConfig,
) -> Result<(usize, usize, usize), String> {
    let (directions, direct_destinations) = tokio::join!(
        api.get_city_directions(origin, &config.currency),
        api.get_direct_destinations(origin, &config.currency)
    );
    let directions = directions.unwrap_or_default();
    let direct_destinations = direct_destinations.unwrap_or_default();
    let existing = store
        .routes_from(origin, config.routes_per_origin)
        .await
        .unwrap_or_default();
    let topology: Vec<String> = data
        .topology
        .adj
        .get(origin)
        .cloned()
        .unwrap_or_default();

    let mut destinations = Vec::new();
    let mut discovered = Vec::new();
    let mut seen = HashSet::new();
    let mut discovered_seen = HashSet::new();
    for destination in direct_destinations
        .iter()
        .chain(topology.iter())
        .chain(directions.iter().take(config.discovery_limit))
    {
        if destination == origin || !data.cities.contains_key(destination) {
            continue;
        }
        if discovered_seen.insert(destination.clone()) {
            discovered.push(destination.clone());
        }
        if seen.insert(destination.clone()) {
            destinations.push(destination.clone());
        }
    }
    for destination in existing {
        if seen.insert(destination.clone()) {
            destinations.push(destination);
        }
    }
    destinations.truncate(config.routes_per_origin);
    if destinations.is_empty() {
        let _ = store.mark_origin(origin).await;
        return Err("нет направлений".to_string());
    }

    let now = Utc::now();
    let earliest = now - chrono::Duration::hours(6);
    let horizon = now + chrono::Duration::days(config.horizon_days as i64);
    let results: Vec<(String, Result<Arc<Vec<Offer>>, String>)> =
        futures::stream::iter(destinations.into_iter().map(|destination| {
            let currency = config.currency.clone();
            async move {
                let result = api
                    .fetch_route_offers(origin, &destination, &currency, false)
                    .await;
                (destination, result)
            }
        }))
        .buffer_unordered(config.concurrency)
        .collect()
        .await;

    let mut successes: Vec<(String, Vec<Offer>)> = Vec::new();
    let mut errors = 0usize;
    for (destination, result) in results {
        match result {
            Ok(offers) => {
                let filtered: Vec<Offer> = offers
                    .iter()
                    .filter(|offer| offer.depart_at >= earliest && offer.depart_at <= horizon)
                    .cloned()
                    .collect();
                successes.push((destination, filtered));
            }
            Err(error) => {
                errors += 1;
                tracing::debug!(origin, destination, error, "bulk route failed");
            }
        }
    }

    if successes.is_empty() {
        let _ = store.mark_origin(origin).await;
        return Err("не удалось получить цены".to_string());
    }

    let total_routes = successes.len();
    let total_offers: usize = successes.iter().map(|(_, offers)| offers.len()).sum();
    let stale_cutoff = (now - chrono::Duration::days(config.stale_days as i64))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    store
        .replace_origin_refresh(
            origin,
            &config.currency,
            &successes,
            &discovered,
            &stale_cutoff,
        )
        .await
        .map_err(|error| format!("db refresh: {error}"))?;
    Ok((total_routes, total_offers, errors))
}


pub async fn run_discovery_scheduler(
    api: Arc<Travelpayouts>,
    store: Store,
    data: Arc<StaticData>,
    config: DiscoveryConfig,
) {
    if !config.enabled {
        tracing::info!("direct discovery disabled");
        return;
    }
    loop {
        let now = Utc::now();
        let last = store
            .get_meta("last_discovery_at")
            .await
            .ok()
            .flatten()
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc));
        let next = match last {
            Some(last) => last + chrono::Duration::hours(config.interval_hours as i64),
            None => {
                if config.on_start {
                    now
                } else {
                    now + chrono::Duration::hours(config.interval_hours as i64)
                }
            }
        };
        if next > now {
            let wait = (next - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(3600));
            tracing::info!(next_utc = %next, "next direct discovery scheduled");
            tokio::time::sleep(wait).await;
        }
        if let Err(error) = run_discovery_pass(&api, &store, &data, &config).await {
            tracing::error!("direct discovery: {error}");
        }
        let _ = store
            .set_meta("last_discovery_at", &Utc::now().to_rfc3339())
            .await;
    }
}

async fn run_discovery_pass(
    api: &Travelpayouts,
    store: &Store,
    data: &StaticData,
    config: &DiscoveryConfig,
) -> Result<(), String> {
    let mut origins: Vec<String> = data.cities.keys().cloned().collect();
    origins.sort_by(|a, b| data.degree(b).cmp(&data.degree(a)));
    let total = origins.len();
    let currency = std::env::var("BULK_CURRENCY")
        .unwrap_or_else(|_| "rub".to_string())
        .to_lowercase();
    tracing::info!(origins = total, "direct discovery started");

    let mut stream = futures::stream::iter(origins.into_iter().map(|origin| {
        let currency = currency.clone();
        async move {
            let result = api.get_direct_destinations(&origin, &currency).await;
            (origin, result)
        }
    }))
    .buffer_unordered(config.concurrency);

    let mut done = 0usize;
    let mut routes = 0usize;
    let mut errors = 0usize;
    while let Some((origin, result)) = stream.next().await {
        done += 1;
        match result {
            Ok(destinations) => {
                let filtered: Vec<String> = destinations
                    .into_iter()
                    .filter(|destination| {
                        destination != &origin && data.cities.contains_key(destination)
                    })
                    .collect();
                match store
                    .upsert_discovered_routes(&origin, &filtered, "direct")
                    .await
                {
                    Ok(count) => routes += count,
                    Err(error) => {
                        errors += 1;
                        tracing::warn!(origin, error = %error, "direct discovery db");
                    }
                }
            }
            Err(error) => {
                errors += 1;
                tracing::debug!(origin, error, "direct discovery api");
            }
        }
        if done % 200 == 0 {
            tracing::info!(done, total, routes, errors, "direct discovery progress");
        }
    }
    tracing::info!(origins = total, routes, errors, "direct discovery finished");
    Ok(())
}
