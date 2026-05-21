//! Binance ticker JSON parser. Extracts the `"price"` field and converts
//! the dollar.cents string to total cents as a `u32`.
//!
//! Metrics: `../../../docs/METRICS.md`.

/// Parse BTC price in cents from a Binance ticker JSON response.
///
/// The input is the full HTTP response including headers. Looks for the
/// `"price":"` marker in the body and extracts an integer dollar.cents
/// value, returning `dollars * 100 + cents` as a `u32`.
///
/// Returns `None` if the price field is not found or malformed.
#[must_use]
pub fn parse_price_cents(response: &[u8]) -> Option<u32> {
    let marker = b"\"price\":\"";
    let start = find_subsequence(response, marker)? + marker.len();

    let mut dollars: u32 = 0;
    let mut index = start;

    // Parse digits before the decimal point.
    while index < response.len() && response[index].is_ascii_digit() {
        let digit = u32::from(response[index] - b'0');
        dollars = dollars.saturating_mul(10).saturating_add(digit);
        index += 1;
    }

    // If no decimal point, return dollars only.
    if index >= response.len() || response[index] != b'.' {
        return Some(dollars.saturating_mul(100));
    }
    index += 1;

    // Parse up to two digits after the decimal point.
    let mut cents: u32 = 0;
    let mut decimal_digits: u32 = 0;
    while index < response.len() && response[index].is_ascii_digit() && decimal_digits < 2 {
        let digit = u32::from(response[index] - b'0');
        cents = cents.saturating_mul(10).saturating_add(digit);
        decimal_digits += 1;
        index += 1;
    }

    // Pad if only one decimal digit. A value like "3.5" means 50 cents, not 5.
    if decimal_digits == 1 {
        cents = cents.saturating_mul(10);
    }

    Some(dollars.saturating_mul(100).saturating_add(cents))
}

/// Find the starting index of `needle` within `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
#[path = "tests/btc.rs"]
mod tests;
