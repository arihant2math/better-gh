//! Fractional indexing for manual item order.
//!
//! Keys are non-empty strings over the base-62 alphabet `0-9A-Za-z`, compared
//! bytewise (the column uses `COLLATE "C"`), and never end in `0`, so a key
//! strictly between any two distinct keys always exists. The web client
//! implements the same algorithm (`web/src/sync/fractional.ts`).

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const MAX_LEN: usize = 128;

fn digit(b: u8) -> Option<usize> {
    DIGITS.iter().position(|d| *d == b)
}

/// Whether `key` is a valid position key.
pub fn is_valid(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_LEN
        && key.bytes().all(|b| digit(b).is_some())
        && !key.ends_with('0')
}

/// Midpoint of `a < b` where `a` may be empty and `b == None` means +∞.
fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        // Common prefix (treating missing digits of `a` as '0').
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().and_then(|c| digit(*c)).unwrap_or(0);
    let db = b
        .and_then(|b| b.first())
        .and_then(|c| digit(*c))
        .unwrap_or(DIGITS.len());
    if db - da > 1 {
        vec![DIGITS[(da + db).div_ceil(2)]]
    } else if let Some(b) = b.filter(|b| b.len() > 1) {
        vec![b[0]]
    } else {
        let mut out = vec![DIGITS[da]];
        out.extend(midpoint(a.get(1..).unwrap_or(&[]), None));
        out
    }
}

/// A key strictly between `a` and `b` (`None` = open end). `a < b` required.
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("").as_bytes();
    let b = b.map(str::as_bytes);
    String::from_utf8(midpoint(a, b)).expect("ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_ordered_keys() {
        let first = between(None, None);
        assert_eq!(first, "V");
        let mut keys = vec![first];
        for _ in 0..200 {
            let k = between(keys.last().map(String::as_str), None);
            assert!(k > *keys.last().unwrap());
            assert!(is_valid(&k), "{k}");
            keys.push(k);
        }
        for _ in 0..100 {
            let k = between(None, Some(&keys[0]));
            assert!(k < keys[0] && is_valid(&k), "{k}");
            keys.insert(0, k);
        }
        // Repeated bisection between two neighbours.
        let (mut lo, hi) = (keys[10].clone(), keys[11].clone());
        for _ in 0..100 {
            let k = between(Some(&lo), Some(&hi));
            assert!(lo < k && k < hi && is_valid(&k), "{lo} {k} {hi}");
            lo = k;
        }
    }

    #[test]
    fn validates() {
        assert!(is_valid("a1"));
        assert!(!is_valid("a0"));
        assert!(!is_valid(""));
        assert!(!is_valid("a-b"));
        assert_eq!(between(Some("a"), Some("b")), "aV");
        assert_eq!(between(Some("a1"), Some("a2")), "a1V");
    }
}
