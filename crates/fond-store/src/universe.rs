use chrono::{DateTime, NaiveDate, Utc};
use fond_core::scanner::ScanReject;
use fond_core::traits::AssetInfo;
use fond_core::types::Symbol;
use sqlx::Row;

use crate::{Result, Store, StoreError, fmt_date, fmt_ts};

pub const SP500_CSV: &str = include_str!("../../../data/universe/sp500.csv");
pub const ETFS_CSV: &str = include_str!("../../../data/universe/etfs.csv");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniverseSource {
    Sp500,
    Etf,
    Manual,
}

impl UniverseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            UniverseSource::Sp500 => "sp500",
            UniverseSource::Etf => "etf",
            UniverseSource::Manual => "manual",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "sp500" => UniverseSource::Sp500,
            "etf" => UniverseSource::Etf,
            "manual" => UniverseSource::Manual,
            other => return Err(StoreError::Corrupt(format!("невідоме джерело '{other}'"))),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniverseEntry {
    pub symbol: Symbol,
    pub name: String,
    pub sector: String,
    pub source: UniverseSource,
    pub excluded: bool,
}

/// Розбирає один CSV-запис (RFC 4180) з тексту, повертає поля та залишок.
fn parse_records(text: &str) -> std::result::Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                    after_quote = true;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !after_quote => in_quotes = true,
            '"' => return Err("несподівані лапки в полі".into()),
            ',' => {
                row.push(std::mem::take(&mut field));
                after_quote = false;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                after_quote = false;
                if !(row.len() == 1 && row[0].is_empty()) {
                    records.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ if after_quote => return Err("символи після закриваючих лапок".into()),
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err("незакриті лапки".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        records.push(row);
    }
    Ok(records)
}

/// Заголовок `symbol,name,sector`; поля з комами беруться в лапки.
pub fn parse_universe_csv(
    text: &str,
    source: UniverseSource,
) -> std::result::Result<Vec<UniverseEntry>, String> {
    let records = parse_records(text)?;
    let mut it = records.into_iter();
    match it.next() {
        Some(h) if h.iter().map(|s| s.trim()).eq(["symbol", "name", "sector"]) => {}
        _ => return Err("очікується заголовок symbol,name,sector".into()),
    }
    let mut out = Vec::new();
    for (i, r) in it.enumerate() {
        if r.len() != 3 {
            return Err(format!(
                "рядок {}: очікувалось 3 поля, є {}",
                i + 2,
                r.len()
            ));
        }
        let symbol = Symbol::new(&r[0]);
        if symbol.as_str().is_empty() {
            return Err(format!("рядок {}: порожній тікер", i + 2));
        }
        out.push(UniverseEntry {
            symbol,
            name: r[1].trim().to_string(),
            sector: r[2].trim().to_string(),
            source,
            excluded: false,
        });
    }
    Ok(out)
}

fn row_to_entry(r: &sqlx::sqlite::SqliteRow) -> Result<UniverseEntry> {
    let symbol: String = r.try_get("symbol")?;
    let source: String = r.try_get("source")?;
    Ok(UniverseEntry {
        symbol: Symbol::new(&symbol),
        name: r.try_get("name")?,
        sector: r.try_get("sector")?,
        source: UniverseSource::parse(&source)?,
        excluded: r.try_get::<i64, _>("excluded")? != 0,
    })
}

impl Store {
    /// Заповнює вселенну вбудованими CSV, якщо вона порожня. `true`, лише якщо щось вставлено.
    pub async fn seed_universe_if_empty(&self) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM universe")
            .fetch_one(&mut *tx)
            .await?;
        if count > 0 {
            return Ok(false);
        }
        let sp =
            parse_universe_csv(SP500_CSV, UniverseSource::Sp500).map_err(StoreError::Corrupt)?;
        let etf = parse_universe_csv(ETFS_CSV, UniverseSource::Etf).map_err(StoreError::Corrupt)?;
        // S&P першим, ETF після: при збігу тікера рядок ETF перемагає.
        for e in sp.iter().chain(etf.iter()) {
            sqlx::query(
                "INSERT INTO universe (symbol, name, sector, source, excluded) \
                 VALUES (?, ?, ?, ?, 0) \
                 ON CONFLICT(symbol) DO UPDATE SET name = excluded.name, \
                 sector = excluded.sector, source = excluded.source",
            )
            .bind(e.symbol.as_str())
            .bind(&e.name)
            .bind(&e.sector)
            .bind(e.source.as_str())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn universe(&self, include_excluded: bool) -> Result<Vec<UniverseEntry>> {
        let sql = if include_excluded {
            "SELECT symbol, name, sector, source, excluded FROM universe ORDER BY symbol"
        } else {
            "SELECT symbol, name, sector, source, excluded FROM universe \
             WHERE excluded = 0 ORDER BY symbol"
        };
        let rows = sqlx::query(sql).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_entry).collect()
    }

    pub async fn set_excluded(&self, s: &Symbol, excluded: bool) -> Result<()> {
        sqlx::query("UPDATE universe SET excluded = ? WHERE symbol = ?")
            .bind(i64::from(excluded))
            .bind(s.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn add_manual(&self, e: &UniverseEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO universe (symbol, name, sector, source, excluded) \
             VALUES (?, ?, ?, 'manual', ?) \
             ON CONFLICT(symbol) DO UPDATE SET name = excluded.name, sector = excluded.sector, \
             source = 'manual', excluded = excluded.excluded",
        )
        .bind(e.symbol.as_str())
        .bind(&e.name)
        .bind(&e.sector)
        .bind(i64::from(e.excluded))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Видаляє лише записи, додані вручну.
    pub async fn remove_manual(&self, s: &Symbol) -> Result<()> {
        sqlx::query("DELETE FROM universe WHERE symbol = ? AND source = 'manual'")
            .bind(s.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_assets(&self, assets: &[AssetInfo], at: DateTime<Utc>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for a in assets {
            sqlx::query(
                "INSERT INTO assets (symbol, tradable, active, updated_at) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(symbol) DO UPDATE SET tradable = excluded.tradable, \
                 active = excluded.active, updated_at = excluded.updated_at",
            )
            .bind(a.symbol.as_str())
            .bind(i64::from(a.tradable))
            .bind(i64::from(a.active))
            .bind(fmt_ts(at))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn asset(&self, s: &Symbol) -> Result<Option<AssetInfo>> {
        let row = sqlx::query("SELECT symbol, tradable, active FROM assets WHERE symbol = ?")
            .bind(s.as_str())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| {
            Ok(AssetInfo {
                symbol: s.clone(),
                tradable: r.try_get::<i64, _>("tradable")? != 0,
                active: r.try_get::<i64, _>("active")? != 0,
            })
        })
        .transpose()
    }

    /// Замінює всі результати сканування за дату.
    pub async fn put_scan_results(
        &self,
        date: NaiveDate,
        results: &[(Symbol, Option<ScanReject>)],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM scan_results WHERE date = ?")
            .bind(fmt_date(date))
            .execute(&mut *tx)
            .await?;
        for (sym, reject) in results {
            let reason = reject
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::Corrupt(format!("причина скану: {e}")))?;
            sqlx::query(
                "INSERT INTO scan_results (date, symbol, passed, reason) VALUES (?, ?, ?, ?)",
            )
            .bind(fmt_date(date))
            .bind(sym.as_str())
            .bind(i64::from(reject.is_none()))
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn scan_results(&self, date: NaiveDate) -> Result<Vec<(Symbol, Option<ScanReject>)>> {
        let rows = sqlx::query(
            "SELECT symbol, passed, reason FROM scan_results WHERE date = ? ORDER BY symbol",
        )
        .bind(fmt_date(date))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let sym: String = r.try_get("symbol")?;
                let passed = r.try_get::<i64, _>("passed")? != 0;
                let reason: Option<String> = r.try_get("reason")?;
                let reject = match (passed, reason) {
                    (true, _) => None,
                    (false, Some(j)) => Some(
                        serde_json::from_str::<ScanReject>(&j)
                            .map_err(|e| StoreError::Corrupt(format!("причина скану: {e}")))?,
                    ),
                    (false, None) => {
                        return Err(StoreError::Corrupt("відхилення без причини".into()));
                    }
                };
                Ok((Symbol::new(&sym), reject))
            })
            .collect()
    }
}
