use crate::api::Travelpayouts;
use crate::data::{haversine_km, StaticData};
use crate::graph::{Graph, GraphCache};
use crate::models::{
    CityPoint, Endpoint, ItineraryGroups, ItineraryResponse, LegResponse, Offer, SearchRequest,
    SearchResponse,
};
use crate::store::Store;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Utc};
use chrono_tz::Tz;
use futures::StreamExt;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

const MAX_LABELS_PER_CITY: usize = 35;
const MAX_LABELS_TOTAL: usize = 9000;
const MAX_OFFERS_PER_LABEL: usize = 1500;
const MAX_FRONTIER_LABELS: usize = 4500;
const MAX_EXPANSIONS: usize = 2_000_000;
const MAX_ANY_CANDIDATES: usize = 5000;
const MAX_ANY_POINTS: usize = 200;
const MAX_PATHS: usize = 700;
const MAX_PATH_NODES: usize = 120_000;
const MAX_BRANCH: usize = 22;

enum OfferSource {
    Ram(Arc<Graph>),
    Live(Vec<Offer>),
}

impl OfferSource {
    fn offers(&self) -> &[Offer] {
        match self {
            Self::Ram(graph) => &graph.offers,
            Self::Live(offers) => offers,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Ram(_) => "ram",
            Self::Live(_) => "live",
        }
    }
}

struct Constraints {
    depart_from: NaiveDate,
    depart_to: NaiveDate,
    arrive_from: NaiveDate,
    arrive_to: NaiveDate,
    max_transfers: u32,
    max_connection_days: u32,
    min_connection_minutes: u32,
    max_legs: usize,
}

#[derive(Clone)]
struct Label {
    city: String,
    arrive_at: Option<DateTime<Utc>>,
    arrive_local_date: Option<NaiveDate>,
    price: f64,
    travel_min: i64,
    transfers: u32,
    legs: usize,
    visited: Vec<String>,
    offer_ids: Vec<usize>,
}

struct PathResult {
    offer_ids: Vec<usize>,
    price: f64,
    travel_min: i64,
    transfers: u32,
    depart_at: DateTime<Utc>,
    arrive_at: DateTime<Utc>,
}

struct PathGen<'a> {
    data: &'a StaticData,
    adj: &'a HashMap<String, Vec<String>>,
    min_to_dest: &'a HashMap<String, usize>,
    dest_set: &'a HashSet<String>,
    max_legs: usize,
    paths: Vec<Vec<String>>,
    visited: HashSet<String>,
    nodes: usize,
}

impl<'a> PathGen<'a> {
    fn dfs(&mut self, node: &str, path: &mut Vec<String>) {
        if self.paths.len() >= MAX_PATHS || self.nodes >= MAX_PATH_NODES {
            return;
        }
        self.nodes += 1;
        let depth = path.len() - 1;
        if depth > 0 && self.dest_set.contains(node) {
            self.paths.push(path.clone());
            return;
        }
        if depth >= self.max_legs {
            return;
        }
        let mut candidates: Vec<String> = Vec::new();
        if let Some(neighbors) = self.adj.get(node) {
            for next in neighbors {
                if self.visited.contains(next) {
                    continue;
                }
                if let Some(distance) = self.min_to_dest.get(next) {
                    if depth + 1 + *distance <= self.max_legs {
                        candidates.push(next.clone());
                    }
                }
            }
        }
        candidates.sort_by(|a, b| {
            let da = self.data.min_distance_to_dest(a, self.dest_set);
            let db = self.data.min_distance_to_dest(b, self.dest_set);
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| self.data.degree(b).cmp(&self.data.degree(a)))
        });
        candidates.truncate(MAX_BRANCH);
        for next in candidates {
            if self.paths.len() >= MAX_PATHS || self.nodes >= MAX_PATH_NODES {
                break;
            }
            self.visited.insert(next.clone());
            path.push(next.clone());
            self.dfs(&next, path);
            path.pop();
            self.visited.remove(&next);
        }
    }
}

fn path_distance(data: &StaticData, path: &[String]) -> f64 {
    path.windows(2)
        .filter_map(|pair| {
            let a = data.cities.get(&pair[0])?;
            let b = data.cities.get(&pair[1])?;
            Some(haversine_km(a.lat, a.lon, b.lat, b.lon))
        })
        .sum()
}

fn push_route(
    routes: &mut Vec<(String, String)>,
    set: &mut HashSet<(String, String)>,
    cap: usize,
    from: &str,
    to: &str,
) -> bool {
    if from == to || routes.len() >= cap {
        return false;
    }
    let key = (from.to_string(), to.to_string());
    if set.contains(&key) {
        return true;
    }
    set.insert(key.clone());
    routes.push(key);
    true
}

fn build_candidate_routes(
    data: &StaticData,
    origins: &[String],
    dests: &[String],
    max_legs: usize,
    max_routes: usize,
    extra_routes: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut routes: Vec<(String, String)> = Vec::new();
    let mut set: HashSet<(String, String)> = HashSet::new();
    let dest_set: HashSet<String> = dests.iter().cloned().collect();
    let origin_set: HashSet<String> = origins.iter().cloned().collect();

    for origin in origins {
        for dest in dests {
            push_route(&mut routes, &mut set, max_routes, origin, dest);
        }
    }

    let db_cap = ((max_routes / 2).max(routes.len() + 10)).min(max_routes);
    for (from, to) in extra_routes {
        if routes.len() >= db_cap {
            break;
        }
        push_route(&mut routes, &mut set, max_routes, from, to);
    }

    let min_to_dest = StaticData::bfs_dist(dests, &data.topology.radj, max_legs);
    let min_from_origin = StaticData::bfs_dist(origins, &data.topology.adj, max_legs);

    let mut generator = PathGen {
        data,
        adj: &data.topology.adj,
        min_to_dest: &min_to_dest,
        dest_set: &dest_set,
        max_legs,
        paths: Vec::new(),
        visited: HashSet::new(),
        nodes: 0,
    };
    for origin in origins {
        if dest_set.contains(origin) {
            continue;
        }
        generator.visited.insert(origin.clone());
        let mut path = vec![origin.clone()];
        generator.dfs(origin, &mut path);
        generator.visited.remove(origin);
    }
    let mut paths = generator.paths;
    paths.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| {
                path_distance(data, a)
                    .partial_cmp(&path_distance(data, b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    paths.dedup();

    let path_cap = ((max_routes * 7) / 10).max(routes.len() + 15).min(max_routes);
    for path in &paths {
        if routes.len() >= path_cap {
            break;
        }
        let edges: Vec<(String, String)> = path
            .windows(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();
        let new_edges = edges
            .iter()
            .filter(|edge| !set.contains(*edge))
            .count();
        if routes.len() + new_edges > max_routes {
            continue;
        }
        for (from, to) in edges {
            push_route(&mut routes, &mut set, max_routes, &from, &to);
        }
    }

    let relevant_hubs: Vec<String> = data
        .hubs
        .iter()
        .filter(|hub| {
            let from = min_from_origin
                .get(*hub)
                .copied()
                .unwrap_or(usize::MAX / 4);
            let to = min_to_dest.get(*hub).copied().unwrap_or(usize::MAX / 4);
            from + to <= max_legs
        })
        .take(14)
        .cloned()
        .collect();

    if max_legs >= 2 {
        for origin in origins {
            for hub in &relevant_hubs {
                if data.topology.has_edge(origin, hub) {
                    push_route(&mut routes, &mut set, max_routes, origin, hub);
                }
            }
        }
        for dest in dests {
            for hub in &relevant_hubs {
                if data.topology.has_edge(hub, dest) {
                    push_route(&mut routes, &mut set, max_routes, hub, dest);
                }
            }
        }
    }

    if max_legs >= 3 {
        for first in &relevant_hubs {
            for second in &relevant_hubs {
                if first != second && data.topology.has_edge(first, second) {
                    push_route(&mut routes, &mut set, max_routes, first, second);
                }
            }
        }
    }

    if max_legs >= 4 {
        let small: Vec<String> = relevant_hubs.iter().take(6).cloned().collect();
        for first in &small {
            for second in &small {
                for third in &small {
                    if first == second || second == third || first == third {
                        continue;
                    }
                    if data.topology.has_edge(first, second)
                        && data.topology.has_edge(second, third)
                    {
                        push_route(&mut routes, &mut set, max_routes, first, second);
                        push_route(&mut routes, &mut set, max_routes, second, third);
                    }
                }
            }
        }
    }

    for path in &paths {
        if routes.len() >= max_routes {
            break;
        }
        let edges: Vec<(String, String)> = path
            .windows(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();
        let new_edges = edges
            .iter()
            .filter(|edge| !set.contains(*edge))
            .count();
        if routes.len() + new_edges > max_routes {
            continue;
        }
        for (from, to) in edges {
            push_route(&mut routes, &mut set, max_routes, &from, &to);
        }
    }

    if routes.len() >= max_routes {
        warnings.push(format!(
            "Список проверяемых маршрутов ограничен {max_routes}; часть вариантов могла быть отброшена."
        ));
    }
    if data.topology.adj.is_empty() {
        warnings.push(
            "Топология OpenFlights недоступна, поэтому проверяются только прямые маршруты."
                .to_string(),
        );
    }
    if !origin_set.is_empty() {
        let missing = origins
            .iter()
            .filter(|origin| !data.topology.adj.contains_key(*origin))
            .count();
        if missing == origins.len() {
            warnings.push(
                "Для выбранных точек отправления нет данных о маршрутах в OpenFlights."
                    .to_string(),
            );
        }
    }
    if routes.is_empty() {
        warnings.push("Не удалось построить список маршрутов-кандидатов.".to_string());
    }

    (routes, warnings)
}

fn resolve_excluded_countries(
    data: &StaticData,
    values: &[String],
) -> Result<HashSet<String>, String> {
    let mut countries = HashSet::new();
    for value in values {
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let country = data
                .resolve_country_code(part)
                .ok_or_else(|| format!("Страна не найдена: {part}"))?;
            countries.insert(country);
        }
    }
    Ok(countries)
}

fn country_of_airport(data: &StaticData, airport: &str) -> Option<String> {
    data.airport_by_code
        .get(airport)
        .map(|value| value.country_code.clone())
}

fn filter_airport_codes(
    data: &StaticData,
    codes: Vec<String>,
    excluded: &HashSet<String>,
) -> Vec<String> {
    if excluded.is_empty() {
        return codes;
    }
    codes
        .into_iter()
        .filter(|code| {
            country_of_airport(data, code)
                .map(|country| !excluded.contains(&country))
                .unwrap_or(false)
        })
        .collect()
}

fn filter_endpoint(
    data: &StaticData,
    codes: Vec<String>,
    points: Vec<CityPoint>,
    excluded: &HashSet<String>,
) -> (Vec<String>, Vec<CityPoint>) {
    let codes = filter_airport_codes(data, codes, excluded);
    let allowed: HashSet<&str> = codes.iter().map(String::as_str).collect();
    let points = points
        .into_iter()
        .filter(|point| allowed.contains(point.code.as_str()))
        .collect();
    (codes, points)
}

async fn resolve_fixed(
    data: &StaticData,
    endpoint: &Endpoint,
) -> Result<(Vec<String>, Vec<CityPoint>), String> {
    match endpoint {
        Endpoint::City { code } => {
            let codes = data
                .resolve_airport_codes(code)
                .ok_or_else(|| format!("Аэропорт или город не найден: {code}"))?;
            let points: Vec<CityPoint> = codes
                .iter()
                .filter_map(|airport| data.airport_point(airport))
                .collect();
            if points.is_empty() {
                return Err(format!("Не найдены координаты для {code}"));
            }
            Ok((codes, points))
        }
        Endpoint::Region {
            lat,
            lon,
            radius_km,
            limit,
        } => {
            let points =
                data.airports_in_radius(*lat, *lon, *radius_km, limit.unwrap_or(100).clamp(1, 400));
            if points.is_empty() {
                return Err("В выбранном радиусе не найдено аэропортов.".to_string());
            }
            let codes = points.iter().map(|point| point.code.clone()).collect();
            Ok((codes, points))
        }
        Endpoint::Country { code, codes, limit } => {
            let mut inputs: Vec<String> = codes.clone();
            if let Some(code) = code {
                inputs.push(code.clone());
            }
            let mut countries: Vec<String> = Vec::new();
            for input in inputs {
                for part in input.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let country = data
                        .resolve_country_code(part)
                        .ok_or_else(|| format!("Страна не найдена: {part}"))?;
                    if !countries.contains(&country) {
                        countries.push(country);
                    }
                }
            }
            if countries.is_empty() {
                return Err("Укажите хотя бы одну страну.".to_string());
            }
            let mut points: Vec<CityPoint> = Vec::new();
            let mut seen = HashSet::new();
            for country in &countries {
                for point in data.airports_in_country(country, 400) {
                    if seen.insert(point.code.clone()) {
                        points.push(point);
                    }
                }
            }
            points.sort_by(|a, b| {
                let da = data
                    .airport_by_code
                    .get(&a.code)
                    .map(|airport| data.degree(&airport.city_code))
                    .unwrap_or(0);
                let db = data
                    .airport_by_code
                    .get(&b.code)
                    .map(|airport| data.degree(&airport.city_code))
                    .unwrap_or(0);
                db.cmp(&da).then_with(|| a.code.cmp(&b.code))
            });
            points.truncate(limit.unwrap_or(200).clamp(1, 400));
            if points.is_empty() {
                return Err("В выбранных странах не найдено аэропортов.".to_string());
            }
            let codes = points.iter().map(|point| point.code.clone()).collect();
            Ok((codes, points))
        }
        Endpoint::Any => Err("Внутренняя ошибка: Any передан в resolve_fixed".to_string()),
    }
}

async fn resolve_endpoints(
    data: &StaticData,
    api: &Travelpayouts,
    store: &Store,
    graph: Option<&Graph>,
    request: &SearchRequest,
    currency: &str,
    exclude_origins: &HashSet<String>,
    exclude_dests: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<(Vec<String>, Vec<CityPoint>, Vec<String>, Vec<CityPoint>), String> {
    let origin_any = matches!(request.origin, Endpoint::Any);
    let dest_any = matches!(request.destination, Endpoint::Any);
    if !origin_any && !dest_any {
        let (origins, origin_points) = resolve_fixed(data, &request.origin).await?;
        let (origins, origin_points) =
            filter_endpoint(data, origins, origin_points, exclude_origins);
        let (dests, dest_points) = resolve_fixed(data, &request.destination).await?;
        let (dests, dest_points) = filter_endpoint(data, dests, dest_points, exclude_dests);
        if origins.is_empty() {
            return Err("Все аэропорты отправления исключены.".to_string());
        }
        if dests.is_empty() {
            return Err("Все аэропорты назначения исключены.".to_string());
        }
        return Ok((origins, origin_points, dests, dest_points));
    }

    if dest_any {
        let (origins, origin_points) = resolve_fixed(data, &request.origin).await?;
        let (origins, origin_points) =
            filter_endpoint(data, origins, origin_points, exclude_origins);
        if origins.is_empty() {
            return Err("Все аэропорты отправления исключены.".to_string());
        }
        let mut candidates: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut directions_failed = false;
        if let Some(graph) = graph {
            for origin in &origins {
                if let Some(destinations) = graph.destinations_by_origin.get(origin) {
                    for code in destinations {
                        if code != origin && seen.insert(code.clone()) {
                            candidates.push(code.clone());
                        }
                    }
                }
            }
            warnings.push(
                "Режим «куда угодно» проверяет все направления из RAM-графа (в пределах технического лимита)."
                    .to_string(),
            );
        } else {
            for origin in &origins {
                match api.get_city_directions(origin, currency).await {
                    Ok(cities) => {
                        for city in cities.into_iter().take(80) {
                            for airport in data.airports_for_city(&city) {
                                if airport != *origin && seen.insert(airport.clone()) {
                                    candidates.push(airport);
                                }
                            }
                        }
                    }
                    Err(_) => directions_failed = true,
                }
                if let Some(origin_airport) = data.airport_by_code.get(origin) {
                    if let Some(neighbors) = data.topology.adj.get(&origin_airport.city_code) {
                        for city in neighbors {
                            for airport in data.airports_for_city(city) {
                                if airport != *origin && seen.insert(airport.clone()) {
                                    candidates.push(airport);
                                }
                            }
                        }
                    }
                }
            }
            let origin_cities: Vec<String> = origins
                .iter()
                .filter_map(|airport| {
                    data.airport_by_code
                        .get(airport)
                        .map(|value| value.city_code.clone())
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            match store
                .destinations_from(&origin_cities, MAX_ANY_CANDIDATES)
                .await
            {
                Ok(cities) => {
                    for city in cities {
                        for airport in data.airports_for_city(&city) {
                            if seen.insert(airport.clone()) {
                                candidates.push(airport);
                            }
                        }
                    }
                }
                Err(error) => warnings.push(format!(
                    "Не удалось прочитать направления из БД: {error}"
                )),
            }
            warnings.push(
                "Режим «куда угодно» проверяет направления из БД и OpenFlights (в пределах технического лимита)."
                    .to_string(),
            );
        }
        if candidates.is_empty() {
            for hub in data.hubs.iter().take(50) {
                for airport in data.airports_for_city(hub) {
                    if seen.insert(airport.clone()) {
                        candidates.push(airport);
                    }
                }
            }
            warnings.push(
                "Для режима «куда угодно» не найдено направлений; проверяются крупные хабы."
                    .to_string(),
            );
        }
        candidates.retain(|code| {
            data.airport_by_code
                .get(code)
                .map(|airport| airport.iata_type == "airport")
                .unwrap_or(false)
        });
        candidates = filter_airport_codes(data, candidates, exclude_dests);
        candidates.truncate(MAX_ANY_CANDIDATES);
        let dest_points: Vec<CityPoint> = candidates
            .iter()
            .take(MAX_ANY_POINTS)
            .filter_map(|code| data.airport_point(code))
            .collect();
        if dest_points.is_empty() {
            return Err(
                "Все аэропорты назначения исключены или направления не найдены.".to_string(),
            );
        }
        if directions_failed {
            warnings.push(
                "Часть запросов city-directions не удалась; список направлений может быть неполным."
                    .to_string(),
            );
        }
        return Ok((origins, origin_points, candidates, dest_points));
    }

    let (dests, dest_points) = resolve_fixed(data, &request.destination).await?;
    let (dests, dest_points) = filter_endpoint(data, dests, dest_points, exclude_dests);
    if dests.is_empty() {
        return Err("Все аэропорты назначения исключены.".to_string());
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    if let Some(graph) = graph {
        for dest in &dests {
            if let Some(origins) = graph.origins_by_destination.get(dest) {
                for code in origins {
                    if code != dest && seen.insert(code.clone()) {
                        candidates.push(code.clone());
                    }
                }
            }
        }
        warnings.push(
            "Режим «откуда угодно» проверяет все аэропорты из RAM-графа, откуда есть рейсы в точку назначения (в пределах технического лимита)."
                .to_string(),
        );
    } else {
        let dest_cities: Vec<String> = dests
            .iter()
            .filter_map(|airport| {
                data.airport_by_code
                    .get(airport)
                    .map(|value| value.city_code.clone())
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for dest in &dests {
            if let Some(dest_airport) = data.airport_by_code.get(dest) {
                if let Some(predecessors) = data.topology.radj.get(&dest_airport.city_code) {
                    for city in predecessors {
                        for airport in data.airports_for_city(city) {
                            if airport != *dest && seen.insert(airport.clone()) {
                                candidates.push(airport);
                            }
                        }
                    }
                }
            }
        }
        match store.origins_to(&dest_cities, MAX_ANY_CANDIDATES).await {
            Ok(cities) => {
                for city in cities {
                    for airport in data.airports_for_city(&city) {
                        if seen.insert(airport.clone()) {
                            candidates.push(airport);
                        }
                    }
                }
            }
            Err(error) => warnings.push(format!(
                "Не удалось прочитать аэропорты отправления из БД: {error}"
            )),
        }
        warnings.push(
            "Режим «откуда угодно» проверяет аэропорты из БД и OpenFlights, откуда есть рейсы в точку назначения (в пределах технического лимита)."
                .to_string(),
        );
    }
    if candidates.is_empty() {
        for hub in data.hubs.iter().take(50) {
            for airport in data.airports_for_city(hub) {
                if seen.insert(airport.clone()) {
                    candidates.push(airport);
                }
            }
        }
        warnings.push(
            "Для режима «откуда угодно» не найдено обратных маршрутов; проверяются крупные хабы."
                .to_string(),
        );
    }
    candidates.retain(|code| {
        data.airport_by_code
            .get(code)
            .map(|airport| airport.iata_type == "airport")
            .unwrap_or(false)
    });
    candidates = filter_airport_codes(data, candidates, exclude_origins);
    candidates.truncate(MAX_ANY_CANDIDATES);
    let origin_points: Vec<CityPoint> = candidates
        .iter()
        .take(MAX_ANY_POINTS)
        .filter_map(|code| data.airport_point(code))
        .collect();
    if origin_points.is_empty() {
        return Err(
            "Все аэропорты отправления исключены или не найдены.".to_string(),
        );
    }
    Ok((candidates, origin_points, dests, dest_points))
}

fn label_timestamp(label: &Label) -> i64 {
    label
        .arrive_at
        .map(|value| value.timestamp())
        .unwrap_or(i64::MIN)
}

fn dominates(a: &Label, b: &Label) -> bool {
    let at = label_timestamp(a);
    let bt = label_timestamp(b);
    let visited_subset = a
        .visited
        .iter()
        .all(|city| b.visited.iter().any(|other| other == city));
    at <= bt
        && a.price <= b.price
        && a.travel_min <= b.travel_min
        && a.transfers <= b.transfers
        && visited_subset
        && (at < bt
            || a.price < b.price
            || a.travel_min < b.travel_min
            || a.transfers < b.transfers)
}

fn same_metrics(a: &Label, b: &Label) -> bool {
    label_timestamp(a) == label_timestamp(b)
        && (a.price - b.price).abs() < 0.001
        && a.travel_min == b.travel_min
        && a.transfers == b.transfers
}

fn merge_labels(map: HashMap<String, Vec<Label>>) -> Vec<Label> {
    let mut output = Vec::new();
    for (_, mut labels) in map {
        labels.sort_by(|a, b| {
            label_timestamp(a)
                .cmp(&label_timestamp(b))
                .then_with(|| {
                    a.price
                        .partial_cmp(&b.price)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.travel_min.cmp(&b.travel_min))
                .then_with(|| a.transfers.cmp(&b.transfers))
        });
        let mut kept: Vec<Label> = Vec::new();
        for label in labels {
            if kept
                .iter()
                .any(|other| dominates(other, &label) || same_metrics(other, &label))
            {
                continue;
            }
            kept.retain(|other| !dominates(&label, other));
            kept.push(label);
            if kept.len() >= MAX_LABELS_PER_CITY {
                break;
            }
        }
        output.extend(kept);
        if output.len() >= MAX_LABELS_TOTAL {
            break;
        }
    }
    output
}

fn pareto_search(
    offers: &[Offer],
    outgoing: &HashMap<String, Vec<usize>>,
    origins: &[String],
    dests: Option<&[String]>,
    constraints: &Constraints,
    direct_bounds: &HashMap<NaiveDate, f64>,
) -> Vec<PathResult> {
    let dest_set: Option<HashSet<&str>> =
        dests.map(|values| values.iter().map(String::as_str).collect());
    let mut frontier: Vec<Label> = origins
        .iter()
        .map(|city| Label {
            city: city.clone(),
            arrive_at: None,
            arrive_local_date: None,
            price: 0.0,
            travel_min: 0,
            transfers: 0,
            legs: 0,
            visited: vec![city.clone()],
            offer_ids: Vec::new(),
        })
        .collect();
    let mut results: Vec<PathResult> = Vec::new();
    let mut expansions: usize = 0;
    let min_connection = Duration::minutes(constraints.min_connection_minutes as i64);
    let max_slice_gap =
        Duration::days(constraints.max_connection_days as i64 + 2) + min_connection;

    for _ in 0..constraints.max_legs {
        let mut next: HashMap<String, Vec<Label>> = HashMap::new();
        for label in &frontier {
            let Some(ids) = outgoing.get(&label.city) else {
                continue;
            };
            let slice: &[usize] = if label.legs == 0 {
                ids
            } else {
                let Some(previous_arrival) = label.arrive_at else {
                    continue;
                };
                let earliest = previous_arrival + min_connection;
                let latest = previous_arrival + max_slice_gap;
                let start = ids.partition_point(|id| offers[*id].depart_at < earliest);
                let end = ids.partition_point(|id| offers[*id].depart_at <= latest);
                if start >= end {
                    continue;
                }
                &ids[start..end]
            };
            let slice = if slice.len() > MAX_OFFERS_PER_LABEL {
                &slice[..MAX_OFFERS_PER_LABEL]
            } else {
                slice
            };
            for &offer_id in slice {
                expansions += 1;
                if expansions > MAX_EXPANSIONS {
                    return results;
                }
                let offer = &offers[offer_id];
                if offer.destination_airport == label.city
                    || label
                        .visited
                        .iter()
                        .any(|city| city == &offer.destination_airport)
                {
                    continue;
                }
                if label.legs == 0 {
                    if offer.depart_local_date < constraints.depart_from
                        || offer.depart_local_date > constraints.depart_to
                    {
                        continue;
                    }
                } else {
                    let Some(previous_arrival) = label.arrive_at else {
                        continue;
                    };
                    let earliest = previous_arrival + min_connection;
                    if offer.depart_at < earliest {
                        continue;
                    }
                    if let Some(previous_local_date) = label.arrive_local_date {
                        let day_gap =
                            (offer.depart_local_date - previous_local_date).num_days();
                        if day_gap > constraints.max_connection_days as i64 {
                            continue;
                        }
                    }
                }
                if offer.depart_local_date > constraints.arrive_to
                    || offer.arrive_local_date > constraints.arrive_to
                {
                    continue;
                }
                let added_transfers = offer.transfers + if label.legs > 0 { 1 } else { 0 };
                let new_transfers = label.transfers.saturating_add(added_transfers);
                if new_transfers > constraints.max_transfers {
                    continue;
                }
                let gap = label
                    .arrive_at
                    .map(|previous| (offer.depart_at - previous).num_minutes().max(0))
                    .unwrap_or(0);
                let new_travel = label.travel_min + offer.duration_min + gap;
                let new_price = label.price + offer.price;
                let new_legs = label.legs + 1;
                let mut offer_ids = label.offer_ids.clone();
                offer_ids.push(offer_id);

                let first_date = label
                    .offer_ids
                    .first()
                    .map(|&id| offers[id].depart_local_date)
                    .unwrap_or(offer.depart_local_date);
                let date_bound = direct_bounds.get(&first_date).copied();
                let is_destination = match &dest_set {
                    Some(set) => set.contains(offer.destination_airport.as_str()),
                    None => true,
                };
                if is_destination {
                    let beats_bound = new_legs == 1
                        || date_bound
                            .map(|bound| new_price < bound)
                            .unwrap_or(true);
                    if offer.arrive_local_date >= constraints.arrive_from
                        && beats_bound
                        && results.len() < 150_000
                    {
                        results.push(PathResult {
                            offer_ids: offer_ids.clone(),
                            price: new_price,
                            travel_min: new_travel,
                            transfers: new_transfers,
                            depart_at: offers[offer_ids[0]].depart_at,
                            arrive_at: offer.arrive_at,
                        });
                    }
                    if dest_set.is_some() {
                        continue;
                    }
                }

                if new_legs >= constraints.max_legs {
                    continue;
                }
                if new_transfers >= constraints.max_transfers {
                    continue;
                }
                if date_bound
                    .map(|bound| new_price >= bound)
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut visited = label.visited.clone();
                visited.push(offer.destination_airport.clone());
                let next_label = Label {
                    city: offer.destination_airport.clone(),
                    arrive_at: Some(offer.arrive_at),
                    arrive_local_date: Some(offer.arrive_local_date),
                    price: new_price,
                    travel_min: new_travel,
                    transfers: new_transfers,
                    legs: new_legs,
                    visited,
                    offer_ids,
                };
                next.entry(next_label.city.clone())
                    .or_default()
                    .push(next_label);
            }
        }
        frontier = merge_labels(next);
        if frontier.len() > MAX_FRONTIER_LABELS {
            frontier.sort_by(|a, b| {
                a.price
                    .partial_cmp(&b.price)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.travel_min.cmp(&b.travel_min))
                    .then_with(|| label_timestamp(a).cmp(&label_timestamp(b)))
                    .then_with(|| a.transfers.cmp(&b.transfers))
            });
            frontier.truncate(MAX_FRONTIER_LABELS);
        }
        if frontier.is_empty() {
            break;
        }
    }
    results
}

fn direct_bounds_from_graph(
    graph: &Graph,
    origins: &[String],
    dests: &[String],
    constraints: &Constraints,
    margin: f64,
) -> HashMap<NaiveDate, f64> {
    let mut bounds: HashMap<NaiveDate, f64> = HashMap::new();
    for origin in origins {
        for dest in dests {
            if let Some(ids) = graph.by_route.get(&(origin.clone(), dest.clone())) {
                for &id in ids {
                    let offer = &graph.offers[id];
                    if offer.transfers != 0 {
                        continue;
                    }
                    if offer.depart_local_date < constraints.depart_from
                        || offer.depart_local_date > constraints.depart_to
                        || offer.arrive_local_date < constraints.arrive_from
                        || offer.arrive_local_date > constraints.arrive_to
                    {
                        continue;
                    }
                    let price = (offer.price * margin * 100.0).round() / 100.0;
                    bounds
                        .entry(offer.depart_local_date)
                        .and_modify(|current| {
                            if price < *current {
                                *current = price;
                            }
                        })
                        .or_insert(price);
                }
            }
        }
    }
    bounds
}

fn direct_bounds_from_offers(
    offers: &[Offer],
    origins: &[String],
    dests: &[String],
    constraints: &Constraints,
    margin: f64,
) -> HashMap<NaiveDate, f64> {
    let origin_set: HashSet<&str> = origins.iter().map(String::as_str).collect();
    let dest_set: HashSet<&str> = dests.iter().map(String::as_str).collect();
    let mut bounds: HashMap<NaiveDate, f64> = HashMap::new();
    for offer in offers {
        if offer.transfers != 0
            || !origin_set.contains(offer.origin_airport.as_str())
            || !dest_set.contains(offer.destination_airport.as_str())
        {
            continue;
        }
        if offer.depart_local_date < constraints.depart_from
            || offer.depart_local_date > constraints.depart_to
            || offer.arrive_local_date < constraints.arrive_from
            || offer.arrive_local_date > constraints.arrive_to
        {
            continue;
        }
        let price = (offer.price * margin * 100.0).round() / 100.0;
        bounds
            .entry(offer.depart_local_date)
            .and_modify(|current| {
                if price < *current {
                    *current = price;
                }
            })
            .or_insert(price);
    }
    bounds
}

fn coords_for(data: &StaticData, airport: &str, city: &str) -> (f64, f64) {
    data.airport_coords(airport)
        .or_else(|| data.cities.get(city).map(|value| (value.lat, value.lon)))
        .unwrap_or((0.0, 0.0))
}

fn format_local(utc: DateTime<Utc>, tz: Option<Tz>, fallback: FixedOffset) -> String {
    match tz {
        Some(tz) => utc.with_timezone(&tz).to_rfc3339(),
        None => utc.with_timezone(&fallback).to_rfc3339(),
    }
}

fn build_response(
    data: &StaticData,
    offers: &[Offer],
    results: Vec<PathResult>,
    currency: &str,
    optimize: &str,
    max_results: usize,
) -> (Vec<ItineraryResponse>, ItineraryGroups) {
    if results.is_empty() {
        return (Vec::new(), ItineraryGroups::default());
    }
    let min_price = results
        .iter()
        .map(|item| item.price)
        .fold(f64::INFINITY, f64::min)
        .max(1.0);
    let min_travel = results
        .iter()
        .map(|item| item.travel_min)
        .min()
        .unwrap_or(1)
        .max(1) as f64;
    let min_depart = results
        .iter()
        .map(|item| item.depart_at.timestamp())
        .min()
        .unwrap_or(0);

    let date_score = |item: &PathResult| -> f64 {
        let normalized_price = item.price / min_price;
        let normalized_travel = item.travel_min as f64 / min_travel;
        let day_offset = (item.depart_at.timestamp() - min_depart).max(0) as f64 / 86_400.0;
        day_offset
            + normalized_price * 0.15
            + normalized_travel * 0.10
            + item.transfers as f64 * 0.02
    };
    let score_of = |item: &PathResult| -> f64 {
        let normalized_price = item.price / min_price;
        let normalized_travel = item.travel_min as f64 / min_travel;
        let transfers = item.transfers as f64;
        match optimize {
            "price" => normalized_price + normalized_travel * 0.03 + transfers * 0.02,
            "duration" => normalized_travel + normalized_price * 0.03 + transfers * 0.02,
            "transfers" => transfers + normalized_price * 0.10 + normalized_travel * 0.02,
            "dates" => date_score(item),
            _ => normalized_price + normalized_travel + transfers * 0.15,
        }
    };

    let indices: Vec<usize> = (0..results.len()).collect();

    let mut price_order = indices.clone();
    price_order.sort_by(|&a, &b| {
        results[a]
            .price
            .partial_cmp(&results[b].price)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| results[a].transfers.cmp(&results[b].transfers))
            .then_with(|| results[a].travel_min.cmp(&results[b].travel_min))
            .then_with(|| results[a].depart_at.cmp(&results[b].depart_at))
    });

    let mut transfer_order = indices.clone();
    transfer_order.sort_by(|&a, &b| {
        results[a]
            .transfers
            .cmp(&results[b].transfers)
            .then_with(|| {
                results[a]
                    .price
                    .partial_cmp(&results[b].price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| results[a].travel_min.cmp(&results[b].travel_min))
            .then_with(|| results[a].depart_at.cmp(&results[b].depart_at))
    });

    let mut date_order = indices.clone();
    date_order.sort_by(|&a, &b| {
        date_score(&results[a])
            .partial_cmp(&date_score(&results[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| results[a].depart_at.cmp(&results[b].depart_at))
            .then_with(|| {
                results[a]
                    .price
                    .partial_cmp(&results[b].price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| results[a].transfers.cmp(&results[b].transfers))
    });

    let mut duration_order = indices.clone();
    duration_order.sort_by(|&a, &b| {
        results[a]
            .travel_min
            .cmp(&results[b].travel_min)
            .then_with(|| {
                results[a]
                    .price
                    .partial_cmp(&results[b].price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| results[a].transfers.cmp(&results[b].transfers))
            .then_with(|| results[a].depart_at.cmp(&results[b].depart_at))
    });

    let mut overall_order = indices.clone();
    overall_order.sort_by(|&a, &b| {
        score_of(&results[a])
            .partial_cmp(&score_of(&results[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                results[a]
                    .price
                    .partial_cmp(&results[b].price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| results[a].travel_min.cmp(&results[b].travel_min))
            .then_with(|| results[a].transfers.cmp(&results[b].transfers))
    });

    let dest_by_index: Vec<String> = results
        .iter()
        .map(|result| {
            result
                .offer_ids
                .last()
                .map(|&id| offers[id].destination_airport.clone())
                .unwrap_or_default()
        })
        .collect();

    let mut best_price_by_dest: HashMap<String, f64> = HashMap::new();
    for (index, destination) in dest_by_index.iter().enumerate() {
        best_price_by_dest
            .entry(destination.clone())
            .and_modify(|price| {
                if results[index].price < *price {
                    *price = results[index].price;
                }
            })
            .or_insert(results[index].price);
    }
    let mut destinations: Vec<(String, f64)> = best_price_by_dest.into_iter().collect();
    destinations.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    destinations.truncate(60);
    let allowed: HashSet<String> = destinations.into_iter().map(|(code, _)| code).collect();
    let per_dest = if allowed.len() <= 2 {
        20
    } else {
        (max_results / 10).clamp(2, 6)
    };

    let mut selected: BTreeSet<usize> = BTreeSet::new();
    for order in [
        &price_order,
        &transfer_order,
        &date_order,
        &duration_order,
        &overall_order,
    ] {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for &index in order {
            let destination = dest_by_index[index].as_str();
            if !allowed.contains(destination) {
                continue;
            }
            let count = counts.entry(destination).or_insert(0);
            if *count < per_dest {
                selected.insert(index);
                *count += 1;
            }
        }
    }

    let mut selected_order: Vec<usize> = selected.into_iter().collect();
    selected_order.sort_by(|&a, &b| {
        score_of(&results[a])
            .partial_cmp(&score_of(&results[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                results[a]
                    .price
                    .partial_cmp(&results[b].price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| results[a].travel_min.cmp(&results[b].travel_min))
            .then_with(|| results[a].transfers.cmp(&results[b].transfers))
    });
    if selected_order.len() > 1000 {
        selected_order.truncate(1000);
    }

    let mut mapping: HashMap<usize, usize> = HashMap::new();
    for (new_index, &old_index) in selected_order.iter().enumerate() {
        mapping.insert(old_index, new_index);
    }

    let group_of = |order: &[usize]| -> Vec<usize> {
        order
            .iter()
            .filter_map(|index| mapping.get(index).copied())
            .collect()
    };
    let groups = ItineraryGroups {
        price: group_of(&price_order),
        transfers: group_of(&transfer_order),
        dates: group_of(&date_order),
        duration: group_of(&duration_order),
    };

    let itineraries = selected_order
        .into_iter()
        .map(|old_index| {
            let result = &results[old_index];
            let score = score_of(result);
            let legs: Vec<LegResponse> = result
                .offer_ids
                .iter()
                .map(|&id| {
                    let offer = &offers[id];
                    let (origin_lat, origin_lon) =
                        coords_for(data, &offer.origin_airport, &offer.origin);
                    let (destination_lat, destination_lon) = coords_for(
                        data,
                        &offer.destination_airport,
                        &offer.destination,
                    );
                    LegResponse {
                        origin: offer.origin.clone(),
                        origin_name: data.city_name(&offer.origin),
                        destination: offer.destination.clone(),
                        destination_name: data.city_name(&offer.destination),
                        origin_airport: offer.origin_airport.clone(),
                        destination_airport: offer.destination_airport.clone(),
                        origin_lat,
                        origin_lon,
                        destination_lat,
                        destination_lon,
                        depart_at: format_local(
                            offer.depart_at,
                            offer.origin_tz,
                            offer.fallback_offset,
                        ),
                        arrive_at: format_local(
                            offer.arrive_at,
                            offer.destination_tz,
                            offer.fallback_offset,
                        ),
                        depart_tz: offer.origin_tz.map(|tz| tz.name().to_string()),
                        arrive_tz: offer.destination_tz.map(|tz| tz.name().to_string()),
                        price: offer.price,
                        duration_min: offer.duration_min,
                        transfers: offer.transfers,
                        airline: offer.airline.clone(),
                        flight_number: offer.flight_number.clone(),
                        link: offer.link.clone(),
                    }
                })
                .collect();
            let first_offer = &offers[result.offer_ids[0]];
            let last_offer = &offers[*result.offer_ids.last().unwrap()];
            ItineraryResponse {
                total_price: (result.price * 100.0).round() / 100.0,
                currency: currency.to_string(),
                total_transfers: result.transfers,
                travel_minutes: result.travel_min,
                elapsed_minutes: (result.arrive_at - result.depart_at).num_minutes(),
                depart_at: format_local(
                    result.depart_at,
                    first_offer.origin_tz,
                    first_offer.fallback_offset,
                ),
                arrive_at: format_local(
                    result.arrive_at,
                    last_offer.destination_tz,
                    last_offer.fallback_offset,
                ),
                depart_tz: first_offer.origin_tz.map(|tz| tz.name().to_string()),
                arrive_tz: last_offer.destination_tz.map(|tz| tz.name().to_string()),
                score: (score * 1000.0).round() / 1000.0,
                legs,
            }
        })
        .collect();

    (itineraries, groups)
}

pub async fn run_search(
    data: Arc<StaticData>,
    api: Arc<Travelpayouts>,
    store: Store,
    graph_cache: Arc<GraphCache>,
    request: SearchRequest,
) -> Result<SearchResponse, String> {
    let started = Instant::now();
    let mut warnings = Vec::new();
    if let Some(warning) = &data.topology_warning {
        warnings.push(warning.clone());
    }

    let currency = request
        .currency
        .clone()
        .unwrap_or_else(|| "rub".to_string())
        .to_lowercase();
    let currency = match currency.as_str() {
        "rub" | "usd" | "eur" | "gbp" | "kzt" => currency,
        _ => "rub".to_string(),
    };

    let max_transfers = request.max_transfers.unwrap_or(2).min(5) as u32;
    let max_legs = max_transfers as usize + 1;
    let max_connection_days = request.max_connection_days.unwrap_or(2).min(30);
    let min_connection_minutes = request.min_connection_minutes.unwrap_or(60).min(1440);
    let optimize = request
        .optimize
        .clone()
        .unwrap_or_else(|| "balanced".to_string())
        .to_lowercase();
    let optimize = match optimize.as_str() {
        "price" | "duration" | "transfers" | "dates" | "balanced" => optimize,
        _ => "balanced".to_string(),
    };
    let max_results = request.max_results.unwrap_or(30).clamp(1, 50);
    let max_routes = request.max_routes.unwrap_or(260).clamp(20, 600);
    let dest_any = matches!(request.destination, Endpoint::Any);
    let origin_any = matches!(request.origin, Endpoint::Any);
    let direct_margin = std::env::var("DIRECT_PRICE_MARGIN")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value >= 0.5 && *value <= 5.0)
        .unwrap_or(1.0);

    let today = Utc::now().date_naive();
    let depart_from = request.depart_from.unwrap_or(today);
    let depart_to = request
        .depart_to
        .unwrap_or(depart_from + Duration::days(30));
    let arrive_from = request.arrive_from.unwrap_or(depart_from);
    let arrive_to = request
        .arrive_to
        .unwrap_or(depart_to.max(depart_from) + Duration::days(30));

    if depart_from > depart_to {
        return Err("Дата начала вылета позже даты окончания вылета.".to_string());
    }
    if arrive_from > arrive_to {
        return Err("Дата начала прилёта позже даты окончания прилёта.".to_string());
    }
    if arrive_to < depart_from {
        return Err("Окно прилёта заканчивается раньше начала окна вылета.".to_string());
    }
    if arrive_to > today + Duration::days(365) {
        warnings.push(
            "Travelpayouts обычно отдаёт цены примерно на год вперёд; за пределами этого окна предложений может не быть."
                .to_string(),
        );
    }

    let constraints = Constraints {
        depart_from,
        depart_to,
        arrive_from,
        arrive_to,
        max_transfers,
        max_connection_days,
        min_connection_minutes,
        max_legs,
    };

    let exclude_origins = resolve_excluded_countries(&data, &request.exclude_origin_countries)?;
    let exclude_dests =
        resolve_excluded_countries(&data, &request.exclude_destination_countries)?;

    let graph = graph_cache.snapshot().await;
    let graph_usable = graph.currency == currency && !graph.offers.is_empty();
    let both_any = origin_any && dest_any;
    if both_any {
        if !graph_usable {
            return Err(
                "Нет данных для поиска «откуда угодно» + «куда угодно». Дождитесь загрузки базы."
                    .to_string(),
            );
        }
        let mut results = Vec::new();
        for (index, offer) in graph.offers.iter().enumerate() {
            if offer.depart_local_date < constraints.depart_from
                || offer.depart_local_date > constraints.depart_to
                || offer.arrive_local_date < constraints.arrive_from
                || offer.arrive_local_date > constraints.arrive_to
            {
                continue;
            }
            if offer.transfers > constraints.max_transfers {
                continue;
            }
            if country_of_airport(&data, &offer.origin_airport)
                .map(|country| exclude_origins.contains(&country))
                .unwrap_or(false)
                || country_of_airport(&data, &offer.destination_airport)
                    .map(|country| exclude_dests.contains(&country))
                    .unwrap_or(false)
            {
                continue;
            }
            results.push(PathResult {
                offer_ids: vec![index],
                price: offer.price,
                travel_min: offer.duration_min,
                transfers: offer.transfers,
                depart_at: offer.depart_at,
                arrive_at: offer.arrive_at,
            });
        }
        let (itineraries, groups) = build_response(
            &data,
            &graph.offers,
            results,
            &currency,
            &optimize,
            max_results,
        );
        warnings.push(
            "Для «откуда угодно» + «куда угодно» показаны самые дешёвые прямые билеты по всей базе. Для поиска пересадок выберите город или регион."
                .to_string(),
        );
        if itineraries.is_empty() {
            warnings.push(
                "Дешёвых прямых билетов в выбранном окне дат не найдено. Расширьте даты или выберите точку."
                    .to_string(),
            );
        }
        let cache = api.stats().await;
        return Ok(SearchResponse {
            itineraries,
            groups,
            origin_cities: Vec::new(),
            destination_cities: Vec::new(),
            routes_requested: graph.route_set.len(),
            routes_failed: 0,
            offers_considered: graph.offers.len(),
            cache,
            warnings,
            source: "ram".to_string(),
            elapsed_ms: started.elapsed().as_millis(),
        });
    }
    let (mut origins, origin_points, mut dests, dest_points) = resolve_endpoints(
        &data,
        &api,
        &store,
        if graph_usable { Some(graph.as_ref()) } else { None },
        &request,
        &currency,
        &exclude_origins,
        &exclude_dests,
        &mut warnings,
    )
    .await?;

    if origins.is_empty() || (!dest_any && dests.is_empty()) {
        return Err("Не удалось определить точки отправления или назначения.".to_string());
    }

    if graph_usable {
        if dest_any {
            let mut seen: HashSet<String> = dests.iter().cloned().collect();
            'outer_dest: for origin in &origins {
                if let Some(list) = graph.destinations_by_origin.get(origin) {
                    for destination in list {
                        if seen.insert(destination.clone()) {
                            dests.push(destination.clone());
                            if dests.len() >= MAX_ANY_CANDIDATES {
                                break 'outer_dest;
                            }
                        }
                    }
                }
            }
        }
        if origin_any {
            let mut seen: HashSet<String> = origins.iter().cloned().collect();
            'outer_origin: for destination in &dests {
                if let Some(list) = graph.origins_by_destination.get(destination) {
                    for origin in list {
                        if seen.insert(origin.clone()) {
                            origins.push(origin.clone());
                            if origins.len() >= MAX_ANY_CANDIDATES {
                                break 'outer_origin;
                            }
                        }
                    }
                }
            }
        }
    }
    origins.retain(|code| {
        country_of_airport(&data, code)
            .map(|country| !exclude_origins.contains(&country))
            .unwrap_or(false)
    });
    dests.retain(|code| {
        country_of_airport(&data, code)
            .map(|country| !exclude_dests.contains(&country))
            .unwrap_or(false)
    });
    if origins.is_empty() || (!dest_any && dests.is_empty()) {
        return Err("Все аэропорты отправления или назначения исключены.".to_string());
    }

    let mut graph_results = Vec::new();
    if graph_usable {
        let destination_arg = if dest_any && exclude_dests.is_empty() {
            None
        } else {
            Some(dests.as_slice())
        };
        let direct_bounds = if dest_any || dests.len() > 1 {
            HashMap::new()
        } else {
            direct_bounds_from_graph(&graph, &origins, &dests, &constraints, direct_margin)
        };
        graph_results = pareto_search(
            &graph.offers,
            &graph.outgoing,
            &origins,
            destination_arg,
            &constraints,
            &direct_bounds,
        );
        graph_results.sort_by(|a, b| a.offer_ids.cmp(&b.offer_ids));
        graph_results.dedup_by(|a, b| a.offer_ids == b.offer_ids);
    }

    let (source, results, routes_requested, routes_failed, offers_considered) =
        if !graph_results.is_empty() {
            let graph_routes = graph.route_set.len();
            let graph_offers = graph.offers.len();
            (
                OfferSource::Ram(graph),
                graph_results,
                graph_routes,
                0,
                graph_offers,
            )
        } else {
            let extra_routes = match store.routes_touching(&origins, &dests, max_routes).await {
                Ok(routes) => routes,
                Err(error) => {
                    warnings.push(format!("Не удалось прочитать маршруты из БД: {error}"));
                    Vec::new()
                }
            };
            let (routes, route_warnings) = build_candidate_routes(
                &data,
                &origins,
                &dests,
                max_legs,
                max_routes,
                &extra_routes,
            );
            warnings.extend(route_warnings);
            let routes_requested = routes.len();

            let mut all_offers: Vec<Offer> = Vec::new();
            let mut outgoing: HashMap<String, Vec<usize>> = HashMap::new();
            let mut routes_failed = 0usize;
            let mut fetch_errors: Vec<String> = Vec::new();

            let mut stream =
                futures::stream::iter(routes.into_iter().map(|(origin, destination)| {
                    let api = api.clone();
                    let currency = currency.clone();
                    async move {
                        let result = api
                            .get_route_offers(&origin, &destination, &currency)
                            .await;
                        (origin, destination, result)
                    }
                }))
                .buffer_unordered(8);

            while let Some((origin, destination, result)) = stream.next().await {
                match result {
                    Ok(offers) => {
                        for offer in offers.iter() {
                            if offer.depart_local_date < depart_from
                                || offer.depart_local_date > arrive_to
                                || offer.arrive_local_date > arrive_to
                                || offer.arrive_local_date < depart_from
                            {
                                continue;
                            }
                            let index = all_offers.len();
                            all_offers.push(offer.clone());
                            outgoing.entry(offer.origin.clone()).or_default().push(index);
                        }
                    }
                    Err(error) => {
                        routes_failed += 1;
                        if fetch_errors.len() < 4 {
                            fetch_errors.push(format!("{origin}->{destination}: {error}"));
                        }
                    }
                }
            }

            for ids in outgoing.values_mut() {
                ids.sort_by(|a, b| {
                    offers_sort_key(&all_offers[*a])
                        .partial_cmp(&offers_sort_key(&all_offers[*b]))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                ids.dedup();
            }

            if routes_failed > 0 {
                warnings.push(format!(
                    "Не удалось получить цены для {routes_failed} из {routes_requested} маршрутов."
                ));
                warnings.extend(fetch_errors);
            }

            let direct_bounds = if dest_any || dests.len() > 1 {
                HashMap::new()
            } else {
                direct_bounds_from_offers(
                    &all_offers,
                    &origins,
                    &dests,
                    &constraints,
                    direct_margin,
                )
            };
            let mut results = pareto_search(
                &all_offers,
                &outgoing,
                &origins,
                Some(&dests),
                &constraints,
                &direct_bounds,
            );
            results.sort_by(|a, b| a.offer_ids.cmp(&b.offer_ids));
            results.dedup_by(|a, b| a.offer_ids == b.offer_ids);
            let offers_considered = all_offers.len();
            (
                OfferSource::Live(all_offers),
                results,
                routes_requested,
                routes_failed,
                offers_considered,
            )
        };

    let (itineraries, groups) = build_response(
        &data,
        source.offers(),
        results,
        &currency,
        &optimize,
        max_results,
    );
    if itineraries.is_empty() {
        warnings.push(
            "Маршруты не найдены. Попробуйте расширить окна дат, увеличить радиус/лимит городов, число пересадок или лимит маршрутов."
                .to_string(),
        );
    }
    let cache = api.stats().await;

    Ok(SearchResponse {
        itineraries,
        groups,
        origin_cities: origin_points,
        destination_cities: dest_points,
        routes_requested,
        routes_failed,
        offers_considered,
        cache,
        warnings,
        source: source.label().to_string(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn offers_sort_key(offer: &Offer) -> (i64, f64) {
    (offer.depart_at.timestamp(), offer.price)
}
