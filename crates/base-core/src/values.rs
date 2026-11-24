//! Value types: validation of JSON input and mapping to storage values.

use crate::catalog::{AttrDef, ValueType};
use crate::error::{BaseError, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

/// A storage value, independent of the database driver.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Text(String),
    Bool(bool),
    Int(i64),
    Num(Decimal),
    Date(NaiveDate),
    Ts(DateTime<Utc>),
    Time(NaiveTime),
    Json(Value),
    TextArr(Vec<String>),
    BoolArr(Vec<bool>),
    IntArr(Vec<i64>),
    NumArr(Vec<Decimal>),
    DateArr(Vec<NaiveDate>),
    TsArr(Vec<DateTime<Utc>>),
    TimeArr(Vec<NaiveTime>),
}

/// Storage class of a value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Text,
    Bool,
    Int,
    Num,
    Date,
    Ts,
    Time,
    Json,
}

impl Col {
    pub fn of(vt: &ValueType) -> Col {
        match vt {
            ValueType::String | ValueType::Enum { .. } | ValueType::Formatted { .. } | ValueType::Duration | ValueType::Recurrence => Col::Text,
            ValueType::Boolean => Col::Bool,
            ValueType::Int => Col::Int,
            ValueType::Decimal { .. } => Col::Num,
            ValueType::Date => Col::Date,
            ValueType::Datetime => Col::Ts,
            ValueType::Time => Col::Time,
            ValueType::Text { .. } | ValueType::Money | ValueType::Struct { .. } => Col::Json,
        }
    }
    /// SQL column type; lists of JSON values are stored as one jsonb array.
    pub fn sql(&self, list: bool) -> &'static str {
        match (self, list) {
            (Col::Text, false) => "text",
            (Col::Bool, false) => "boolean",
            (Col::Int, false) => "bigint",
            (Col::Num, false) => "numeric",
            (Col::Date, false) => "date",
            (Col::Ts, false) => "timestamptz",
            (Col::Time, false) => "time",
            (Col::Json, _) => "jsonb",
            (Col::Text, true) => "text[]",
            (Col::Bool, true) => "boolean[]",
            (Col::Int, true) => "bigint[]",
            (Col::Num, true) => "numeric[]",
            (Col::Date, true) => "date[]",
            (Col::Ts, true) => "timestamptz[]",
            (Col::Time, true) => "time[]",
        }
    }
}

fn bad(attr: &str, why: &str) -> BaseError {
    BaseError::invalid("value.invalid", "value does not match the attribute type").with("attribute", attr).with("reason", why)
}

/// Validate and normalize a single (non-list) JSON value.
pub fn normalize(vt: &ValueType, attr: &str, v: &Value) -> Result<Value> {
    let s = || v.as_str().ok_or_else(|| bad(attr, "string expected"));
    Ok(match vt {
        ValueType::String => Value::String(s()?.to_string()),
        ValueType::Boolean => Value::Bool(v.as_bool().ok_or_else(|| bad(attr, "boolean expected"))?),
        ValueType::Int => json!(v.as_i64().ok_or_else(|| bad(attr, "integer expected"))?),
        ValueType::Decimal { scale, .. } => {
            let d = decimal_of(v).ok_or_else(|| bad(attr, "decimal expected"))?;
            if d.scale() > *scale {
                return Err(bad(attr, "too many decimal places"));
            }
            Value::String(d.to_string())
        }
        ValueType::Text { formats } => {
            let (format, content) = match v {
                Value::String(c) => (formats[0].clone(), c.clone()),
                Value::Object(o) => (
                    o.get("format").and_then(|x| x.as_str()).unwrap_or(&formats[0]).to_string(),
                    o.get("content").and_then(|x| x.as_str()).ok_or_else(|| bad(attr, "content missing"))?.to_string(),
                ),
                _ => return Err(bad(attr, "text expected")),
            };
            if !formats.contains(&format) {
                return Err(bad(attr, "text format not allowed"));
            }
            json!({"format": format, "content": content})
        }
        ValueType::Date => Value::String(NaiveDate::from_str(s()?).map_err(|_| bad(attr, "date YYYY-MM-DD expected"))?.to_string()),
        ValueType::Datetime => Value::String(
            DateTime::parse_from_rfc3339(s()?).map_err(|_| bad(attr, "RFC 3339 date-time expected"))?.with_timezone(&Utc).to_rfc3339(),
        ),
        ValueType::Time => Value::String(NaiveTime::from_str(s()?).map_err(|_| bad(attr, "time HH:MM[:SS] expected"))?.to_string()),
        ValueType::Duration => {
            let x = s()?;
            if !x.starts_with('P') || x.len() < 3 {
                return Err(bad(attr, "ISO 8601 duration expected"));
            }
            Value::String(x.to_string())
        }
        ValueType::Recurrence => {
            let x = s()?;
            if !x.split(';').any(|p| p.starts_with("FREQ=")) {
                return Err(bad(attr, "RRULE with FREQ expected"));
            }
            Value::String(x.to_string())
        }
        ValueType::Enum { values } => {
            let x = s()?;
            if !values.iter().any(|e| e == x) {
                return Err(bad(attr, "value not in enum"));
            }
            Value::String(x.to_string())
        }
        ValueType::Formatted { format } => {
            let x = s()?.trim().to_string();
            if !formatted_ok(format, &x) {
                return Err(bad(attr, "format check failed").with("format", format.clone()));
            }
            Value::String(x)
        }
        ValueType::Money => {
            let o = v.as_object().ok_or_else(|| bad(attr, "money {amount, currency} expected"))?;
            let amount = o.get("amount").and_then(decimal_of).ok_or_else(|| bad(attr, "amount expected"))?;
            let cur = o.get("currency").and_then(|c| c.as_str()).ok_or_else(|| bad(attr, "currency expected"))?;
            if cur.len() != 3 || !cur.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(bad(attr, "ISO 4217 currency code expected"));
            }
            json!({"amount": amount.to_string(), "currency": cur})
        }
        ValueType::Struct { fields } => {
            let o = v.as_object().ok_or_else(|| bad(attr, "object expected"))?;
            let mut out = serde_json::Map::new();
            for f in fields {
                match o.get(&f.name) {
                    Some(x) if !x.is_null() => {
                        out.insert(f.name.clone(), normalize(&f.vt, &format!("{attr}.{}", f.name), x)?);
                    }
                    _ if f.required => return Err(bad(attr, "struct field missing").with("field", f.name.clone())),
                    _ => {}
                }
            }
            if let Some(k) = o.keys().find(|k| !fields.iter().any(|f| &f.name == *k)) {
                return Err(bad(attr, "unknown struct field").with("field", k.clone()));
            }
            Value::Object(out)
        }
    })
}

fn decimal_of(v: &Value) -> Option<Decimal> {
    match v {
        Value::String(s) => Decimal::from_str(s).ok(),
        Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
        _ => None,
    }
}

fn formatted_ok(format: &str, x: &str) -> bool {
    match format {
        "email" => {
            let mut p = x.split('@');
            matches!((p.next(), p.next(), p.next()), (Some(a), Some(b), None) if !a.is_empty() && b.contains('.') && !x.contains(' '))
        }
        "url" => (x.starts_with("http://") || x.starts_with("https://")) && x.len() > 8 && !x.contains(' '),
        "phone" => x.starts_with('+') && x[1..].chars().all(|c| c.is_ascii_digit() || c == ' ' || c == '-') && x.len() >= 6,
        "iban" => iban_ok(x),
        _ => false,
    }
}

fn iban_ok(x: &str) -> bool {
    let s: String = x.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    if s.len() < 15 || s.len() > 34 || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let digits = if c.is_ascii_digit() { c.to_string() } else { (c as u32 - 'A' as u32 + 10).to_string() };
        for d in digits.chars() {
            rem = (rem * 10 + d.to_digit(10).unwrap()) % 97;
        }
    }
    rem == 1
}

/// Validate a JSON input for an attribute and convert it to a storage value.
pub fn to_val(a: &AttrDef, v: &Value) -> Result<Val> {
    if v.is_null() {
        if a.required {
            return Err(BaseError::invalid("value.required", "required attribute without value").with("attribute", a.name.clone()));
        }
        return Ok(Val::Null);
    }
    let col = Col::of(&a.vt);
    if a.list {
        let arr = v.as_array().ok_or_else(|| bad(&a.name, "list expected"))?;
        let norm: Vec<Value> = arr.iter().map(|x| normalize(&a.vt, &a.name, x)).collect::<Result<_>>()?;
        return Ok(match col {
            Col::Json => Val::Json(Value::Array(norm)),
            Col::Text => Val::TextArr(norm.iter().map(|x| x.as_str().unwrap().to_string()).collect()),
            Col::Bool => Val::BoolArr(norm.iter().map(|x| x.as_bool().unwrap()).collect()),
            Col::Int => Val::IntArr(norm.iter().map(|x| x.as_i64().unwrap()).collect()),
            Col::Num => Val::NumArr(norm.iter().map(|x| Decimal::from_str(x.as_str().unwrap()).unwrap()).collect()),
            Col::Date => Val::DateArr(norm.iter().map(|x| NaiveDate::from_str(x.as_str().unwrap()).unwrap()).collect()),
            Col::Ts => Val::TsArr(norm.iter().map(|x| parse_ts(x.as_str().unwrap())).collect()),
            Col::Time => Val::TimeArr(norm.iter().map(|x| NaiveTime::from_str(x.as_str().unwrap()).unwrap()).collect()),
        });
    }
    let n = normalize(&a.vt, &a.name, v)?;
    Ok(match col {
        Col::Json => Val::Json(n),
        Col::Text => Val::Text(n.as_str().unwrap().to_string()),
        Col::Bool => Val::Bool(n.as_bool().unwrap()),
        Col::Int => Val::Int(n.as_i64().unwrap()),
        Col::Num => Val::Num(Decimal::from_str(n.as_str().unwrap()).unwrap()),
        Col::Date => Val::Date(NaiveDate::from_str(n.as_str().unwrap()).unwrap()),
        Col::Ts => Val::Ts(parse_ts(n.as_str().unwrap())),
        Col::Time => Val::Time(NaiveTime::from_str(n.as_str().unwrap()).unwrap()),
    })
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)).unwrap_or_default()
}

/// Render a stored JSON value as plain text for display templates.
pub fn display(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(display).collect::<Vec<_>>().join(", "),
        Value::Object(o) => {
            if let (Some(a), Some(c)) = (o.get("amount"), o.get("currency")) {
                format!("{} {}", display(a), display(c))
            } else if let Some(c) = o.get("content") {
                display(c)
            } else {
                o.values().map(display).collect::<Vec<_>>().join(" ")
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::StructField;

    fn attr(vt: ValueType, required: bool, list: bool) -> AttrDef {
        AttrDef { name: "a".into(), vt, required, list, indexed: false, unique: false }
    }

    #[test]
    fn primitives() {
        assert_eq!(to_val(&attr(ValueType::Int, true, false), &json!(5)).unwrap(), Val::Int(5));
        assert!(to_val(&attr(ValueType::Int, true, false), &json!("5")).is_err());
        assert!(to_val(&attr(ValueType::String, true, false), &Value::Null).is_err());
        assert_eq!(to_val(&attr(ValueType::String, false, false), &Value::Null).unwrap(), Val::Null);
    }

    #[test]
    fn decimal_scale() {
        let a = attr(ValueType::Decimal { precision: 10, scale: 2 }, true, false);
        assert!(to_val(&a, &json!("1.23")).is_ok());
        assert!(to_val(&a, &json!("1.234")).is_err());
    }

    #[test]
    fn dates_and_lists() {
        let a = attr(ValueType::Date, true, true);
        let v = to_val(&a, &json!(["2026-01-31", "2026-02-01"])).unwrap();
        assert!(matches!(v, Val::DateArr(ref d) if d.len() == 2));
        assert!(to_val(&attr(ValueType::Date, true, false), &json!("31.01.2026")).is_err());
        let ts = normalize(&ValueType::Datetime, "a", &json!("2026-01-31T10:00:00+01:00")).unwrap();
        assert_eq!(ts, json!("2026-01-31T09:00:00+00:00"));
    }

    #[test]
    fn money_enum_formatted() {
        assert_eq!(normalize(&ValueType::Money, "a", &json!({"amount": 12.5, "currency": "EUR"})).unwrap(), json!({"amount": "12.5", "currency": "EUR"}));
        assert!(normalize(&ValueType::Money, "a", &json!({"amount": "1", "currency": "eur"})).is_err());
        let e = ValueType::Enum { values: vec!["open".into(), "done".into()] };
        assert!(normalize(&e, "a", &json!("open")).is_ok());
        assert!(normalize(&e, "a", &json!("x")).is_err());
        let f = |k: &str| ValueType::Formatted { format: k.into() };
        assert!(normalize(&f("email"), "a", &json!("a@b.de")).is_ok());
        assert!(normalize(&f("email"), "a", &json!("a@b")).is_err());
        assert!(normalize(&f("iban"), "a", &json!("DE89 3704 0044 0532 0130 00")).is_ok());
        assert!(normalize(&f("iban"), "a", &json!("DE89 3704 0044 0532 0130 01")).is_err());
    }

    #[test]
    fn text_struct_recurrence() {
        let t = ValueType::Text { formats: vec!["MD".into(), "HTML".into()] };
        assert_eq!(normalize(&t, "a", &json!("# x")).unwrap(), json!({"format": "MD", "content": "# x"}));
        assert!(normalize(&t, "a", &json!({"format": "TXT", "content": "x"})).is_err());
        let s = ValueType::Struct { fields: vec![StructField { name: "street".into(), vt: ValueType::String, required: true }] };
        assert!(normalize(&s, "a", &json!({"street": "Hauptstr."})).is_ok());
        assert!(normalize(&s, "a", &json!({})).is_err());
        assert!(normalize(&s, "a", &json!({"street": "x", "zip": "1"})).is_err());
        assert!(normalize(&ValueType::Recurrence, "a", &json!("FREQ=MONTHLY;BYDAY=1MO")).is_ok());
        assert!(normalize(&ValueType::Recurrence, "a", &json!("monthly")).is_err());
    }

    #[test]
    fn display_values() {
        assert_eq!(display(&json!({"amount": "12.50", "currency": "EUR"})), "12.50 EUR");
        assert_eq!(display(&json!(["a", "b"])), "a, b");
    }
}
