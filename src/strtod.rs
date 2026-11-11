//! glibc `strtod()` in the C locale, for `ratelimit = "N/unit"`.
//!
//! What matters for metalog is where parsing stops (the `/unit` must follow
//! immediately) and the value.  Decimal input is rounded correctly by Rust's
//! float parser, like glibc's; hex floats are accumulated exactly up to 64
//! significant bits, which covers anything a config file would contain.

use crate::num::is_c_space;

/// `strtod(s, &end)`: (value, end offset); end is 0 if nothing converted.
pub fn strtod(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && is_c_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let sign = |v: f64| if neg { -v } else { v };
    let rest = &s[i..];
    let starts = |p: &[u8]| rest.len() >= p.len() && rest[..p.len()].eq_ignore_ascii_case(p);

    if starts(b"infinity") {
        return (sign(f64::INFINITY), i + 8);
    }
    if starts(b"inf") {
        return (sign(f64::INFINITY), i + 3);
    }
    if starts(b"nan") {
        let mut j = i + 3;
        // "nan(n-char-sequence)" is consumed only if the ')' is there.
        if s.get(j) == Some(&b'(') {
            let mut k = j + 1;
            while k < s.len() && (s[k].is_ascii_alphanumeric() || s[k] == b'_') {
                k += 1;
            }
            if s.get(k) == Some(&b')') {
                j = k + 1;
            }
        }
        return (sign(f64::NAN), j);
    }

    if rest.len() >= 2 && rest[0] == b'0' && (rest[1] | 0x20) == b'x' {
        if let Some((v, n)) = hex(&rest[2..]) {
            return (sign(v), i + 2 + n);
        }
        // "0x" without hex digits: the "0" converts and parsing stops at 'x'.
        return (sign(0.0), i + 1);
    }

    // Decimal: digits [. digits] [e [sign] digits]
    let mut j = i;
    let mut digits = 0;
    while j < s.len() && s[j].is_ascii_digit() {
        j += 1;
        digits += 1;
    }
    if j < s.len() && s[j] == b'.' {
        j += 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return (0.0, 0);
    }
    let mant_end = j;
    if j < s.len() && (s[j] | 0x20) == b'e' {
        let mut k = j + 1;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            k += 1;
        }
        if k < s.len() && s[k].is_ascii_digit() {
            while k < s.len() && s[k].is_ascii_digit() {
                k += 1;
            }
            j = k;
        }
    }
    let _ = mant_end;
    let text = std::str::from_utf8(&s[i..j]).expect("ASCII digits");
    // Rust's parser wants a digit before an exponent and accepts "1." and
    // ".5", which is what remains to handle.
    let v: f64 = text.parse().unwrap_or(0.0);
    (sign(v), j)
}

/// Hex float body after "0x": (value, bytes consumed), `None` if no digits.
fn hex(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    let mut mant: u64 = 0;
    let mut exp: i64 = 0;
    let mut any = false;
    let mut lost = false;
    let hexval = |b: u8| (b as char).to_digit(16);
    while i < s.len() {
        let Some(d) = hexval(s[i]) else { break };
        any = true;
        if mant >> 60 == 0 {
            mant = mant * 16 + u64::from(d);
        } else {
            exp += 4;
            lost |= d != 0;
        }
        i += 1;
    }
    if i < s.len() && s[i] == b'.' {
        let mut k = i + 1;
        let mut frac_any = false;
        while k < s.len() {
            let Some(d) = hexval(s[k]) else { break };
            frac_any = true;
            if mant >> 60 == 0 {
                mant = mant * 16 + u64::from(d);
                exp -= 4;
            } else {
                lost |= d != 0;
            }
            k += 1;
        }
        if any || frac_any {
            any = true;
            i = k;
        }
    }
    if !any {
        return None;
    }
    if i < s.len() && (s[i] | 0x20) == b'p' {
        let mut k = i + 1;
        let mut eneg = false;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            eneg = s[k] == b'-';
            k += 1;
        }
        if k < s.len() && s[k].is_ascii_digit() {
            let mut e: i64 = 0;
            while k < s.len() && s[k].is_ascii_digit() {
                e = (e * 10 + i64::from(s[k] - b'0')).min(1 << 20);
                k += 1;
            }
            exp += if eneg { -e } else { e };
            i = k;
        }
    }
    let _ = lost;
    let v = (mant as f64) * 2f64.powi(exp.clamp(-2000, 2000) as i32);
    Some((v, i))
}

/// The x86-64 `cvttsd2si` conversion C compiles `(int64_t) double` to:
/// truncation, with NaN and out-of-range values giving `INT64_MIN`.
pub fn cvttsd2si(v: f64) -> i64 {
    if v.is_nan() || v >= 9_223_372_036_854_775_808.0 || v < -9_223_372_036_854_775_808.0 { i64::MIN } else { v as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal() {
        assert_eq!(strtod(b"10/s"), (10.0, 2));
        assert_eq!(strtod(b" 2.5/m"), (2.5, 4));
        assert_eq!(strtod(b"1e3/h"), (1000.0, 3));
        assert_eq!(strtod(b"1e/h"), (1.0, 1));
        assert_eq!(strtod(b".5"), (0.5, 2));
        assert_eq!(strtod(b"5."), (5.0, 2));
        assert_eq!(strtod(b"-3"), (-3.0, 2));
        assert_eq!(strtod(b"abc").1, 0);
        assert_eq!(strtod(b".").1, 0);
        assert_eq!(strtod(b"").1, 0);
    }

    #[test]
    fn special() {
        assert_eq!(strtod(b"inf/s"), (f64::INFINITY, 3));
        assert_eq!(strtod(b"-Infinity"), (f64::NEG_INFINITY, 9));
        let (v, e) = strtod(b"nan(12)x");
        assert!(v.is_nan());
        assert_eq!(e, 7);
        assert_eq!(strtod(b"nan(").1, 3);
        assert_eq!(strtod(b"0x10/s"), (16.0, 4));
        assert_eq!(strtod(b"0x1p4"), (16.0, 5));
        assert_eq!(strtod(b"0x.8"), (0.5, 4));
        assert_eq!(strtod(b"0xg"), (0.0, 1));
    }

    #[test]
    fn conversion() {
        assert_eq!(cvttsd2si(1.9), 1);
        assert_eq!(cvttsd2si(f64::NAN), i64::MIN);
        assert_eq!(cvttsd2si(1e300), i64::MIN);
        assert_eq!(cvttsd2si(-0.0), 0);
    }
}
