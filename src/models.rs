use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoordsRaw {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AirportRaw {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub city_code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_translations: HashMap<String, String>,
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub time_zone: String,
    #[serde(default)]
    pub iata_type: String,
    pub coordinates: Option<CoordsRaw>,
    #[serde(default = "default_true")]
    pub flightable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CityRaw {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_translations: HashMap<String, String>,
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub time_zone: String,
    pub coordinates: Option<CoordsRaw>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CountryRaw {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_translations: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Country {
    pub code: String,
    pub name: String,
    pub name_en: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountryPoint {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Airport {
    pub code: String,
    pub city_code: String,
    pub name: String,
    pub name_en: String,
    pub country_code: String,
    pub lat: f64,
    pub lon: f64,
    pub flightable: bool,
    pub iata_type: String,
}

#[derive(Debug, Clone)]
pub struct City {
    pub code: String,
    pub name: String,
    pub name_en: String,
    pub country_code: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CityPoint {
    pub code: String,
    pub name: String,
    pub country: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub origin: String,
    pub destination: String,
    pub origin_airport: String,
    pub destination_airport: String,
    pub depart_at: DateTime<Utc>,
    pub arrive_at: DateTime<Utc>,
    pub depart_local_date: NaiveDate,
    pub arrive_local_date: NaiveDate,
    pub origin_tz: Option<Tz>,
    pub destination_tz: Option<Tz>,
    pub fallback_offset: FixedOffset,
    pub price: f64,
    pub transfers: u32,
    pub duration_min: i64,
    pub airline: String,
    pub flight_number: String,
    pub link: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceOfferRaw {
    #[serde(default)]
    pub flight_number: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub origin_airport: Option<String>,
    #[serde(default)]
    pub destination_airport: Option<String>,
    pub departure_at: String,
    #[serde(default)]
    pub airline: Option<String>,
    pub origin: String,
    pub destination: String,
    pub price: f64,
    #[serde(default)]
    pub transfers: Option<u32>,
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(default)]
    pub return_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PricesResponse {
    #[serde(default)]
    pub data: Option<Vec<PriceOfferRaw>>,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirectionRaw {
    #[serde(default)]
    pub price: f64,
}

#[derive(Debug, Deserialize)]
pub struct DirectionsResponse {
    #[serde(default)]
    pub data: Option<HashMap<String, DirectionRaw>>,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Endpoint {
    City {
        code: String,
    },
    Region {
        lat: f64,
        lon: f64,
        radius_km: f64,
        #[serde(default)]
        limit: Option<usize>,
    },
    Country {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        codes: Vec<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    Any,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    pub origin: Endpoint,
    pub destination: Endpoint,
    #[serde(default)]
    pub depart_from: Option<NaiveDate>,
    #[serde(default)]
    pub depart_to: Option<NaiveDate>,
    #[serde(default)]
    pub arrive_from: Option<NaiveDate>,
    #[serde(default)]
    pub arrive_to: Option<NaiveDate>,
    #[serde(default)]
    pub max_transfers: Option<u8>,
    #[serde(default)]
    pub max_connection_days: Option<u32>,
    #[serde(default)]
    pub min_connection_minutes: Option<u32>,
    #[serde(default)]
    pub optimize: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub max_routes: Option<usize>,
    #[serde(default)]
    pub exclude_origin_countries: Vec<String>,
    #[serde(default)]
    pub exclude_destination_countries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CacheStats {
    pub routes: usize,
    pub offers: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub itineraries: Vec<ItineraryResponse>,
    pub groups: ItineraryGroups,
    pub origin_cities: Vec<CityPoint>,
    pub destination_cities: Vec<CityPoint>,
    pub routes_requested: usize,
    pub routes_failed: usize,
    pub offers_considered: usize,
    pub cache: CacheStats,
    pub warnings: Vec<String>,
    pub source: String,
    pub elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct ItineraryResponse {
    pub total_price: f64,
    pub currency: String,
    pub total_transfers: u32,
    pub travel_minutes: i64,
    pub elapsed_minutes: i64,
    pub depart_at: String,
    pub arrive_at: String,
    pub depart_tz: Option<String>,
    pub arrive_tz: Option<String>,
    pub score: f64,
    pub legs: Vec<LegResponse>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ItineraryGroups {
    pub price: Vec<usize>,
    pub transfers: Vec<usize>,
    pub dates: Vec<usize>,
    pub duration: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub struct LegResponse {
    pub origin: String,
    pub origin_name: String,
    pub destination: String,
    pub destination_name: String,
    pub origin_airport: String,
    pub destination_airport: String,
    pub origin_lat: f64,
    pub origin_lon: f64,
    pub destination_lat: f64,
    pub destination_lon: f64,
    pub depart_at: String,
    pub arrive_at: String,
    pub depart_tz: Option<String>,
    pub arrive_tz: Option<String>,
    pub price: f64,
    pub duration_min: i64,
    pub transfers: u32,
    pub airline: String,
    pub flight_number: String,
    pub link: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}


#[derive(Debug, Clone, Serialize, Default)]
pub struct DbStats {
    pub cities: i64,
    pub routes: i64,
    pub offers: i64,
    pub tracked_origins: i64,
    pub last_bulk_started: Option<String>,
    pub last_bulk_finished: Option<String>,
    pub last_bulk_status: Option<String>,
    pub last_bulk_origins: i64,
    pub last_bulk_routes: i64,
    pub last_bulk_offers: i64,
    pub ram_offers: i64,
    pub ram_routes: i64,
    pub ram_built_at: Option<String>,
}
