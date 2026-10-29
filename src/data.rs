use crate::models::{Airport, AirportRaw, City, CityPoint, CityRaw, Country, CountryPoint, CountryRaw};
use anyhow::{Context, Result};
use chrono_tz::Tz;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const AIRPORTS_URL: &str = "https://api.travelpayouts.com/data/ru/airports.json";
const CITIES_URL: &str = "https://api.travelpayouts.com/data/ru/cities.json";
const COUNTRIES_URL: &str = "https://api.travelpayouts.com/data/ru/countries.json";
const ROUTES_URL: &str = "https://raw.githubusercontent.com/jpatokal/openflights/master/data/routes.dat";

const CURATED_HUBS: &[&str] = &[
    "IST", "DXB", "DOH", "AUH", "RUH", "JED", "FRA", "MUC", "AMS", "CDG", "LHR", "LGW", "MAD",
    "BCN", "FCO", "MXP", "ZRH", "VIE", "CPH", "ARN", "OSL", "HEL", "WAW", "PRG", "BUD", "ATH",
    "CAI", "ADD", "NBO", "JNB", "DEL", "BOM", "SIN", "KUL", "BKK", "HKG", "PVG", "PEK", "CAN",
    "ICN", "NRT", "HND", "JFK", "EWR", "ORD", "ATL", "LAX", "SFO", "MIA", "DFW", "YYZ", "YVR",
    "MEX", "GRU", "EZE", "BOG", "LIM", "SCL", "SYD", "MEL", "AKL", "MOW", "LED", "KBP", "ALA",
    "TAS", "TSE", "GYD", "TBS", "EVN", "AYT", "TLV",
];

#[derive(Debug, Default, Clone)]
pub struct Topology {
    pub adj: HashMap<String, Vec<String>>,
    pub radj: HashMap<String, Vec<String>>,
}

impl Topology {
    pub fn has_edge(&self, from: &str, to: &str) -> bool {
        self.adj
            .get(from)
            .map(|v| v.iter().any(|x| x == to))
            .unwrap_or(false)
    }
}

pub struct StaticData {
    pub cities: HashMap<String, City>,
    pub countries: HashMap<String, Country>,
    pub airports: Vec<Airport>,
    pub airport_by_code: HashMap<String, Airport>,
    pub airport_to_city: HashMap<String, String>,
    pub topology: Topology,
    pub hubs: Vec<String>,
    pub tz_by_airport: HashMap<String, Option<Tz>>,
    pub tz_by_city: HashMap<String, Option<Tz>>,
    pub topology_warning: Option<String>,
}

fn parse_tz(value: &str) -> Option<Tz> {
    if value.trim().is_empty() {
        return None;
    }
    value.trim().parse::<Tz>().ok()
}

fn normalize_name(value: &str) -> String {
    value
        .to_lowercase()
        .replace('ё', "е")
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_iata_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 3 && bytes.iter().all(|value| value.is_ascii_uppercase())
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0088_f64;
    let p1 = lat1.to_radians();
    let p2 = lat2.to_radians();
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

async fn ensure_file(
    client: &reqwest::Client,
    path: &Path,
    url: &str,
    max_age: Duration,
) -> Result<()> {
    if let Ok(metadata) = tokio::fs::metadata(path).await {
        if let Ok(modified) = metadata.modified() {
            if modified.elapsed().map(|age| age < max_age).unwrap_or(false) {
                return Ok(());
            }
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let download = async {
        let response = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("download failed: {url}"))?
            .error_for_status()
            .with_context(|| format!("download failed: {url}"))?;
        let bytes = response.bytes().await?;
        tokio::fs::write(path, &bytes).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match download {
        Ok(()) => Ok(()),
        Err(error) => {
            if tokio::fs::metadata(path).await.is_ok() {
                tracing::warn!("using cached file {url}: {error}");
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

impl StaticData {
    pub async fn load(client: &reqwest::Client, data_dir: &Path) -> Result<Arc<Self>> {
        tokio::fs::create_dir_all(data_dir).await?;
        let airports_path = data_dir.join("airports.json");
        let cities_path = data_dir.join("cities.json");
        let countries_path = data_dir.join("countries.json");
        let routes_path = data_dir.join("routes.dat");

        let max_age_hours: u64 = std::env::var("STATIC_DATA_MAX_AGE_HOURS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(24);
        let max_age = if max_age_hours == 0 {
            Duration::from_secs(u64::MAX / 2)
        } else {
            Duration::from_secs(max_age_hours.saturating_mul(3600))
        };
        ensure_file(client, &airports_path, AIRPORTS_URL, max_age).await?;
        ensure_file(client, &cities_path, CITIES_URL, max_age).await?;
        ensure_file(client, &countries_path, COUNTRIES_URL, max_age).await?;

        let airports_raw: Vec<AirportRaw> = serde_json::from_slice(
            &tokio::fs::read(&airports_path)
                .await
                .context("read airports.json")?,
        )
        .context("parse airports.json")?;
        let cities_raw: Vec<CityRaw> = serde_json::from_slice(
            &tokio::fs::read(&cities_path)
                .await
                .context("read cities.json")?,
        )
        .context("parse cities.json")?;
        let countries_raw: Vec<CountryRaw> = serde_json::from_slice(
            &tokio::fs::read(&countries_path)
                .await
                .context("read countries.json")?,
        )
        .context("parse countries.json")?;

        let mut countries: HashMap<String, Country> = HashMap::new();
        for raw in &countries_raw {
            let code = raw.code.trim().to_uppercase();
            if code.is_empty() {
                continue;
            }
            let raw_name = raw.name.clone().unwrap_or_default();
            let name_en = raw
                .name_translations
                .get("en")
                .cloned()
                .unwrap_or_else(|| raw_name.clone());
            countries.insert(
                code.clone(),
                Country {
                    code,
                    name: if raw_name.is_empty() {
                        name_en.clone()
                    } else {
                        raw_name
                    },
                    name_en,
                },
            );
        }

        let mut cities: HashMap<String, City> = HashMap::new();
        let mut tz_by_city: HashMap<String, Option<Tz>> = HashMap::new();

        for raw in &cities_raw {
            let code = raw.code.trim().to_uppercase();
            if code.is_empty() {
                continue;
            }
            let Some(coords) = &raw.coordinates else {
                continue;
            };
            let raw_name = raw.name.clone().unwrap_or_default();
            let name_en = raw
                .name_translations
                .get("en")
                .cloned()
                .unwrap_or_else(|| raw_name.clone());
            let tz = parse_tz(&raw.time_zone);
            cities.insert(
                code.clone(),
                City {
                    code: code.clone(),
                    name: if raw_name.is_empty() {
                        name_en.clone()
                    } else {
                        raw_name.clone()
                    },
                    name_en,
                    country_code: raw.country_code.trim().to_uppercase(),
                    lat: coords.lat,
                    lon: coords.lon,
                },
            );
            tz_by_city.insert(code, tz);
        }

        let mut airports = Vec::new();
        let mut airport_by_code = HashMap::new();
        let mut airport_to_city = HashMap::new();
        let mut tz_by_airport = HashMap::new();

        for raw in &airports_raw {
            let code = raw.code.trim().to_uppercase();
            if code.is_empty() {
                continue;
            }
            let Some(coords) = &raw.coordinates else {
                continue;
            };
            let city_code = raw.city_code.trim().to_uppercase();
            let raw_name = raw.name.clone().unwrap_or_default();
            let name_en = raw
                .name_translations
                .get("en")
                .cloned()
                .unwrap_or_else(|| raw_name.clone());
            let tz = parse_tz(&raw.time_zone);
            let airport = Airport {
                code: code.clone(),
                city_code: city_code.clone(),
                name: if raw_name.is_empty() {
                    name_en.clone()
                } else {
                    raw_name.clone()
                },
                name_en,
                country_code: raw.country_code.trim().to_uppercase(),
                lat: coords.lat,
                lon: coords.lon,
                flightable: raw.flightable,
                iata_type: raw.iata_type.trim().to_lowercase(),
            };
            if !city_code.is_empty() {
                airport_to_city.insert(code.clone(), city_code.clone());
                tz_by_city.entry(city_code.clone()).or_insert(tz);
                cities.entry(city_code.clone()).or_insert_with(|| City {
                    code: city_code,
                    name: airport.name.clone(),
                    name_en: airport.name_en.clone(),
                    country_code: airport.country_code.clone(),
                    lat: airport.lat,
                    lon: airport.lon,
                });
            }
            tz_by_airport.insert(code.clone(), tz);
            airport_by_code.insert(code, airport.clone());
            airports.push(airport);
        }

        let mut topology = Topology::default();
        let mut topology_warning = None;
        match ensure_file(client, &routes_path, ROUTES_URL, max_age).await {
            Ok(()) => match tokio::fs::read_to_string(&routes_path).await {
                Ok(text) => {
                    let mut adj_sets: HashMap<String, HashSet<String>> = HashMap::new();
                    let mut radj_sets: HashMap<String, HashSet<String>> = HashMap::new();
                    for line in text.lines() {
                        let fields: Vec<&str> = line.split(',').collect();
                        if fields.len() < 8 {
                            continue;
                        }
                        let from_airport = fields[2].trim().to_uppercase();
                        let to_airport = fields[4].trim().to_uppercase();
                        if from_airport.is_empty()
                            || to_airport.is_empty()
                            || from_airport == "\\N"
                            || to_airport == "\\N"
                        {
                            continue;
                        }
                        let stops = fields[7].trim().parse::<u32>().unwrap_or(0);
                        if stops > 2 {
                            continue;
                        }
                        let Some(from_city) = airport_to_city.get(&from_airport) else {
                            continue;
                        };
                        let Some(to_city) = airport_to_city.get(&to_airport) else {
                            continue;
                        };
                        if from_city == to_city {
                            continue;
                        }
                        adj_sets
                            .entry(from_city.clone())
                            .or_default()
                            .insert(to_city.clone());
                        radj_sets
                            .entry(to_city.clone())
                            .or_default()
                            .insert(from_city.clone());
                    }
                    for (k, v) in adj_sets {
                        let mut list: Vec<String> = v.into_iter().collect();
                        list.sort();
                        topology.adj.insert(k, list);
                    }
                    for (k, v) in radj_sets {
                        let mut list: Vec<String> = v.into_iter().collect();
                        list.sort();
                        topology.radj.insert(k, list);
                    }
                }
                Err(err) => {
                    topology_warning = Some(format!("Не удалось прочитать routes.dat: {err}"));
                }
            },
            Err(err) => {
                topology_warning = Some(format!(
                    "Не удалось загрузить топологию OpenFlights: {err}. Поиск с пересадками будет ограничен."
                ));
            }
        }

        let mut hubs = Vec::new();
        let mut hub_seen = HashSet::new();
        for hub in CURATED_HUBS {
            let code = hub.to_uppercase();
            if cities.contains_key(&code) && hub_seen.insert(code.clone()) {
                hubs.push(code);
            }
        }
        let mut by_degree: Vec<(String, usize)> = topology
            .adj
            .iter()
            .map(|(code, list)| {
                let reverse = topology.radj.get(code).map(|v| v.len()).unwrap_or(0);
                (code.clone(), list.len() + reverse)
            })
            .filter(|(code, _)| cities.contains_key(code))
            .collect();
        by_degree.sort_by(|a, b| b.1.cmp(&a.1));
        for (code, _) in by_degree.into_iter().take(40) {
            if hub_seen.insert(code.clone()) {
                hubs.push(code);
            }
        }
        hubs.truncate(90);

        Ok(Arc::new(Self {
            cities,
            countries,
            airports,
            airport_by_code,
            airport_to_city,
            topology,
            hubs,
            tz_by_airport,
            tz_by_city,
            topology_warning,
        }))
    }

    pub fn resolve_city_code(&self, input: &str) -> Option<String> {
        let code = input.trim().to_uppercase();
        if code.is_empty() {
            return None;
        }
        if self.cities.contains_key(&code) {
            return Some(code);
        }
        if let Some(city_code) = self.airport_to_city.get(&code) {
            return Some(city_code.clone());
        }
        let lower = normalize_name(input);
        if lower.is_empty() {
            return None;
        }
        let mut matches = self.cities.values().filter(|city| {
            normalize_name(&city.name) == lower || normalize_name(&city.name_en) == lower
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.code.clone())
    }

    pub fn city_point(&self, code: &str) -> Option<CityPoint> {
        let code = code.trim().to_uppercase();
        if let Some(city) = self.cities.get(&code) {
            return Some(CityPoint {
                code: city.code.clone(),
                name: city.name.clone(),
                country: city.country_code.clone(),
                lat: city.lat,
                lon: city.lon,
                distance_km: None,
            });
        }
        if let Some(city_code) = self.airport_to_city.get(&code) {
            if let Some(city) = self.cities.get(city_code) {
                return Some(CityPoint {
                    code: city.code.clone(),
                    name: city.name.clone(),
                    country: city.country_code.clone(),
                    lat: city.lat,
                    lon: city.lon,
                    distance_km: None,
                });
            }
        }
        self.airport_by_code.get(&code).map(|airport| CityPoint {
            code: airport.code.clone(),
            name: airport.name.clone(),
            country: airport.country_code.clone(),
            lat: airport.lat,
            lon: airport.lon,
            distance_km: None,
        })
    }

    pub fn search_cities(&self, query: &str, limit: usize) -> Vec<CityPoint> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let query_norm = normalize_name(&query);
        let query_upper = query.to_uppercase();
        let mut scored: Vec<(u8, CityPoint)> = Vec::new();
        let mut seen = HashSet::new();

        if let Some(city) = self.cities.get(&query_upper) {
            seen.insert(city.code.clone());
            if let Some(point) = self.city_point(&city.code) {
                scored.push((0, point));
            }
        }
        if let Some(city_code) = self.airport_to_city.get(&query_upper) {
            if seen.insert(query_upper.clone()) {
                if let Some(mut point) = self.city_point(city_code) {
                    if let Some(airport) = self.airport_by_code.get(&query_upper) {
                        point.name = format!("{} · {}", point.name, airport.name);
                    }
                    point.code = query_upper.clone();
                    scored.push((1, point));
                }
            }
        }

        for city in self.cities.values() {
            let code = city.code.to_lowercase();
            let name = normalize_name(&city.name);
            let name_en = normalize_name(&city.name_en);
            let score = if code == query {
                0
            } else if code.starts_with(&query) {
                2
            } else if name.starts_with(&query_norm) || name_en.starts_with(&query_norm) {
                3
            } else if name.contains(&query_norm) || name_en.contains(&query_norm) {
                4
            } else {
                continue;
            };
            if !seen.insert(city.code.clone()) {
                continue;
            }
            if let Some(point) = self.city_point(&city.code) {
                scored.push((score, point));
            }
        }

        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
        });
        scored
            .into_iter()
            .take(limit.clamp(1, 50))
            .map(|(_, point)| point)
            .collect()
    }

    pub fn resolve_country_code(&self, input: &str) -> Option<String> {
        let code = input.trim().to_uppercase();
        if code.is_empty() {
            return None;
        }
        if self.countries.contains_key(&code) {
            return Some(code);
        }
        let lower = normalize_name(input);
        let mut matches = self.countries.values().filter(|country| {
            normalize_name(&country.name) == lower || normalize_name(&country.name_en) == lower
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.code.clone())
    }

    pub fn search_countries(&self, query: &str, limit: usize) -> Vec<CountryPoint> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let query_norm = normalize_name(&query);
        let query_upper = query.to_uppercase();
        let mut scored: Vec<(u8, CountryPoint)> = Vec::new();
        if let Some(country) = self.countries.get(&query_upper) {
            scored.push((
                0,
                CountryPoint {
                    code: country.code.clone(),
                    name: country.name.clone(),
                },
            ));
        }
        for country in self.countries.values() {
            let code = country.code.to_lowercase();
            let name = normalize_name(&country.name);
            let name_en = normalize_name(&country.name_en);
            let score = if code == query {
                0
            } else if code.starts_with(&query) {
                1
            } else if name.starts_with(&query_norm) || name_en.starts_with(&query_norm) {
                2
            } else if name.contains(&query_norm) || name_en.contains(&query_norm) {
                3
            } else {
                continue;
            };
            if scored.iter().any(|(_, item)| item.code == country.code) {
                continue;
            }
            scored.push((
                score,
                CountryPoint {
                    code: country.code.clone(),
                    name: country.name.clone(),
                },
            ));
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
        });
        scored
            .into_iter()
            .take(limit.clamp(1, 50))
            .map(|(_, item)| item)
            .collect()
    }

    pub fn airports_for_city(&self, city_code: &str) -> Vec<String> {
        let mut codes: Vec<String> = self
            .airports
            .iter()
            .filter(|airport| {
                airport.flightable
                    && airport.iata_type == "airport"
                    && is_iata_code(&airport.code)
                    && airport.city_code == city_code
            })
            .map(|airport| airport.code.clone())
            .collect();
        codes.sort();
        codes.dedup();
        codes
    }

    pub fn resolve_airport_codes(&self, input: &str) -> Option<Vec<String>> {
        let code = input.trim().to_uppercase();
        if let Some(airport) = self.airport_by_code.get(&code) {
            if airport.iata_type == "airport" && airport.flightable && is_iata_code(&code) {
                return Some(vec![code]);
            }
        }
        if let Some(city_code) = self.resolve_city_code(input) {
            let airports = self.airports_for_city(&city_code);
            if !airports.is_empty() {
                return Some(airports);
            }
        }
        if let Some(airport) = self.airport_by_code.get(&code) {
            if airport.iata_type == "airport" && is_iata_code(&code) {
                return Some(vec![code]);
            }
        }
        None
    }

    pub fn airport_point(&self, airport_code: &str) -> Option<CityPoint> {
        let airport = self.airport_by_code.get(airport_code)?;
        if airport.iata_type != "airport" || !is_iata_code(&airport.code) {
            return None;
        }
        let name = self
            .cities
            .get(&airport.city_code)
            .map(|city| city.name.clone())
            .unwrap_or_else(|| airport.name.clone());
        Some(CityPoint {
            code: airport.code.clone(),
            name,
            country: airport.country_code.clone(),
            lat: airport.lat,
            lon: airport.lon,
            distance_km: None,
        })
    }

    pub fn airports_in_radius(
        &self,
        lat: f64,
        lon: f64,
        radius_km: f64,
        limit: usize,
    ) -> Vec<CityPoint> {
        let radius = radius_km.max(1.0);
        let mut best: HashMap<String, (f64, usize)> = HashMap::new();
        for airport in &self.airports {
            if !airport.flightable
                || airport.iata_type != "airport"
                || !is_iata_code(&airport.code)
            {
                continue;
            }
            let distance = haversine_km(lat, lon, airport.lat, airport.lon);
            if distance > radius {
                continue;
            }
            let degree = self.degree(&airport.city_code);
            best.entry(airport.code.clone())
                .or_insert((distance, degree));
        }
        let mut list: Vec<(String, f64, usize)> = best
            .into_iter()
            .map(|(code, (distance, degree))| (code, distance, degree))
            .collect();
        list.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        list.truncate(limit.clamp(1, 400));
        list.into_iter()
            .filter_map(|(code, distance, _)| {
                self.airport_point(&code).map(|mut point| {
                    point.distance_km = Some((distance * 10.0).round() / 10.0);
                    point
                })
            })
            .collect()
    }

    pub fn airports_in_country(&self, country_code: &str, limit: usize) -> Vec<CityPoint> {
        let country_code = country_code.trim().to_uppercase();
        let mut seen = HashSet::new();
        let mut airports: Vec<(String, usize)> = Vec::new();
        for airport in &self.airports {
            if !airport.flightable
                || airport.iata_type != "airport"
                || !is_iata_code(&airport.code)
                || airport.country_code != country_code
            {
                continue;
            }
            if !seen.insert(airport.code.clone()) {
                continue;
            }
            airports.push((airport.code.clone(), self.degree(&airport.city_code)));
        }
        airports.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        airports.truncate(limit.clamp(1, 400));
        airports
            .into_iter()
            .filter_map(|(code, _)| self.airport_point(&code))
            .collect()
    }

    pub fn airport_coords(&self, code: &str) -> Option<(f64, f64)> {
        self.airport_by_code.get(code).map(|a| (a.lat, a.lon))
    }

    pub fn city_name(&self, code: &str) -> String {
        self.cities
            .get(code)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| code.to_string())
    }

    pub fn airport_timezone(&self, code: &str) -> Option<Tz> {
        self.tz_by_airport.get(code).copied().flatten()
    }

    pub fn city_timezone(&self, code: &str) -> Option<Tz> {
        self.tz_by_city.get(code).copied().flatten()
    }

    pub fn degree(&self, code: &str) -> usize {
        self.topology.adj.get(code).map(|v| v.len()).unwrap_or(0)
            + self.topology.radj.get(code).map(|v| v.len()).unwrap_or(0)
    }

    pub fn min_distance_to_dest(&self, code: &str, dests: &HashSet<String>) -> f64 {
        let Some(city) = self.cities.get(code) else {
            return f64::INFINITY;
        };
        dests
            .iter()
            .filter_map(|dest| self.cities.get(dest))
            .map(|dest| haversine_km(city.lat, city.lon, dest.lat, dest.lon))
            .fold(f64::INFINITY, f64::min)
    }

    pub fn bfs_dist(
        starts: &[String],
        graph: &HashMap<String, Vec<String>>,
        max_depth: usize,
    ) -> HashMap<String, usize> {
        let mut dist = HashMap::new();
        let mut queue = VecDeque::new();
        for start in starts {
            if dist.insert(start.clone(), 0).is_none() {
                queue.push_back(start.clone());
            }
        }
        while let Some(node) = queue.pop_front() {
            let depth = dist[&node];
            if depth >= max_depth {
                continue;
            }
            if let Some(neighbors) = graph.get(&node) {
                for next in neighbors {
                    if !dist.contains_key(next) {
                        dist.insert(next.clone(), depth + 1);
                        queue.push_back(next.clone());
                    }
                }
            }
        }
        dist
    }
}
