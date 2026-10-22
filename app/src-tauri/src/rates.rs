//! Currency rates: the European Central Bank's daily reference rates (via frankfurter.dev),
//! cached in `rates.json` so conversions work offline. Fetched only once a currency is typed and
//! the cache is over a day old; the calculator never waits for the network.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use serde::Deserialize;

const URL: &str = "https://api.frankfurter.dev/v1/latest?base=EUR";
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Deserialize)]
struct Latest {
    date: String,
    /// Units of each currency per euro.
    rates: HashMap<String, f64>,
}

struct Rates {
    latest: Latest,
    fetched: SystemTime,
}

static RATES: RwLock<Option<Rates>> = RwLock::new(None);
static LOADED: AtomicBool = AtomicBool::new(false);
static FETCHING: AtomicBool = AtomicBool::new(false);

fn cache() -> PathBuf {
    common::data_dir().join("rates.json")
}

/// How many units of `code` buy one euro, and the date of the rates, from the cache.
pub fn per_euro(code: &str) -> Option<(f64, String)> {
    load_cache();
    refresh_if_stale();
    let rates = RATES.read().unwrap();
    let latest = &rates.as_ref()?.latest;
    let per_euro = if code == "EUR" {
        1.0
    } else {
        *latest.rates.get(code)?
    };
    Some((per_euro, latest.date.clone()))
}

fn load_cache() {
    if LOADED.swap(true, Ordering::SeqCst) {
        return;
    }
    let path = cache();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    match serde_json::from_str::<Latest>(&text) {
        Ok(latest) => {
            let fetched = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            *RATES.write().unwrap() = Some(Rates { latest, fetched });
        }
        Err(e) => tracing::warn!("ignoring unreadable {}: {e}", path.display()),
    }
}

fn refresh_if_stale() {
    let stale = RATES
        .read()
        .unwrap()
        .as_ref()
        .is_none_or(|r| r.fetched.elapsed().is_ok_and(|age| age > STALE));
    if !stale || FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        match fetch() {
            Ok((latest, text)) => {
                tracing::info!("currency rates of {} fetched", latest.date);
                if let Err(e) = std::fs::write(cache(), text) {
                    tracing::warn!("could not cache currency rates: {e}");
                }
                *RATES.write().unwrap() = Some(Rates {
                    latest,
                    fetched: SystemTime::now(),
                });
            }
            // Offline: keep the cached rates and try again on a later currency query.
            Err(e) => tracing::warn!("currency rates unavailable: {e}"),
        }
        FETCHING.store(false, Ordering::SeqCst);
    });
}

fn fetch() -> Result<(Latest, String), Box<dyn std::error::Error>> {
    // Windows' own TLS and certificate store.
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let text = agent.get(URL).call()?.body_mut().read_to_string()?;
    let latest: Latest = serde_json::from_str(&text)?;
    if latest.rates.is_empty() {
        return Err("no rates in the response".into());
    }
    Ok((latest, text))
}

#[cfg(test)]
pub fn set_for_test(date: &str, rates: &[(&str, f64)]) {
    LOADED.store(true, Ordering::SeqCst);
    *RATES.write().unwrap() = Some(Rates {
        latest: Latest {
            date: date.into(),
            rates: rates.iter().map(|&(c, r)| (c.to_string(), r)).collect(),
        },
        fetched: SystemTime::now(),
    });
}

#[cfg(test)]
mod tests {
    /// `cargo test -p seekr fetches_live_rates -- --ignored` (needs the network).
    #[test]
    #[ignore]
    fn fetches_live_rates() {
        let (latest, _) = super::fetch().unwrap();
        println!("{} INR per EUR on {}", latest.rates["INR"], latest.date);
        assert!(latest.rates.len() > 25);
    }
}
