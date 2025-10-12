use chrono::{DateTime, Local, SecondsFormat, Utc};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum TimeFormat {
    Human,
    Iso,
}

pub fn format_time(datetime: DateTime<Utc>, format: TimeFormat) -> String {
    match format {
        TimeFormat::Human => format_human(datetime),
        TimeFormat::Iso => format_iso(datetime),
    }
}

fn format_human(datetime: DateTime<Utc>) -> String {
    let local = datetime.with_timezone(&Local);
    local.format("%a %b %e %H:%M:%S %Y").to_string()
}

fn format_iso(datetime: DateTime<Utc>) -> String {
    datetime
        .with_timezone(&Local)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}
