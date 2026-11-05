use holtmail_core::Timestamp;

/// A UTC calendar time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 is Sunday.
    pub weekday: u32,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub(crate) fn civil(at: Timestamp) -> Civil {
    let secs = at.as_millis().div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month: month as u32,
        day: day as u32,
        hour: (rem / 3600) as u32,
        minute: (rem % 3600 / 60) as u32,
        second: (rem % 60) as u32,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

impl Civil {
    /// `Sat Jan  3 01:05:34 1996`, as mbox separator lines carry it.
    pub(crate) fn asctime(&self) -> String {
        format!(
            "{} {} {:>2} {:02}:{:02}:{:02} {}",
            DAYS[self.weekday as usize],
            MONTHS[self.month as usize - 1],
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.year
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_to_calendar_time() {
        let c = civil(Timestamp::from_secs(820_631_134));
        assert_eq!(c.asctime(), "Wed Jan  3 01:05:34 1996");
        assert_eq!(
            civil(Timestamp::from_secs(1_741_942_800)).asctime(),
            "Fri Mar 14 09:00:00 2025"
        );
        assert_eq!(
            civil(Timestamp::from_secs(951_782_400)).asctime(),
            "Tue Feb 29 00:00:00 2000"
        );
        assert_eq!(
            civil(Timestamp::from_secs(-1)).asctime(),
            "Wed Dec 31 23:59:59 1969"
        );
    }
}
