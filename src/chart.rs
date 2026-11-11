//! Axis ticks and number formatting shared by the SVG chart and the viewer.

/// 1-2-5 ticks inside `lo..=hi`, or 1..9 when the range is too narrow for that.
pub fn log_ticks(lo: f64, hi: f64) -> Vec<f64> {
    let ticks = |mantissas: &[f64]| -> Vec<f64> {
        (lo.log10().floor() as i32..=hi.log10().ceil() as i32)
            .flat_map(|e| mantissas.iter().map(move |m| m * 10f64.powi(e)))
            .filter(|v| (lo..=hi).contains(v))
            .collect()
    };
    let ticks_125 = ticks(&[1.0, 2.0, 5.0]);
    if ticks_125.len() >= 3 {
        ticks_125
    } else {
        ticks(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])
    }
}

/// Ratio with three significant digits and no trailing zeros (1, 0.5, 0.001).
pub fn format_ratio(v: &f64) -> String {
    if *v <= 0.0 {
        return "0".to_string();
    }
    let decimals = (2 - v.log10().floor() as i32).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Integer with thousands separators (axis labels are always non-negative counts).
pub fn format_count(v: &f64) -> String {
    let digits = format!("{:.0}", v.max(0.0));
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_ticks_adapt_to_range() {
        assert_eq!(log_ticks(0.9, 120.0).len(), 7); // 1 2 5 10 20 50 100
        assert_eq!(log_ticks(330.0, 650.0).len(), 3); // 400 500 600
    }

    #[test]
    fn formats_ratios() {
        assert_eq!(format_ratio(&1.0), "1");
        assert_eq!(format_ratio(&0.5), "0.5");
        assert_eq!(format_ratio(&0.001), "0.001");
        assert_eq!(format_ratio(&0.6259), "0.626");
        assert_eq!(format_ratio(&20.0), "20");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(&0.0), "0");
        assert_eq!(format_count(&999.0), "999");
        assert_eq!(format_count(&5398.4), "5,398");
        assert_eq!(format_count(&12345678.0), "12,345,678");
    }
}
