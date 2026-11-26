//! Conversion of core values to driver parameters.

use i3cm_core::values::{Col, Val};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use tokio_postgres::types::ToSql;

pub type Param = Box<dyn ToSql + Sync + Send>;

/// A typed parameter; NULLs need the column class so that the driver accepts them.
pub fn param(v: &Val, col: Col, list: bool) -> Param {
    match v {
        Val::Null => null(col, list),
        Val::Text(s) => Box::new(s.clone()),
        Val::Bool(b) => Box::new(*b),
        Val::Int(i) => Box::new(*i),
        Val::Num(d) => Box::new(*d),
        Val::Date(d) => Box::new(*d),
        Val::Ts(t) => Box::new(*t),
        Val::Time(t) => Box::new(*t),
        Val::Json(j) => Box::new(j.clone()),
        Val::TextArr(a) => Box::new(a.clone()),
        Val::BoolArr(a) => Box::new(a.clone()),
        Val::IntArr(a) => Box::new(a.clone()),
        Val::NumArr(a) => Box::new(a.clone()),
        Val::DateArr(a) => Box::new(a.clone()),
        Val::TsArr(a) => Box::new(a.clone()),
        Val::TimeArr(a) => Box::new(a.clone()),
    }
}

fn null(col: Col, list: bool) -> Param {
    match (col, list) {
        (Col::Json, _) => Box::new(None::<Value>),
        (Col::Text, false) => Box::new(None::<String>),
        (Col::Bool, false) => Box::new(None::<bool>),
        (Col::Int, false) => Box::new(None::<i64>),
        (Col::Num, false) => Box::new(None::<Decimal>),
        (Col::Date, false) => Box::new(None::<NaiveDate>),
        (Col::Ts, false) => Box::new(None::<DateTime<Utc>>),
        (Col::Time, false) => Box::new(None::<NaiveTime>),
        (Col::Text, true) => Box::new(None::<Vec<String>>),
        (Col::Bool, true) => Box::new(None::<Vec<bool>>),
        (Col::Int, true) => Box::new(None::<Vec<i64>>),
        (Col::Num, true) => Box::new(None::<Vec<Decimal>>),
        (Col::Date, true) => Box::new(None::<Vec<NaiveDate>>),
        (Col::Ts, true) => Box::new(None::<Vec<DateTime<Utc>>>),
        (Col::Time, true) => Box::new(None::<Vec<NaiveTime>>),
    }
}

pub fn refs(params: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect()
}
