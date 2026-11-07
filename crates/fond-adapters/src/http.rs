//! Спільні HTTP-утиліти: маскований секрет, повтори, ліміт запитів, клієнт.

use fond_core::error::IsTransient;
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Секретне значення. `Debug`/`Display` показують лише `****` і останні 4 символи.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Відкрите значення — лише для підстановки у запит.
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn masked(&self) -> String {
        let n = self.0.chars().count();
        if n <= 4 {
            "****".to_string()
        } else {
            let tail: String = self.0.chars().skip(n - 4).collect();
            format!("****{tail}")
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

/// Повторює `op` лише для тимчасових помилок; пауза `base·2^(n−1)`, не більше `max`.
pub async fn with_retry<T, E, F, Fut>(p: &RetryPolicy, mut op: F) -> Result<T, E>
where
    E: IsTransient,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let max = p.max_attempts.max(1);
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt >= max || !e.is_transient() => return Err(e),
            Err(_) => {
                let factor = 2u32.saturating_pow(attempt - 1);
                let delay = p.base_delay.saturating_mul(factor).min(p.max_delay);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Token bucket: ємність `per_minute`, безперервне поповнення `per_minute` за 60 с.
pub struct RateLimiter {
    capacity: f64,
    per_sec: f64,
    state: Mutex<Bucket>,
}

impl RateLimiter {
    /// `per_minute == 0` трактується як 1.
    pub fn new(per_minute: u32) -> Self {
        let cap = f64::from(per_minute.max(1));
        Self {
            capacity: cap,
            per_sec: cap / 60.0,
            state: Mutex::new(Bucket {
                tokens: cap,
                last: Instant::now(),
            }),
        }
    }

    pub async fn acquire(&self) {
        // Мьютекс тримаємо і під час очікування: так виклики йдуть по черзі.
        let mut b = self.state.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.per_sec).min(self.capacity);
        b.last = now;
        if b.tokens < 1.0 {
            let wait = Duration::from_secs_f64((1.0 - b.tokens) / self.per_sec);
            tokio::time::sleep(wait).await;
            b.tokens = 1.0;
            b.last = Instant::now();
        }
        b.tokens -= 1.0;
    }
}

/// HTTP-клієнт із таймаутом і user-agent `fond/0.1` (rustls).
pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("fond/0.1")
        .build()
        .expect("reqwest client with rustls and a static user agent always builds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fond_core::error::BrokerError;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::Instant;

    #[test]
    fn secret_debug_is_masked() {
        let s = Secret::new("sk-abcdef1234");
        assert_eq!(format!("{s:?}"), "****1234");
        assert_eq!(format!("{s}"), "****1234");
        assert!(!format!("{s}").contains("abcdef"));
        assert_eq!(s.expose(), "sk-abcdef1234");
        assert_eq!(format!("{:?}", Secret::new("abcd")), "****");
        assert_eq!(format!("{:?}", Secret::new("")), "****");
    }

    #[tokio::test(start_paused = true)]
    async fn retry_retries_transient_only() {
        let p = RetryPolicy::default();
        let calls = AtomicU32::new(0);
        let r: Result<u32, BrokerError> = with_retry(&p, || async {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= 2 {
                Err(BrokerError::Transient("x".into()))
            } else {
                Ok(n)
            }
        })
        .await;
        assert_eq!(r.unwrap(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let r: Result<u32, BrokerError> = with_retry(&p, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(BrokerError::Unauthorized)
        })
        .await;
        assert!(matches!(r, Err(BrokerError::Unauthorized)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backoff_is_exponential_and_capped() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(1),
        };
        let start = Instant::now();
        let calls = AtomicU32::new(0);
        let r: Result<(), BrokerError> = with_retry(&p, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(BrokerError::Transient("x".into()))
        })
        .await;
        assert!(matches!(r, Err(BrokerError::Transient(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        // паузи після спроб 1..4: 500, 1000, 1000 (cap), 1000 (cap)
        assert_eq!(start.elapsed(), Duration::from_millis(3500));
    }

    #[test]
    fn retry_policy_defaults() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.base_delay, Duration::from_millis(500));
        assert_eq!(p.max_delay, Duration::from_secs(8));
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_spaces_calls() {
        let rl = RateLimiter::new(60);
        let start = Instant::now();
        for _ in 0..60 {
            rl.acquire().await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
        rl.acquire().await; // 61-й
        let e = start.elapsed();
        assert!(
            e >= Duration::from_millis(990) && e <= Duration::from_millis(1100),
            "{e:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_zero_is_treated_as_one() {
        let rl = RateLimiter::new(0);
        rl.acquire().await;
        let start = Instant::now();
        rl.acquire().await;
        assert!(start.elapsed() >= Duration::from_secs(59));
    }

    #[test]
    fn http_client_builds() {
        let _ = http_client(Duration::from_secs(5));
    }
}
