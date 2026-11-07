use chrono::NaiveDate;
use fond_core::traits::Session;
use fond_core::types::{Bar, Symbol};
use sqlx::Row;

use crate::{Result, Store, fmt_date, fmt_time, parse_date, parse_time};

impl Store {
    pub async fn upsert_bars(&self, symbol: &Symbol, bars: &[Bar]) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        for b in bars {
            sqlx::query(
                "INSERT INTO bars (symbol, date, open, high, low, close, volume) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(symbol, date) DO UPDATE SET open = excluded.open, \
                 high = excluded.high, low = excluded.low, close = excluded.close, \
                 volume = excluded.volume",
            )
            .bind(symbol.as_str())
            .bind(fmt_date(b.date))
            .bind(b.open)
            .bind(b.high)
            .bind(b.low)
            .bind(b.close)
            .bind(b.volume)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(bars.len())
    }

    pub async fn bars(&self, symbol: &Symbol, from: NaiveDate, to: NaiveDate) -> Result<Vec<Bar>> {
        let rows = sqlx::query(
            "SELECT date, open, high, low, close, volume FROM bars \
             WHERE symbol = ? AND date >= ? AND date <= ? ORDER BY date",
        )
        .bind(symbol.as_str())
        .bind(fmt_date(from))
        .bind(fmt_date(to))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Bar {
                    date: parse_date(r.try_get("date")?)?,
                    open: r.try_get("open")?,
                    high: r.try_get("high")?,
                    low: r.try_get("low")?,
                    close: r.try_get("close")?,
                    volume: r.try_get("volume")?,
                })
            })
            .collect()
    }

    pub async fn last_bar_date(&self, symbol: &Symbol) -> Result<Option<NaiveDate>> {
        let v: Option<String> = sqlx::query_scalar("SELECT MAX(date) FROM bars WHERE symbol = ?")
            .bind(symbol.as_str())
            .fetch_one(&self.pool)
            .await?;
        v.as_deref().map(parse_date).transpose()
    }

    pub async fn upsert_sessions(&self, sessions: &[Session]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for s in sessions {
            sqlx::query(
                "INSERT INTO calendar_sessions (date, open, close) VALUES (?, ?, ?) \
                 ON CONFLICT(date) DO UPDATE SET open = excluded.open, close = excluded.close",
            )
            .bind(fmt_date(s.date))
            .bind(fmt_time(s.open))
            .bind(fmt_time(s.close))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn sessions(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT date, open, close FROM calendar_sessions \
             WHERE date >= ? AND date <= ? ORDER BY date",
        )
        .bind(fmt_date(from))
        .bind(fmt_date(to))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Session {
                    date: parse_date(r.try_get("date")?)?,
                    open: parse_time(r.try_get("open")?)?,
                    close: parse_time(r.try_get("close")?)?,
                })
            })
            .collect()
    }
}
