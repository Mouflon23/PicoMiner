//! Compact number formatting for the 128x64 screen.

use core::fmt::{self, Display, Formatter};

/// Prints a value with a K/M/G/T/P suffix and three significant digits.
pub struct Si(pub f64);

impl Display for Si {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut value = self.0;
        if value < 0.01 {
            return write!(f, "{value:.5}");
        }
        if value < 1.0 {
            return write!(f, "{value:.3}");
        }
        let mut suffix = "";
        for next in ["K", "M", "G", "T", "P", "E"] {
            if value < 1000.0 {
                break;
            }
            value /= 1000.0;
            suffix = next;
        }
        if value < 10.0 {
            write!(f, "{value:.2}{suffix}")
        } else if value < 100.0 {
            write!(f, "{value:.1}{suffix}")
        } else {
            write!(f, "{value:.0}{suffix}")
        }
    }
}

/// Prints seconds as `3d 04:05` or `04:05:06`.
pub struct Uptime(pub u32);

impl Display for Uptime {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let (days, hours) = (self.0 / 86_400, self.0 / 3600 % 24);
        let (minutes, seconds) = (self.0 / 60 % 60, self.0 % 60);
        if days > 0 {
            write!(f, "{days}d {hours:02}:{minutes:02}")
        } else {
            write!(f, "{hours:02}:{minutes:02}:{seconds:02}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si_suffixes() {
        assert_eq!(format!("{}", Si(0.00015)), "0.00015");
        assert_eq!(format!("{}", Si(0.5)), "0.500");
        assert_eq!(format!("{}", Si(1.0)), "1.00");
        assert_eq!(format!("{}", Si(42.5)), "42.5");
        assert_eq!(format!("{}", Si(1234.0)), "1.23K");
        assert_eq!(format!("{}", Si(987_654.0)), "988K");
        assert_eq!(format!("{}", Si(2.5e12)), "2.50T");
    }

    #[test]
    fn uptime() {
        assert_eq!(format!("{}", Uptime(0)), "00:00:00");
        assert_eq!(format!("{}", Uptime(3725)), "01:02:05");
        assert_eq!(format!("{}", Uptime(3 * 86_400 + 4 * 3600 + 300)), "3d 04:05");
    }
}
