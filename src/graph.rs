use crate::data::is_iata_code;
use crate::models::Offer;
use crate::store::Store;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Graph {
    pub offers: Vec<Offer>,
    pub outgoing: HashMap<String, Vec<usize>>,
    pub by_route: HashMap<(String, String), Vec<usize>>,
    pub destinations_by_origin: HashMap<String, Vec<String>>,
    pub origins_by_destination: HashMap<String, Vec<String>>,
    pub route_set: HashSet<(String, String)>,
    pub built_at: DateTime<Utc>,
    pub currency: String,
}

impl Graph {
    fn empty(currency: String) -> Self {
        Self {
            offers: Vec::new(),
            outgoing: HashMap::new(),
            by_route: HashMap::new(),
            destinations_by_origin: HashMap::new(),
            origins_by_destination: HashMap::new(),
            route_set: HashSet::new(),
            built_at: Utc::now(),
            currency,
        }
    }

    fn build(mut offers: Vec<Offer>, currency: String) -> Self {
        offers.retain(|offer| {
            is_iata_code(&offer.origin_airport) && is_iata_code(&offer.destination_airport)
        });
        offers.sort_by(|a, b| {
            a.depart_at
                .cmp(&b.depart_at)
                .then_with(|| {
                    a.price
                        .partial_cmp(&b.price)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let mut outgoing: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_route: HashMap<(String, String), Vec<usize>> = HashMap::new();
        let mut route_set: HashSet<(String, String)> = HashSet::new();
        for (index, offer) in offers.iter().enumerate() {
            outgoing
                .entry(offer.origin_airport.clone())
                .or_default()
                .push(index);
            by_route
                .entry((offer.origin_airport.clone(), offer.destination_airport.clone()))
                .or_default()
                .push(index);
            route_set.insert((offer.origin_airport.clone(), offer.destination_airport.clone()));
        }
        let mut destinations_by_origin: HashMap<String, Vec<String>> = HashMap::new();
        let mut origins_by_destination: HashMap<String, Vec<String>> = HashMap::new();
        for (origin, destination) in &route_set {
            destinations_by_origin
                .entry(origin.clone())
                .or_default()
                .push(destination.clone());
            origins_by_destination
                .entry(destination.clone())
                .or_default()
                .push(origin.clone());
        }
        for list in destinations_by_origin.values_mut() {
            list.sort();
            list.dedup();
        }
        for list in origins_by_destination.values_mut() {
            list.sort();
            list.dedup();
        }
        Self {
            offers,
            outgoing,
            by_route,
            destinations_by_origin,
            origins_by_destination,
            route_set,
            built_at: Utc::now(),
            currency,
        }
    }

}

pub struct GraphCache {
    inner: RwLock<Arc<Graph>>,
    store: Store,
    currency: String,
    horizon_days: u32,
}

impl GraphCache {
    pub async fn new(store: Store, currency: String, horizon_days: u32) -> Arc<Self> {
        let cache = Arc::new(Self {
            inner: RwLock::new(Arc::new(Graph::empty(currency.clone()))),
            store,
            currency,
            horizon_days,
        });
        match cache.rebuild().await {
            Ok(count) => tracing::info!(offers = count, "ram graph loaded"),
            Err(error) => tracing::error!("ram graph load: {error}"),
        }
        cache
    }

    pub async fn snapshot(&self) -> Arc<Graph> {
        self.inner.read().await.clone()
    }

    pub async fn rebuild(&self) -> Result<usize> {
        let offers = self
            .store
            .load_offers_for_graph(&self.currency, self.horizon_days)
            .await?;
        let count = offers.len();
        let graph = Graph::build(offers, self.currency.clone());
        *self.inner.write().await = Arc::new(graph);
        Ok(count)
    }
}
