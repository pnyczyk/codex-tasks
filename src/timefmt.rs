use chrono::{DateTime, Local, Utc};

/// Formats a timestamp in the classic Unix `ls -l` style (e.g. `Oct 12 10:01`).
pub fn format_unix_style(datetime: DateTime<Utc>) -> String {
    datetime
        .with_timezone(&Local)
        .format("%b %e %H:%M")
        .to_string()
}
