use crate::data::StaticData;
use crate::models::{DbStats, Offer};
use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS cities (
        code TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        name_en TEXT NOT NULL,
        country TEXT NOT NULL,
        lat REAL NOT NULL,
        lon REAL NOT NULL,
        degree INTEGER NOT NULL DEFAULT 0,
        flightable INTEGER NOT NULL DEFAULT 1,
        last_bulk_at TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_cities_bulk ON cities(last_bulk_at, degree DESC)",
    "CREATE TABLE IF NOT EXISTS routes (
        origin TEXT NOT NULL,
        destination TEXT NOT NULL,
        first_seen TEXT NOT NULL,
        last_seen TEXT NOT NULL,
        last_refreshed TEXT,
        source TEXT NOT NULL DEFAULT 'bulk',
        PRIMARY KEY (origin, destination)
    )",
    "CREATE INDEX IF NOT EXISTS idx_routes_origin ON routes(origin)",
    "CREATE INDEX IF NOT EXISTS idx_routes_destination ON routes(destination)",
    "CREATE TABLE IF NOT EXISTS offers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        origin TEXT NOT NULL,
        destination TEXT NOT NULL,
        origin_airport TEXT NOT NULL,
        destination_airport TEXT NOT NULL,
        depart_at TEXT NOT NULL,
        arrive_at TEXT NOT NULL,
        depart_local_date TEXT NOT NULL,
        arrive_local_date TEXT NOT NULL,
        origin_tz TEXT,
        destination_tz TEXT,
        fallback_offset INTEGER NOT NULL,
        price REAL NOT NULL,
        currency TEXT NOT NULL,
        transfers INTEGER NOT NULL,
        duration_min INTEGER NOT NULL,
        airline TEXT NOT NULL,
        flight_number TEXT NOT NULL,
        link TEXT NOT NULL,
        fetched_at TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_offers_route ON offers(origin, destination, currency, fetched_at)",
    "CREATE INDEX IF NOT EXISTS idx_offers_origin ON offers(origin, currency)",
    "CREATE INDEX IF NOT EXISTS idx_offers_destination ON offers(destination, currency)",
    "CREATE INDEX IF NOT EXISTS idx_offers_depart ON offers(depart_at)",
    "CREATE INDEX IF NOT EXISTS idx_offers_currency_depart ON offers(currency, depart_at)",
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS bulk_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        started_at TEXT NOT NULL,
        finished_at TEXT,
        origins_count INTEGER NOT NULL DEFAULT 0,
        routes_count INTEGER NOT NULL DEFAULT 0,
        offers_count INTEGER NOT NULL DEFAULT 0,
        errors_count INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL,
        details TEXT
    )",
];

fn now_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn placeholders(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(",")
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(30));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .with_context(|| format!("open sqlite {}", path.display()))?;
        let store = Self { pool };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        for statement in SCHEMA {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .with_context(|| format!("schema: {statement}"))?;
        }
        let removed = sqlx::query("DELETE FROM offers WHERE transfers <> 0")
            .execute(&self.pool)
            .await?;
        if removed.rows_affected() > 0 {
            sqlx::query("UPDATE routes SET last_refreshed = NULL")
                .execute(&self.pool)
                .await?;
            sqlx::query("UPDATE cities SET last_bulk_at = NULL")
                .execute(&self.pool)
                .await?;
            tracing::info!(
                removed = removed.rows_affected(),
                "removed non-direct offers; refresh state reset"
            );
        }
        Ok(())
    }

    pub async fn sync_cities(&self, data: &StaticData) -> Result<()> {
        let flightable: HashSet<String> = data
            .airports
            .iter()
            .filter(|airport| {
                airport.flightable
                    && airport.iata_type == "airport"
                    && crate::data::is_iata_code(&airport.code)
                    && !airport.city_code.is_empty()
            })
            .map(|airport| airport.city_code.clone())
            .collect();
        let mut tx = self.pool.begin().await?;
        for city in data.cities.values() {
            let degree = data.degree(&city.code) as i64;
            let is_flightable = if flightable.contains(&city.code) { 1 } else { 0 };
            sqlx::query(
                "INSERT INTO cities(code,name,name_en,country,lat,lon,degree,flightable)
                 VALUES(?,?,?,?,?,?,?,?)
                 ON CONFLICT(code) DO UPDATE SET
                    name=excluded.name,
                    name_en=excluded.name_en,
                    country=excluded.country,
                    lat=excluded.lat,
                    lon=excluded.lon,
                    degree=excluded.degree,
                    flightable=CASE WHEN cities.flightable = 1 OR excluded.flightable = 1 THEN 1 ELSE 0 END",
            )
            .bind(&city.code)
            .bind(&city.name)
            .bind(&city.name_en)
            .bind(&city.country_code)
            .bind(city.lat)
            .bind(city.lon)
            .bind(degree)
            .bind(is_flightable)
            .execute(&mut *tx)
            .await?;
        }
        let current: HashSet<String> = data.cities.keys().cloned().collect();
        let rows = sqlx::query("SELECT code FROM cities")
            .fetch_all(&mut *tx)
            .await?;
        let mut removed = 0usize;
        for row in rows {
            let code: String = row.get("code");
            if current.contains(&code) {
                continue;
            }
            sqlx::query("DELETE FROM offers WHERE origin = ? OR destination = ?")
                .bind(&code)
                .bind(&code)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM routes WHERE origin = ? OR destination = ?")
                .bind(&code)
                .bind(&code)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE cities SET flightable = 0 WHERE code = ?")
                .bind(&code)
                .execute(&mut *tx)
                .await?;
            removed += 1;
        }
        tx.commit().await?;
        if removed > 0 {
            tracing::info!(removed, "deactivated missing cities");
        }
        Ok(())
    }

    pub async fn next_origins(
        &self,
        limit: usize,
        forced: &[String],
        countries: &[String],
    ) -> Result<Vec<String>> {
        if !forced.is_empty() {
            return Ok(forced.to_vec());
        }
        let mut sql = String::from("SELECT code FROM cities WHERE 1 = 1");
        if !countries.is_empty() {
            sql.push_str(&format!(" AND country IN ({})", placeholders(countries.len())));
        }
        sql.push_str(
            " ORDER BY (last_bulk_at IS NOT NULL) ASC, last_bulk_at ASC, degree DESC LIMIT ?",
        );
        let mut query = sqlx::query(&sql);
        for country in countries {
            query = query.bind(country);
        }
        query = query.bind(limit as i64);
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| row.get::<String, _>("code"))
            .collect())
    }

    pub async fn routes_from(&self, origin: &str, limit: usize) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT destination FROM routes WHERE origin = ?
             ORDER BY (last_refreshed IS NOT NULL) ASC, last_refreshed ASC, last_seen DESC
             LIMIT ?",
        )
        .bind(origin)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| row.get::<String, _>("destination"))
            .collect())
    }

    pub async fn destinations_from(
        &self,
        origins: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        if origins.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT destination, MAX(last_seen) AS seen FROM routes
             WHERE origin IN ({}) GROUP BY destination ORDER BY seen DESC LIMIT ?",
            placeholders(origins.len())
        );
        let mut query = sqlx::query(&sql);
        for origin in origins {
            query = query.bind(origin);
        }
        query = query.bind(limit as i64);
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| row.get::<String, _>("destination"))
            .collect())
    }

    pub async fn origins_to(&self, dests: &[String], limit: usize) -> Result<Vec<String>> {
        if dests.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT origin, MAX(last_seen) AS seen FROM routes
             WHERE destination IN ({}) GROUP BY origin ORDER BY seen DESC LIMIT ?",
            placeholders(dests.len())
        );
        let mut query = sqlx::query(&sql);
        for dest in dests {
            query = query.bind(dest);
        }
        query = query.bind(limit as i64);
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| row.get::<String, _>("origin"))
            .collect())
    }

    pub async fn routes_touching(
        &self,
        origins: &[String],
        dests: &[String],
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        if origins.is_empty() && dests.is_empty() {
            return Ok(Vec::new());
        }
        let mut parts = Vec::new();
        if !origins.is_empty() {
            parts.push(format!("origin IN ({})", placeholders(origins.len())));
        }
        if !dests.is_empty() {
            parts.push(format!("destination IN ({})", placeholders(dests.len())));
        }
        let sql = format!(
            "SELECT origin, destination FROM routes WHERE {} ORDER BY last_seen DESC LIMIT ?",
            parts.join(" OR ")
        );
        let mut query = sqlx::query(&sql);
        for origin in origins {
            query = query.bind(origin);
        }
        for dest in dests {
            query = query.bind(dest);
        }
        query = query.bind(limit as i64);
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|row| {
                (
                    row.get::<String, _>("origin"),
                    row.get::<String, _>("destination"),
                )
            })
            .collect())
    }

    pub async fn fresh_offers(
        &self,
        origin: &str,
        destination: &str,
        currency: &str,
        ttl: Duration,
    ) -> Result<Option<Vec<Offer>>> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::hours(18));
        let cutoff = rfc3339(cutoff);
        let Some(row) =
            sqlx::query("SELECT last_refreshed FROM routes WHERE origin = ? AND destination = ?")
                .bind(origin)
                .bind(destination)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let last: Option<String> = row.try_get("last_refreshed")?;
        let Some(last) = last else {
            return Ok(None);
        };
        if last < cutoff {
            return Ok(None);
        }
        let rows = sqlx::query(
            "SELECT origin, destination, origin_airport, destination_airport,
                    depart_at, arrive_at, depart_local_date, arrive_local_date,
                    origin_tz, destination_tz, fallback_offset, price, transfers,
                    duration_min, airline, flight_number, link
             FROM offers
             WHERE origin = ? AND destination = ? AND currency = ?
             ORDER BY depart_at",
        )
        .bind(origin)
        .bind(destination)
        .bind(currency)
        .fetch_all(&self.pool)
        .await?;
        let mut offers = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(offer) = row_to_offer(row)? {
                offers.push(offer);
            }
        }
        Ok(Some(offers))
    }

    pub async fn replace_route_offers(
        &self,
        origin: &str,
        destination: &str,
        currency: &str,
        offers: &[Offer],
    ) -> Result<()> {
        let now = now_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM offers WHERE origin = ? AND destination = ? AND currency = ?")
            .bind(origin)
            .bind(destination)
            .bind(currency)
            .execute(&mut *tx)
            .await?;
        for offer in offers {
            insert_offer(&mut *tx, offer, currency, &now).await?;
        }
        upsert_route_success(&mut tx, origin, destination, &now).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn replace_origin_refresh(
        &self,
        origin: &str,
        currency: &str,
        successes: &[(String, Vec<Offer>)],
        discovered: &[String],
        stale_cutoff: &str,
    ) -> Result<()> {
        let now = now_string();
        let success_dests: HashSet<&str> =
            successes.iter().map(|(dest, _)| dest.as_str()).collect();
        let mut tx = self.pool.begin().await?;
        for (destination, offers) in successes {
            sqlx::query("DELETE FROM offers WHERE origin = ? AND destination = ? AND currency = ?")
                .bind(origin)
                .bind(destination)
                .bind(currency)
                .execute(&mut *tx)
                .await?;
            for offer in offers {
                insert_offer(&mut *tx, offer, currency, &now).await?;
            }
            upsert_route_success(&mut tx, origin, destination, &now).await?;
        }
        for destination in discovered {
            if success_dests.contains(destination.as_str()) {
                continue;
            }
            sqlx::query(
                "INSERT INTO routes(origin,destination,first_seen,last_seen,last_refreshed,source)
                 VALUES(?,?,?,?,NULL,'directions')
                 ON CONFLICT(origin,destination) DO UPDATE SET last_seen=excluded.last_seen",
            )
            .bind(origin)
            .bind(destination)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM offers WHERE origin = ? AND currency = ? AND fetched_at < ?")
            .bind(origin)
            .bind(currency)
            .bind(stale_cutoff)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE cities SET last_bulk_at = ? WHERE code = ?")
            .bind(&now)
            .bind(origin)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_offers_for_graph(
        &self,
        currency: &str,
        horizon_days: u32,
    ) -> Result<Vec<Offer>> {
        let from = rfc3339(Utc::now() - chrono::Duration::days(1));
        let to = rfc3339(Utc::now() + chrono::Duration::days(horizon_days as i64));
        let rows = sqlx::query(
            "SELECT origin, destination, origin_airport, destination_airport,
                    depart_at, arrive_at, depart_local_date, arrive_local_date,
                    origin_tz, destination_tz, fallback_offset, price, transfers,
                    duration_min, airline, flight_number, link
             FROM offers
             WHERE currency = ? AND depart_at >= ? AND depart_at <= ?
             ORDER BY depart_at",
        )
        .bind(currency)
        .bind(&from)
        .bind(&to)
        .fetch_all(&self.pool)
        .await?;
        let mut offers = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(offer) = row_to_offer(row)? {
                offers.push(offer);
            }
        }
        Ok(offers)
    }

    pub async fn mark_origin(&self, origin: &str) -> Result<()> {
        sqlx::query("UPDATE cities SET last_bulk_at = ? WHERE code = ?")
            .bind(now_string())
            .bind(origin)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn purge(
        &self,
        now: DateTime<Utc>,
        horizon_days: u32,
        stale_days: u32,
    ) -> Result<()> {
        let now_s = rfc3339(now);
        let horizon = rfc3339(now + chrono::Duration::days(horizon_days as i64));
        let stale = rfc3339(now - chrono::Duration::days(stale_days as i64));
        sqlx::query("DELETE FROM offers WHERE depart_at < ? OR depart_at > ?")
            .bind(&now_s)
            .bind(&horizon)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM offers WHERE fetched_at < ?")
            .bind(&stale)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn start_bulk_log(&self) -> Result<i64> {
        let now = now_string();
        sqlx::query(
            "UPDATE bulk_log SET finished_at = COALESCE(finished_at, ?), status = 'interrupted'
             WHERE status = 'running'",
        )
        .bind(&now)
        .execute(&self.pool)
        .await?;
        let result = sqlx::query("INSERT INTO bulk_log(started_at,status) VALUES(?,'running')")
            .bind(&now)
            .execute(&self.pool)
            .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn finish_bulk_log(
        &self,
        id: i64,
        origins: usize,
        routes: usize,
        offers: usize,
        errors: usize,
        status: &str,
        details: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE bulk_log SET finished_at=?, origins_count=?, routes_count=?,
             offers_count=?, errors_count=?, status=?, details=? WHERE id=?",
        )
        .bind(now_string())
        .bind(origins as i64)
        .bind(routes as i64)
        .bind(offers as i64)
        .bind(errors as i64)
        .bind(status)
        .bind(details)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let value = sqlx::query_scalar("SELECT value FROM meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(value)
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO meta(key,value) VALUES(?,?)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_discovered_routes(
        &self,
        origin: &str,
        destinations: &[String],
        source: &str,
    ) -> Result<usize> {
        let now = now_string();
        let mut tx = self.pool.begin().await?;
        let mut count = 0usize;
        for destination in destinations {
            let destination = destination.trim().to_uppercase();
            if destination.is_empty() || destination == origin {
                continue;
            }
            let result = sqlx::query(
                "INSERT INTO routes(origin,destination,first_seen,last_seen,last_refreshed,source)
                 VALUES(?,?,?,?,NULL,?)
                 ON CONFLICT(origin,destination) DO UPDATE SET last_seen=excluded.last_seen",
            )
            .bind(origin)
            .bind(&destination)
            .bind(&now)
            .bind(&now)
            .bind(source)
            .execute(&mut *tx)
            .await?;
            count += result.rows_affected() as usize;
        }
        if !destinations.is_empty() {
            sqlx::query("UPDATE cities SET flightable = 1 WHERE code = ?")
                .bind(origin)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(count)
    }

    pub async fn stats(&self) -> Result<DbStats> {
        let cities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cities")
            .fetch_one(&self.pool)
            .await?;
        let routes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routes")
            .fetch_one(&self.pool)
            .await?;
        let offers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM offers")
            .fetch_one(&self.pool)
            .await?;
        let tracked_origins: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM cities WHERE last_bulk_at IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;

        let mut stats = DbStats {
            cities,
            routes,
            offers,
            tracked_origins,
            ..DbStats::default()
        };
        if let Some(row) = sqlx::query(
            "SELECT started_at, finished_at, status, origins_count, routes_count, offers_count
             FROM bulk_log ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?
        {
            stats.last_bulk_started = row.try_get::<String, _>("started_at").ok();
            stats.last_bulk_finished = row.try_get::<Option<String>, _>("finished_at").ok().flatten();
            stats.last_bulk_status = row.try_get::<String, _>("status").ok();
            stats.last_bulk_origins = row.try_get::<i64, _>("origins_count").unwrap_or(0);
            stats.last_bulk_routes = row.try_get::<i64, _>("routes_count").unwrap_or(0);
            stats.last_bulk_offers = row.try_get::<i64, _>("offers_count").unwrap_or(0);
        }
        Ok(stats)
    }
}

async fn insert_offer(
    connection: &mut SqliteConnection,
    offer: &Offer,
    currency: &str,
    fetched_at: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO offers(
            origin, destination, origin_airport, destination_airport,
            depart_at, arrive_at, depart_local_date, arrive_local_date,
            origin_tz, destination_tz, fallback_offset, price, currency,
            transfers, duration_min, airline, flight_number, link, fetched_at
         ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&offer.origin)
    .bind(&offer.destination)
    .bind(&offer.origin_airport)
    .bind(&offer.destination_airport)
    .bind(rfc3339(offer.depart_at))
    .bind(rfc3339(offer.arrive_at))
    .bind(offer.depart_local_date.format("%Y-%m-%d").to_string())
    .bind(offer.arrive_local_date.format("%Y-%m-%d").to_string())
    .bind(offer.origin_tz.map(|tz| tz.name().to_string()))
    .bind(offer.destination_tz.map(|tz| tz.name().to_string()))
    .bind(offer.fallback_offset.local_minus_utc() as i64)
    .bind(offer.price)
    .bind(currency)
    .bind(offer.transfers as i64)
    .bind(offer.duration_min)
    .bind(&offer.airline)
    .bind(&offer.flight_number)
    .bind(&offer.link)
    .bind(fetched_at)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn upsert_route_success(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    origin: &str,
    destination: &str,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO routes(origin,destination,first_seen,last_seen,last_refreshed,source)
         VALUES(?,?,?,?,?,'bulk')
         ON CONFLICT(origin,destination) DO UPDATE SET
            last_seen=excluded.last_seen,
            last_refreshed=excluded.last_refreshed,
            source='bulk'",
    )
    .bind(origin)
    .bind(destination)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn row_to_offer(row: &SqliteRow) -> Result<Option<Offer>> {
    let depart_s: String = row.try_get("depart_at")?;
    let arrive_s: String = row.try_get("arrive_at")?;
    let depart_local_s: String = row.try_get("depart_local_date")?;
    let arrive_local_s: String = row.try_get("arrive_local_date")?;
    let fallback_secs: i64 = row.try_get("fallback_offset")?;
    let origin_tz_s: Option<String> = row.try_get("origin_tz")?;
    let destination_tz_s: Option<String> = row.try_get("destination_tz")?;

    let Ok(depart) = DateTime::parse_from_rfc3339(&depart_s) else {
        return Ok(None);
    };
    let Ok(arrive) = DateTime::parse_from_rfc3339(&arrive_s) else {
        return Ok(None);
    };
    let Ok(depart_local_date) = NaiveDate::parse_from_str(&depart_local_s, "%Y-%m-%d") else {
        return Ok(None);
    };
    let Ok(arrive_local_date) = NaiveDate::parse_from_str(&arrive_local_s, "%Y-%m-%d") else {
        return Ok(None);
    };
    let fallback_offset = FixedOffset::east_opt(fallback_secs as i32)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc offset"));
    Ok(Some(Offer {
        origin: row.try_get("origin")?,
        destination: row.try_get("destination")?,
        origin_airport: row.try_get("origin_airport")?,
        destination_airport: row.try_get("destination_airport")?,
        depart_at: depart.with_timezone(&Utc),
        arrive_at: arrive.with_timezone(&Utc),
        depart_local_date,
        arrive_local_date,
        origin_tz: origin_tz_s.as_deref().and_then(|value| value.parse::<Tz>().ok()),
        destination_tz: destination_tz_s
            .as_deref()
            .and_then(|value| value.parse::<Tz>().ok()),
        fallback_offset,
        price: row.try_get("price")?,
        transfers: row.try_get::<i64, _>("transfers")? as u32,
        duration_min: row.try_get("duration_min")?,
        airline: row.try_get("airline")?,
        flight_number: row.try_get("flight_number")?,
        link: row.try_get("link")?,
    }))
}
