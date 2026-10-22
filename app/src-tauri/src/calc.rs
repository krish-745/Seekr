//! Calculator and converters, offered as the first result when the query evaluates: date math
//! ("today + 30 days", "days until 25 dec"), time zones ("time in tokyo", "3pm ist to pst"), and
//! everything fend does (math, units, number bases, percentages), with currencies at the ECB's
//! daily rates converted to rupees unless the query names another target. Enter copies the result.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

use chrono::{
    DateTime, Datelike, Duration as Days, Local, Months, NaiveDate, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;
use fend_core::{CustomUnitAttribute, ExchangeRateFnV2, ExchangeRateFnV2Options, Interrupt};

use crate::{
    query::{Action, Item},
    rates,
};

const COPY: Action = Action {
    id: "copy",
    label: "Copy result",
};
/// A keystroke has 16 ms for its first results; a calculation that takes longer is dropped.
const BUDGET: Duration = Duration::from_millis(8);
const DEFAULT_CURRENCY: &str = "INR";
/// The currencies the ECB publishes rates for.
const CURRENCIES: [&str; 31] = [
    "AUD", "BGN", "BRL", "CAD", "CHF", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "IDR",
    "ILS", "INR", "ISK", "JPY", "KRW", "MXN", "MYR", "NOK", "NZD", "PHP", "PLN", "RON", "SEK",
    "SGD", "THB", "TRY", "USD", "ZAR",
];

struct Answer {
    value: String,
    note: String,
}

pub fn items(text: &str) -> Vec<Item> {
    let text = text.trim();
    let now = Utc::now();
    let answer = date_math(text, now.with_timezone(&Local).date_naive())
        .or_else(|| zones(text, now, &Local))
        .or_else(|| math(text));
    let Some(Answer { value, note }) = answer else {
        return Vec::new();
    };
    vec![Item {
        id: format!("calc:{value}"),
        title: value.clone(),
        subtitle: note,
        preview: format!("{text}\n= {value}"),
        icon: None,
        line: None,
        actions: vec![COPY],
    }]
}

// ---- fend: math, units, bases, percentages, currency ----

struct Deadline(Instant);

impl Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() > self.0
    }
}

thread_local! {
    /// The date of the rates a calculation used, if it converted currency.
    static RATES_DATE: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct Ecb;

impl ExchangeRateFnV2 for Ecb {
    fn relative_to_base_currency(
        &self,
        currency: &str,
        _: &ExchangeRateFnV2Options,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync + 'static>> {
        let (value, date) =
            rates::per_euro(currency).ok_or_else(|| format!("no rate for {currency}"))?;
        RATES_DATE.with(|d| *d.borrow_mut() = Some(date));
        Ok(value)
    }
}

fn math(text: &str) -> Option<Answer> {
    let lower = text.to_lowercase();
    // A bare word is a name being typed ("c" is the speed of light to fend), not a calculation.
    let worth_trying = text.chars().any(|c| c.is_ascii_digit())
        || ["(", "sqrt", "pi", " to ", " in ", " as "]
            .iter()
            .any(|w| lower.contains(w));
    if !worth_trying {
        return None;
    }
    let mut input = rupee_sign(&currency_codes(text));
    let currencies: Vec<&str> = CURRENCIES
        .iter()
        .copied()
        .filter(|c| {
            input
                .split(|ch: char| !ch.is_alphanumeric())
                .any(|w| w == *c)
        })
        .collect();
    let symbol = input.contains(['$', '€', '£', '¥']);
    let converts = [" to ", " in ", " as "].iter().any(|w| lower.contains(w));
    if (symbol || !currencies.is_empty()) && !converts && currencies != [DEFAULT_CURRENCY] {
        input = format!("{input} to {DEFAULT_CURRENCY}");
    }
    let input = percent_change(&input).unwrap_or(input);

    let mut context = fend_core::Context::new();
    for (singular, plural) in [("rupee", "rupees"), ("Rs", "Rs")] {
        context.define_custom_unit_v1(singular, plural, "INR", &CustomUnitAttribute::None);
    }
    context.set_exchange_rate_handler_v2(Ecb);
    RATES_DATE.with(|d| d.borrow_mut().take());
    let deadline = Deadline(Instant::now() + BUDGET);
    let result = fend_core::evaluate_preview_with_interrupt(&input, &context, &deadline);
    let value = result.get_main_result().trim();
    if value.is_empty() {
        return None;
    }
    let date = RATES_DATE.with(|d| d.borrow_mut().take());
    let (value, rounded) = tidy(value, if date.is_some() { 2 } else { 4 });
    let mut note = match date {
        Some(date) => format!("Currency · ECB rates of {date}"),
        None => "Calculator".into(),
    };
    if rounded {
        note.push_str(" · rounded");
    }
    Some(Answer { value, note })
}

/// "approx. 944.6902260993 INR" becomes "944.69 INR": `decimals` places (or four significant
/// digits below 1), without trailing zeros. Says whether anything was dropped.
fn tidy(value: &str, decimals: usize) -> (String, bool) {
    let (body, approx) = match value.strip_prefix("approx. ") {
        Some(rest) => (rest, true),
        None => (value, false),
    };
    let (number, unit) = body.split_once(' ').unwrap_or((body, ""));
    let Ok(x) = number.parse::<f64>() else {
        return (body.to_string(), approx);
    };
    if !number.contains('.') || number.contains(['e', 'E']) {
        return (body.to_string(), approx);
    }
    let places = if x.abs() >= 1.0 || x == 0.0 {
        decimals
    } else {
        // 0.000123456 keeps 0.0001235.
        decimals.max((3.0 - x.abs().log10().floor()) as usize)
    };
    let short = format!("{x:.places$}");
    let short = short.trim_end_matches('0').trim_end_matches('.');
    let rounded = approx || short.parse::<f64>().ok() != Some(x);
    let out = if unit.is_empty() {
        short.to_string()
    } else {
        format!("{short} {unit}")
    };
    (out, rounded)
}

/// "100 usd" means USD: fend only knows the codes in capitals. "try" stays a word.
fn currency_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let upper = word.to_uppercase();
        if word != "try" && CURRENCIES.contains(&upper.as_str()) {
            out.push_str(&upper);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// "₹500" is "500 INR": fend's parser doesn't take ₹ as a unit.
fn rupee_sign(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '₹' {
            out.push(c);
            continue;
        }
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        while let Some(&d) = chars
            .peek()
            .filter(|d| d.is_ascii_digit() || **d == '.' || **d == ',')
        {
            out.push(d);
            chars.next();
        }
        out.push_str(" INR");
    }
    out
}

/// "150 + 10%" means 165 (fend reads it as 150.1), "80 - 25%" means 60.
fn percent_change(input: &str) -> Option<String> {
    let body = input.trim().strip_suffix('%')?;
    let at = body.rfind(['+', '-'])?;
    let (base, sign, percent) = (body[..at].trim(), &body[at..=at], body[at + 1..].trim());
    percent.parse::<f64>().ok()?;
    if base.is_empty() || base.ends_with(['*', '/', '^', 'e', 'E']) {
        return None;
    }
    Some(format!("({base}) * (1 {sign} {percent} / 100)"))
}

// ---- dates ----

fn date_math(text: &str, today: NaiveDate) -> Option<Answer> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    for prefix in ["days until ", "days till ", "days to ", "days since "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let date = date(rest, today)?;
            let days = (date - today).num_days();
            let days = if prefix == "days since " { -days } else { days };
            return Some(Answer {
                value: count(days, "day"),
                note: format!(
                    "{} {}",
                    prefix.trim_start_matches("days ").trim(),
                    long(date)
                ),
            });
        }
    }
    if let Some(rest) = lower.strip_suffix(" ago") {
        return shifted(today, span(rest)?, -1, today);
    }
    if let Some(rest) = lower.strip_prefix("in ") {
        return shifted(today, span(rest)?, 1, today);
    }
    for (word, sign) in [(" from ", 1), (" after ", 1), (" before ", -1)] {
        if let Some((left, right)) = lower.split_once(word) {
            return shifted(date(right, today)?, span(left)?, sign, today);
        }
    }
    for (word, sign) in [(" + ", 1), (" - ", -1)] {
        if let Some((left, right)) = lower.rsplit_once(word) {
            let start = date(left, today)?;
            if let Some(amount) = span(right) {
                return shifted(start, amount, sign, today);
            }
            let end = date(right, today).filter(|_| sign < 0)?;
            let days = (start - end).num_days();
            return Some(Answer {
                value: count(days, "day"),
                note: format!("from {} to {}", long(end), long(start)),
            });
        }
    }
    // A date on its own says which day of the week it is.
    let day = date(lower, today)?;
    Some(Answer {
        value: long(day),
        note: relative(day, today),
    })
}

fn shifted(
    start: NaiveDate,
    (n, unit): (i64, Unit),
    sign: i64,
    today: NaiveDate,
) -> Option<Answer> {
    let n = n * sign;
    let day = match unit {
        Unit::Days => start.checked_add_signed(Days::days(n))?,
        Unit::Weeks => start.checked_add_signed(Days::weeks(n))?,
        Unit::Months | Unit::Years => {
            let months = if unit == Unit::Years { n * 12 } else { n };
            let m = Months::new(u32::try_from(months.unsigned_abs()).ok()?);
            if months < 0 {
                start.checked_sub_months(m)?
            } else {
                start.checked_add_months(m)?
            }
        }
    };
    Some(Answer {
        value: long(day),
        note: relative(day, today),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Days,
    Weeks,
    Months,
    Years,
}

/// "30 days", "2 weeks", "1 month", "3 years".
fn span(text: &str) -> Option<(i64, Unit)> {
    let (n, unit) = text.trim().split_once(' ')?;
    let unit = match unit.trim().trim_end_matches('s') {
        "day" => Unit::Days,
        "week" => Unit::Weeks,
        "month" => Unit::Months,
        "year" => Unit::Years,
        _ => return None,
    };
    Some((n.parse().ok()?, unit))
}

/// today, tomorrow, yesterday, 2026-12-25, 25 dec 2026, dec 25, december 25, 2026.
pub(crate) fn date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let text = text.trim().trim_end_matches('.');
    match text {
        "today" | "now" => return Some(today),
        "tomorrow" => return today.succ_opt(),
        "yesterday" => return today.pred_opt(),
        _ => {}
    }
    if !text.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let text = text.replace(',', "");
    if let Ok(d) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return Some(d);
    }
    let formats = ["%d %b %Y", "%d %B %Y", "%b %d %Y", "%B %d %Y"];
    if let Some(d) = formats
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(&text, f).ok())
    {
        return Some(d);
    }
    // Without a year: the next time that day comes round.
    let this_year = formats
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(&format!("{text} {}", today.year()), f).ok())?;
    if this_year >= today {
        Some(this_year)
    } else {
        this_year.with_year(today.year() + 1)
    }
}

fn long(day: NaiveDate) -> String {
    day.format("%A, %-d %B %Y").to_string()
}

fn relative(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        n if n > 0 => format!("In {}", count(n, "day")),
        n => format!("{} ago", count(-n, "day")),
    }
}

fn count(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

// ---- time zones ----

fn zones<L: TimeZone>(text: &str, now: DateTime<Utc>, local: &L) -> Option<Answer> {
    let lower = text.to_lowercase();
    let lower = lower.trim();
    // "time in tokyo", "now in pst", "tokyo time"
    let place = ["time in ", "now in ", "time "]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .or_else(|| lower.strip_suffix(" time"));
    if let Some(place) = place {
        let tz = zone(place)?;
        let there = now.with_timezone(&tz);
        return Some(Answer {
            value: there.format("%-I:%M %p").to_string(),
            note: format!("{} · {}", there.format("%a %-d %b"), describe(&tz, &there)),
        });
    }
    // "3pm ist to pst", "15:30 in london", "9am tokyo in london"
    let (left, right) = lower
        .rsplit_once(" to ")
        .or_else(|| lower.rsplit_once(" in "))?;
    let target = zone(right)?;
    let today = now.with_timezone(local).date_naive();
    let from = |time: NaiveTime, tz: &Tz| -> Option<DateTime<Tz>> {
        let at = tz.from_local_datetime(&today.and_time(time)).earliest()?;
        Some(at.with_timezone(&target))
    };
    let (there, source) = if let Some(time) = clock(left) {
        let at = local
            .from_local_datetime(&today.and_time(time))
            .earliest()?;
        (at.with_timezone(&target), "here".to_string())
    } else {
        let (time, place) = left.split_once(' ')?;
        let (time, tz) = (clock(time)?, zone(place)?);
        let start = tz.from_local_datetime(&today.and_time(time)).earliest()?;
        (from(time, &tz)?, start.format("%-I:%M %p %Z").to_string())
    };
    Some(Answer {
        value: there.format("%-I:%M %p").to_string(),
        note: format!(
            "{} · {} · from {source}",
            there.format("%a %-d %b"),
            describe(&target, &there)
        ),
    })
}

/// "Tokyo (JST, UTC+09:00)".
fn describe(tz: &Tz, at: &DateTime<Tz>) -> String {
    let city = tz.name().rsplit('/').next().unwrap_or("").replace('_', " ");
    format!("{city} ({}, UTC{})", at.format("%Z"), at.format("%:z"))
}

/// "3pm", "3:30 pm", "15:30", "noon", "midnight".
fn clock(text: &str) -> Option<NaiveTime> {
    let text = text.trim().replace(' ', "");
    match text.as_str() {
        "noon" => return NaiveTime::from_hms_opt(12, 0, 0),
        "midnight" => return NaiveTime::from_hms_opt(0, 0, 0),
        _ => {}
    }
    let (body, pm) = match (text.strip_suffix("am"), text.strip_suffix("pm")) {
        (Some(b), _) => (b, Some(false)),
        (_, Some(b)) => (b, Some(true)),
        _ => (text.as_str(), None),
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        // A bare number is only a time with am or pm: "15 in km" is a calculation.
        None if pm.is_some() => (body.parse::<u32>().ok()?, 0),
        None => return None,
    };
    let h = match pm {
        Some(pm) if (1..=12).contains(&h) => h % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// An abbreviation, a country or city the time zone database doesn't name, or a database name or
/// its city ("tokyo", "los angeles", "Europe/Paris").
fn zone(text: &str) -> Option<Tz> {
    let text = text.trim().to_lowercase();
    let name = match text.as_str() {
        "ist" | "india" | "delhi" | "new delhi" | "mumbai" | "bangalore" | "bengaluru"
        | "chennai" | "hyderabad" | "pune" => "Asia/Kolkata",
        "utc" | "gmt" => "UTC",
        "pst" | "pdt" | "pt" | "san francisco" | "sf" | "seattle" | "california" => {
            "America/Los_Angeles"
        }
        "mst" | "mdt" => "America/Denver",
        "cst" | "cdt" | "ct" => "America/Chicago",
        "est" | "edt" | "et" | "nyc" | "boston" | "washington" => "America/New_York",
        "bst" | "uk" | "england" => "Europe/London",
        "cet" | "cest" | "france" => "Europe/Paris",
        "germany" => "Europe/Berlin",
        "eet" | "eest" => "Europe/Athens",
        "msk" => "Europe/Moscow",
        "gst" | "uae" => "Asia/Dubai",
        "pkt" | "pakistan" => "Asia/Karachi",
        "jst" | "japan" => "Asia/Tokyo",
        "kst" | "korea" => "Asia/Seoul",
        "sgt" => "Asia/Singapore",
        "hkt" => "Asia/Hong_Kong",
        "beijing" | "china" => "Asia/Shanghai",
        "aest" | "aedt" | "australia" => "Australia/Sydney",
        "nzst" | "nzdt" | "new zealand" => "Pacific/Auckland",
        _ => {
            let key = text.replace(' ', "_");
            return chrono_tz::TZ_VARIANTS.iter().copied().find(|tz| {
                let name = tz.name().to_lowercase();
                name == key || name.rsplit('/').next() == Some(key.as_str())
            });
        }
    };
    name.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Option<String> {
        math(text).map(|a| a.value)
    }

    #[test]
    fn math_units_bases_and_percentages() {
        assert_eq!(value("2+2").as_deref(), Some("4"));
        assert_eq!(value("sqrt(2) * 10").as_deref(), Some("14.1421"));
        assert_eq!(value("5 km to miles").as_deref(), Some("3.1069 miles"));
        assert_eq!(value("255 to hex").as_deref(), Some("ff"));
        assert_eq!(value("0b1010 to decimal").as_deref(), Some("10"));
        assert_eq!(value("150 + 10%").as_deref(), Some("165"));
        assert_eq!(value("80 - 25%").as_deref(), Some("60"));
        assert_eq!(value("20% of 150").as_deref(), Some("30"));
        // Names being typed and plain numbers aren't calculations.
        assert_eq!(value("chrome"), None);
        assert_eq!(value("c"), None);
        assert_eq!(value("42"), None);
        assert_eq!(value("report 2024"), None);
        assert_eq!(
            tidy("approx. 0.000123456 kg", 4),
            ("0.0001235 kg".into(), true)
        );
        assert_eq!(tidy("2.50", 4), ("2.5".into(), false));
        assert_eq!(tidy("1e30", 4), ("1e30".into(), false));
    }

    #[test]
    fn currency_defaults_to_rupees_at_cached_rates() {
        rates::set_for_test("2026-10-21", &[("USD", 1.1), ("INR", 92.4), ("GBP", 0.85)]);
        let a = math("10 usd").unwrap();
        assert_eq!(a.value, "840 INR");
        assert_eq!(a.note, "Currency · ECB rates of 2026-10-21");
        assert_eq!(value("$10").as_deref(), Some("840 INR"));
        assert_eq!(value("17 gbp to usd").as_deref(), Some("22 USD"));
        assert_eq!(value("₹924 to EUR").as_deref(), Some("10 EUR"));
        assert_eq!(value("924 rupees in eur").as_deref(), Some("10 EUR"));
        assert_eq!(value("₹ 924"), None, "already rupees");
    }

    #[test]
    fn date_math() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let v = |t: &str| super::date_math(t, today).map(|a| a.value);
        assert_eq!(
            v("today + 30 days").as_deref(),
            Some("Saturday, 21 November 2026")
        );
        assert_eq!(
            v("2026-01-31 + 1 month").as_deref(),
            Some("Saturday, 28 February 2026")
        );
        assert_eq!(
            v("3 weeks from tomorrow").as_deref(),
            Some("Friday, 13 November 2026")
        );
        assert_eq!(v("10 days ago").as_deref(), Some("Monday, 12 October 2026"));
        assert_eq!(v("in 2 years").as_deref(), Some("Sunday, 22 October 2028"));
        assert_eq!(v("days until 25 dec").as_deref(), Some("64 days"));
        assert_eq!(v("days since 2026-01-01").as_deref(), Some("294 days"));
        assert_eq!(v("2026-12-25 - 2026-10-22").as_deref(), Some("64 days"));
        assert_eq!(v("2026-12-25 - today").as_deref(), Some("64 days"));
        assert_eq!(v("dec 25").as_deref(), Some("Friday, 25 December 2026"));
        assert_eq!(v("1 jan").as_deref(), Some("Friday, 1 January 2027"));
        assert_eq!(v("tomorrow").as_deref(), Some("Friday, 23 October 2026"));
        assert_eq!(v("notepad"), None);
        assert_eq!(v("2+2"), None);
    }

    #[test]
    fn time_zones() {
        // 10:00 UTC is 3:30 pm in India.
        let now = Utc.with_ymd_and_hms(2026, 10, 22, 10, 0, 0).unwrap();
        let local = chrono_tz::Asia::Kolkata;
        let v = |t: &str| zones(t, now, &local).map(|a| a.value);
        assert_eq!(v("time in tokyo").as_deref(), Some("7:00 PM"));
        assert_eq!(v("london time").as_deref(), Some("11:00 AM"));
        assert_eq!(v("3pm ist to pst").as_deref(), Some("2:30 AM"));
        assert_eq!(v("9am in new york").as_deref(), Some("11:30 PM"));
        assert_eq!(v("15:00 Europe/Paris to UTC").as_deref(), Some("1:00 PM"));
        let note = zones("time in tokyo", now, &local).unwrap().note;
        assert_eq!(note, "Thu 22 Oct · Tokyo (JST, UTC+09:00)");
        assert_eq!(v("5 km to miles"), None);
        assert_eq!(v("files in downloads"), None);
    }
}
