use crate::data::{haversine_km, StaticData};
use crate::models::{CacheStats, DirectionsResponse, Offer, PriceOfferRaw, PricesResponse};
use crate::store::Store;
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use chrono_tz::Tz;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};

const CACHE_TTL: Duration = Duration::from_secs(3600);
const MAX_CACHE_ROUTES: usize = 1500;
const MAX_CONCURRENT: usize = 6;

struct CacheEntry {
    offers: Arc<Vec<Offer>>,
    fetched: Instant,
}

struct DirectionsEntry {
    codes: Vec<String>,
    fetched: Instant,
}

pub struct Travelpayouts {
    http: reqwest::Client,
    token: String,
    data: Arc<StaticData>,
    cache: RwLock<HashMap<String, CacheEntry>>,
    directions: RwLock<HashMap<String, DirectionsEntry>>,
    semaphore: Semaphore,
    store: Option<Store>,
    db_ttl: Duration,
    db_horizon_days: u32,
}

fn route_key(origin: &str, destination: &str, currency: &str) -> String {
    format!("{origin}|{destination}|{currency}")
}

fn direction_key(label: &str, origin: &str, currency: &str) -> String {
    format!("{label}|{origin}|{currency}")
}

fn local_date(utc: DateTime<Utc>, tz: Option<Tz>, fallback: FixedOffset) -> NaiveDate {
    match tz {
        Some(tz) => utc.with_timezone(&tz).date_naive(),
        None => utc.with_timezone(&fallback).date_naive(),
    }
}

fn short(text: &str) -> String {
    if text.len() > 240 {
        format!("{}...", &text[..240])
    } else {
        text.to_string()
    }
}

impl Travelpayouts {
    pub fn new(
        token: String,
        data: Arc<StaticData>,
        store: Option<Store>,
        db_ttl: Duration,
        db_horizon_days: u32,
    ) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(35))
            .user_agent("aviasales-pathfinder/0.1")
            .build()
            .expect("http client");
        Arc::new(Self {
            http,
            token,
            data,
            cache: RwLock::new(HashMap::new()),
            directions: RwLock::new(HashMap::new()),
            semaphore: Semaphore::new(MAX_CONCURRENT),
            store,
            db_ttl,
            db_horizon_days,
        })
    }

    pub async fn get_route_offers(
        &self,
        origin: &str,
        destination: &str,
        currency: &str,
    ) -> Result<Arc<Vec<Offer>>, String> {
        let key = route_key(origin, destination, currency);
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&key) {
                if entry.fetched.elapsed() < CACHE_TTL {
                    return Ok(entry.offers.clone());
                }
            }
        }

        if let Some(store) = &self.store {
            match store
                .fresh_offers(origin, destination, currency, self.db_ttl)
                .await
            {
                Ok(Some(offers)) => {
                    let offers = Arc::new(offers);
                    let mut cache = self.cache.write().await;
                    cache.insert(
                        key,
                        CacheEntry {
                            offers: offers.clone(),
                            fetched: Instant::now(),
                        },
                    );
                    drop(cache);
                    self.maybe_evict().await;
                    return Ok(offers);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!("db read {origin}->{destination}: {error}");
                }
            }
        }

        self.fetch_route_offers(origin, destination, currency, true)
            .await
    }

    pub async fn fetch_route_offers(
        &self,
        origin: &str,
        destination: &str,
        currency: &str,
        store_it: bool,
    ) -> Result<Arc<Vec<Offer>>, String> {
        let key = route_key(origin, destination, currency);
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| "api semaphore closed".to_string())?;

        let mut last_error = String::new();
        for attempt in 0..3 {
            let request = self
                .http
                .get("https://api.travelpayouts.com/aviasales/v3/prices_for_dates")
                .query(&[
                    ("origin", origin),
                    ("destination", destination),
                    ("one_way", "true"),
                    ("unique", "false"),
                    ("sorting", "price"),
                    ("direct", "true"),
                    ("currency", currency),
                    ("limit", "1000"),
                    ("page", "1"),
                    ("token", self.token.as_str()),
                ]);
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        let parsed: PricesResponse = match serde_json::from_str(&text) {
                            Ok(value) => value,
                            Err(err) => {
                                return Err(format!(
                                    "{} -> {}: {}",
                                    origin,
                                    destination,
                                    short(&err.to_string())
                                ));
                            }
                        };
                        if parsed.success {
                            let offers = Arc::new(self.convert_offers(parsed.data.unwrap_or_default()));
                            {
                                let mut cache = self.cache.write().await;
                                cache.insert(
                                    key.clone(),
                                    CacheEntry {
                                        offers: offers.clone(),
                                        fetched: Instant::now(),
                                    },
                                );
                            }
                            self.maybe_evict().await;
                            if store_it {
                                if let Some(store) = &self.store {
                                    let filtered = self.filter_for_db(&offers);
                                    if let Err(error) = store
                                        .replace_route_offers(
                                            origin,
                                            destination,
                                            currency,
                                            &filtered,
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            "db write {origin}->{destination}: {error}"
                                        );
                                    }
                                }
                            }
                            return Ok(offers);
                        }
                        last_error = parsed
                            .error
                            .unwrap_or_else(|| "API success=false".to_string());
                        break;
                    }
                    last_error = format!("HTTP {}: {}", status.as_u16(), short(&text));
                    let retryable =
                        status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error();
                    if !retryable {
                        break;
                    }
                }
                Err(err) => {
                    last_error = format!("{origin} -> {destination}: {err}");
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(600 * (attempt as u64 + 1))).await;
            }
        }

        Err(last_error)
    }

    fn filter_for_db(&self, offers: &[Offer]) -> Vec<Offer> {
        let now = Utc::now() - chrono::Duration::hours(6);
        let horizon = Utc::now() + chrono::Duration::days(self.db_horizon_days as i64);
        offers
            .iter()
            .filter(|offer| offer.depart_at >= now && offer.depart_at <= horizon)
            .cloned()
            .collect()
    }

    fn convert_offers(&self, raw_offers: Vec<PriceOfferRaw>) -> Vec<Offer> {
        let mut offers = Vec::with_capacity(raw_offers.len());
        let now = Utc::now();
        for raw in raw_offers {
            if raw
                .return_at
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                continue;
            }
            if raw.transfers.unwrap_or(0) != 0 {
                continue;
            }
            if !raw.price.is_finite() || raw.price <= 0.0 {
                continue;
            }
            let Ok(depart) = DateTime::parse_from_rfc3339(&raw.departure_at) else {
                continue;
            };
            let duration = raw.duration.unwrap_or(0);
            if !(15..=2160).contains(&duration) {
                continue;
            }
            let fallback = *depart.offset();
            let depart_utc = depart.with_timezone(&Utc);
            let arrive_utc = depart_utc + chrono::Duration::minutes(duration);
            if depart_utc < now - chrono::Duration::hours(24) || arrive_utc <= depart_utc {
                continue;
            }
            let origin = raw.origin.trim().to_uppercase();
            let destination = raw.destination.trim().to_uppercase();
            if origin.is_empty() || destination.is_empty() || origin == destination {
                continue;
            }
            let Some(origin_city) = self.data.cities.get(&origin) else {
                continue;
            };
            let Some(destination_city) = self.data.cities.get(&destination) else {
                continue;
            };
            let distance = haversine_km(
                origin_city.lat,
                origin_city.lon,
                destination_city.lat,
                destination_city.lon,
            );
            if distance / (duration as f64 / 60.0) > 1200.0 {
                continue;
            }
            let origin_airport = raw
                .origin_airport
                .unwrap_or_else(|| origin.clone())
                .trim()
                .to_uppercase();
            let destination_airport = raw
                .destination_airport
                .unwrap_or_else(|| destination.clone())
                .trim()
                .to_uppercase();
            if self
                .data
                .airport_by_code
                .get(&origin_airport)
                .map(|airport| airport.iata_type != "airport")
                .unwrap_or(false)
                || self
                    .data
                    .airport_by_code
                    .get(&destination_airport)
                    .map(|airport| airport.iata_type != "airport")
                    .unwrap_or(false)
            {
                continue;
            }
            let origin_tz = self
                .data
                .airport_timezone(&origin_airport)
                .or_else(|| self.data.city_timezone(&origin));
            let destination_tz = self
                .data
                .airport_timezone(&destination_airport)
                .or_else(|| self.data.city_timezone(&destination));
            let depart_local_date = depart.date_naive();
            let arrive_local_date = local_date(arrive_utc, destination_tz, fallback);
            let link = raw.link.unwrap_or_default();
            let link = if link.starts_with('/') {
                format!("https://www.aviasales.ru{link}")
            } else {
                link
            };
            offers.push(Offer {
                origin,
                destination,
                origin_airport,
                destination_airport,
                depart_at: depart_utc,
                arrive_at: arrive_utc,
                depart_local_date,
                arrive_local_date,
                origin_tz,
                destination_tz,
                fallback_offset: fallback,
                price: raw.price,
                transfers: raw.transfers.unwrap_or(0),
                duration_min: duration,
                airline: raw.airline.unwrap_or_default(),
                flight_number: raw.flight_number.unwrap_or_default(),
                link,
            });
        }

        offers.sort_by(|a, b| {
            a.depart_at
                .cmp(&b.depart_at)
                .then_with(|| {
                    a.price
                        .partial_cmp(&b.price)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let mut seen: HashSet<(DateTime<Utc>, String, String, i64, u32, String)> = HashSet::new();
        offers.retain(|offer| {
            seen.insert((
                offer.depart_at,
                offer.origin_airport.clone(),
                offer.destination_airport.clone(),
                offer.duration_min,
                offer.transfers,
                offer.flight_number.clone(),
            ))
        });
        offers
    }

    async fn maybe_evict(&self) {
        let mut cache = self.cache.write().await;
        let now = Instant::now();
        cache.retain(|_, entry| now.duration_since(entry.fetched) < CACHE_TTL);
        if cache.len() > MAX_CACHE_ROUTES {
            let mut entries: Vec<(String, Instant)> = cache
                .iter()
                .map(|(key, entry)| (key.clone(), entry.fetched))
                .collect();
            entries.sort_by_key(|(_, fetched)| *fetched);
            let remove = cache.len() - MAX_CACHE_ROUTES;
            for (key, _) in entries.into_iter().take(remove) {
                cache.remove(&key);
            }
        }
    }

    pub async fn get_city_directions(
        &self,
        origin: &str,
        currency: &str,
    ) -> Result<Vec<String>, String> {
        self.fetch_destination_codes(
            "city-directions",
            "https://api.travelpayouts.com/v1/city-directions",
            origin,
            currency,
            120,
        )
        .await
    }

    pub async fn get_direct_destinations(
        &self,
        origin: &str,
        currency: &str,
    ) -> Result<Vec<String>, String> {
        self.fetch_destination_codes(
            "direct",
            "https://api.travelpayouts.com/v1/prices/direct",
            origin,
            currency,
            usize::MAX,
        )
        .await
    }

    async fn fetch_destination_codes(
        &self,
        label: &str,
        url: &str,
        origin: &str,
        currency: &str,
        limit: usize,
    ) -> Result<Vec<String>, String> {
        let key = direction_key(label, origin, currency);
        {
            let cache = self.directions.read().await;
            if let Some(entry) = cache.get(&key) {
                if entry.fetched.elapsed() < CACHE_TTL {
                    return Ok(entry.codes.clone());
                }
            }
        }

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| "api semaphore closed".to_string())?;

        let mut last_error = String::new();
        for attempt in 0..3 {
            let request = self.http.get(url).query(&[
                ("origin", origin),
                ("currency", currency),
                ("token", self.token.as_str()),
            ]);
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        let parsed: DirectionsResponse = match serde_json::from_str(&text) {
                            Ok(value) => value,
                            Err(err) => {
                                return Err(format!(
                                    "{label} {origin}: {}",
                                    short(&err.to_string())
                                ));
                            }
                        };
                        if parsed.success {
                            let mut list: Vec<(String, f64)> = parsed
                                .data
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(code, value)| (code.to_uppercase(), value.price))
                                .collect();
                            list.sort_by(|a, b| {
                                a.1.partial_cmp(&b.1)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                                    .then_with(|| a.0.cmp(&b.0))
                            });
                            let codes: Vec<String> = list
                                .into_iter()
                                .take(limit)
                                .map(|(code, _)| code)
                                .collect();
                            {
                                let mut cache = self.directions.write().await;
                                cache.insert(
                                    key.clone(),
                                    DirectionsEntry {
                                        codes: codes.clone(),
                                        fetched: Instant::now(),
                                    },
                                );
                            }
                            return Ok(codes);
                        }
                        last_error = parsed
                            .error
                            .unwrap_or_else(|| "API success=false".to_string());
                        break;
                    }
                    last_error = format!("HTTP {}: {}", status.as_u16(), short(&text));
                    let retryable =
                        status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error();
                    if !retryable {
                        break;
                    }
                }
                Err(err) => {
                    last_error = format!("{label} {origin}: {err}");
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(600 * (attempt as u64 + 1))).await;
            }
        }

        Err(last_error)
    }

    pub async fn stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        CacheStats {
            routes: cache.len(),
            offers: cache.values().map(|entry| entry.offers.len()).sum(),
        }
    }
}
