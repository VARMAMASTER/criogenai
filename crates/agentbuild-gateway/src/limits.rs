//! Per-key rate limits and token budgets (design 0006 §13.2–13.3), in
//! memory. Time comes from Tokio's clock, so tests can pause and advance
//! it; calendar periods are UTC.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// How long a budget lasts before it starts over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetPeriod {
    /// A UTC calendar day.
    Day,
    /// A UTC calendar month.
    Month,
    /// Never starts over.
    Total,
}

impl BudgetPeriod {
    /// `"day"`, `"month"` or `"total"`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "day" => Some(Self::Day),
            "month" => Some(Self::Month),
            "total" => Some(Self::Total),
            _ => None,
        }
    }
}

/// Limits for one virtual key; none are set by default.
///
/// ```
/// use agentbuild_gateway::{BudgetPeriod, KeyLimits};
///
/// let limits = KeyLimits::new()
///     .requests_per_minute(60)
///     .tokens_per_minute(100_000)
///     .budget(5_000_000, BudgetPeriod::Month);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyLimits {
    /// Requests per minute.
    pub requests_per_minute: Option<u32>,
    /// Input plus output tokens per minute.
    pub tokens_per_minute: Option<u64>,
    /// Input plus output tokens per period.
    pub budget: Option<(u64, BudgetPeriod)>,
    /// Requests in flight at once.
    pub max_concurrent: Option<u32>,
}

impl KeyLimits {
    /// No limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// At most `n` requests per minute (a bucket that refills continuously).
    pub fn requests_per_minute(mut self, n: u32) -> Self {
        self.requests_per_minute = Some(n);
        self
    }

    /// At most `n` tokens per minute, charged when an answer is complete.
    pub fn tokens_per_minute(mut self, n: u64) -> Self {
        self.tokens_per_minute = Some(n);
        self
    }

    /// At most `tokens` per `period`.
    pub fn budget(mut self, tokens: u64, period: BudgetPeriod) -> Self {
        self.budget = Some((tokens, period));
        self
    }

    /// At most `n` requests in flight at once (a stream counts until it
    /// ends).
    pub fn max_concurrent(mut self, n: u32) -> Self {
        self.max_concurrent = Some(n);
        self
    }

    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `key_rate_limited`, retry after the duration.
    RateLimited(Duration),
    /// `budget_exceeded`; the period ends after the duration (none for
    /// `total`).
    Budget(Option<Duration>),
    /// `concurrency_limited`: too many requests in flight; retry soon.
    Concurrency,
}

impl Refusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::RateLimited(_) => "key_rate_limited",
            Self::Budget(_) => "budget_exceeded",
            Self::Concurrency => "concurrency_limited",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::RateLimited(_) => "this key's rate limit is reached; retry after the given time",
            Self::Budget(_) => "this key's token budget is spent for the current period",
            Self::Concurrency => "too many requests are in flight; retry shortly",
        }
    }

    /// Whole seconds for `Retry-After`, at least 1.
    pub(crate) fn retry_after_secs(self) -> Option<u64> {
        let wait = match self {
            Self::RateLimited(wait) => Some(wait),
            Self::Budget(wait) => wait,
            Self::Concurrency => Some(Duration::from_secs(1)),
        }?;
        Some(wait.as_secs_f64().ceil().max(1.0) as u64)
    }
}

/// A bucket that refills continuously up to one minute's allowance.
#[derive(Debug)]
struct Bucket {
    per_minute: f64,
    level: f64,
    at: Instant,
}

impl Bucket {
    fn new(per_minute: u64, now: Instant) -> Self {
        Self {
            per_minute: per_minute as f64,
            level: per_minute as f64,
            at: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let minutes = now.duration_since(self.at).as_secs_f64() / 60.0;
        self.level = (self.level + minutes * self.per_minute).min(self.per_minute);
        self.at = now;
    }

    /// The wait until the level reaches `needed`.
    fn wait_for(&self, needed: f64) -> Duration {
        let missing = needed - self.level;
        if missing <= 0.0 || self.per_minute <= 0.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(missing / self.per_minute * 60.0)
    }
}

/// A key's limits and what it has used.
#[derive(Debug)]
pub(crate) struct Usage {
    requests: Option<Bucket>,
    tokens: Option<Bucket>,
    budget: Option<(u64, BudgetPeriod)>,
    spent: u64,
    /// The period `spent` belongs to.
    period: u64,
    /// Wall-clock seconds at `started`: calendar periods follow Tokio's
    /// clock from there.
    wall: u64,
    started: Instant,
}

impl Usage {
    pub(crate) fn new(limits: &KeyLimits) -> Self {
        let now = Instant::now();
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut usage = Self {
            requests: limits
                .requests_per_minute
                .map(|n| Bucket::new(u64::from(n), now)),
            tokens: limits.tokens_per_minute.map(|n| Bucket::new(n, now)),
            budget: limits.budget,
            spent: 0,
            period: 0,
            wall,
            started: now,
        };
        usage.period = usage.period_now().0;
        usage
    }

    /// Unix seconds now.
    fn wall_now(&self) -> u64 {
        self.wall + self.started.elapsed().as_secs()
    }

    /// The current period's number and the time until it ends.
    fn period_now(&self) -> (u64, Option<Duration>) {
        let now = self.wall_now();
        match self.budget.map(|(_, period)| period) {
            Some(BudgetPeriod::Day) => {
                let day = now / 86_400;
                (day, Some(Duration::from_secs((day + 1) * 86_400 - now)))
            }
            Some(BudgetPeriod::Month) => {
                let (year, month, _) = civil_from_days(now / 86_400);
                let (next_year, next_month) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                let next = days_from_civil(next_year, next_month, 1) * 86_400;
                (
                    (year as u64) * 12 + u64::from(month),
                    Some(Duration::from_secs(next.saturating_sub(now))),
                )
            }
            Some(BudgetPeriod::Total) | None => (0, None),
        }
    }

    fn roll_over(&mut self) {
        let (period, _) = self.period_now();
        if period != self.period {
            self.period = period;
            self.spent = 0;
        }
    }

    /// Admits one request, or says why not (nothing is taken then).
    pub(crate) fn admit(&mut self) -> Result<(), Refusal> {
        let now = Instant::now();
        self.roll_over();
        if let Some((tokens, _)) = self.budget
            && self.spent >= tokens
        {
            return Err(Refusal::Budget(self.period_now().1));
        }
        if let Some(bucket) = &mut self.tokens {
            bucket.refill(now);
            if bucket.level < 0.0 {
                // Until the balance is back to zero.
                return Err(Refusal::RateLimited(bucket.wait_for(0.0)));
            }
        }
        if let Some(bucket) = &mut self.requests {
            bucket.refill(now);
            if bucket.level < 1.0 {
                return Err(Refusal::RateLimited(bucket.wait_for(1.0)));
            }
            bucket.level -= 1.0;
        }
        Ok(())
    }

    /// Takes `tokens` before the answer exists, so requests in flight count
    /// against the budget and the per-minute allowance at once.
    pub(crate) fn reserve(&mut self, tokens: u64) {
        self.charge(tokens);
    }

    /// Replaces a reservation by the tokens really used.
    pub(crate) fn settle(&mut self, reserved: u64, actual: u64) {
        self.roll_over();
        self.spent = self.spent.saturating_sub(reserved).saturating_add(actual);
        if let Some(bucket) = &mut self.tokens {
            bucket.refill(Instant::now());
            bucket.level = (bucket.level + reserved as f64 - actual as f64).min(bucket.per_minute);
        }
    }

    /// Charges an answer's tokens.
    pub(crate) fn charge(&mut self, tokens: u64) {
        self.roll_over();
        self.spent = self.spent.saturating_add(tokens);
        if let Some(bucket) = &mut self.tokens {
            bucket.refill(Instant::now());
            bucket.level -= tokens as f64;
        }
    }

    /// Tokens spent in the current period.
    pub(crate) fn spent(&mut self) -> u64 {
        self.roll_over();
        self.spent
    }
}

// Calendar arithmetic on days since 1970-01-01 (UTC), the well-known
// proleptic Gregorian conversions.

fn days_from_civil(year: i64, month: u32, day: u32) -> u64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468).max(0) as u64
}

fn civil_from_days(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-10-22, a leap day and a year end.
        for (y, m, d) in [(2026, 10, 22), (2024, 2, 29), (2025, 12, 31), (2000, 3, 1)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(2026, 10, 22), 20_748);
    }

    #[tokio::test(start_paused = true)]
    async fn request_buckets_refill_over_the_minute() {
        let mut usage = Usage::new(&KeyLimits::new().requests_per_minute(2));
        assert!(usage.admit().is_ok());
        assert!(usage.admit().is_ok());
        let Err(Refusal::RateLimited(wait)) = usage.admit() else {
            panic!("the third request is refused");
        };
        assert_eq!(wait, Duration::from_secs(30));
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(usage.admit().is_ok());
        assert!(usage.admit().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn tokens_are_charged_after_the_answer() {
        let mut usage = Usage::new(&KeyLimits::new().tokens_per_minute(1_000));
        assert!(usage.admit().is_ok());
        usage.charge(1_500); // one request may go past the allowance
        let Err(refusal) = usage.admit() else {
            panic!("the next one waits");
        };
        assert_eq!(refusal.retry_after_secs(), Some(30));
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(usage.admit().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn reservations_hold_back_concurrent_requests_and_settle_to_the_real_usage() {
        let mut usage = Usage::new(&KeyLimits::new().budget(1_000, BudgetPeriod::Total));
        // Four requests in flight, each reserving 400: the third finds the
        // budget spent, however slowly the answers arrive.
        let mut admitted = 0;
        for _ in 0..4 {
            if usage.admit().is_ok() {
                usage.reserve(400);
                admitted += 1;
            }
        }
        assert_eq!(admitted, 3, "overshoot is bounded by one request");
        // An answer that used less gives the difference back.
        usage.settle(400, 50);
        assert_eq!(usage.spent(), 850);
        assert!(usage.admit().is_ok());

        let mut per_minute = Usage::new(&KeyLimits::new().tokens_per_minute(1_000));
        per_minute.reserve(900);
        per_minute.settle(900, 100);
        assert!(
            per_minute.admit().is_ok(),
            "the refund restored the allowance"
        );
        // The refund never exceeds a full minute's allowance.
        per_minute.settle(0, 0);
        per_minute.settle(100, 0);
        assert!(per_minute.admit().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn budgets_refuse_until_the_period_ends() {
        let mut usage = Usage::new(&KeyLimits::new().budget(100, BudgetPeriod::Day));
        assert!(usage.admit().is_ok());
        usage.charge(150);
        assert_eq!(usage.spent(), 150);
        let Err(Refusal::Budget(Some(left))) = usage.admit() else {
            panic!("the budget is spent");
        };
        assert!(left <= Duration::from_secs(86_400));
        tokio::time::advance(left).await;
        assert!(usage.admit().is_ok(), "a new day");
        assert_eq!(usage.spent(), 0);

        let mut total = Usage::new(&KeyLimits::new().budget(10, BudgetPeriod::Total));
        total.charge(10);
        assert_eq!(total.admit(), Err(Refusal::Budget(None)));
        assert_eq!(Refusal::Budget(None).retry_after_secs(), None);
    }
}
