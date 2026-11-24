//! Integer arithmetic for the vesting curve.

/// `floor(a * b / c)` computed exactly with a 192-bit intermediate.
///
/// `a * b` is formed as three 64-bit limbs and divided by `c` limb by limb (schoolbook division by a one-limb
/// divisor), so the product never overflows. Every use in this crate has `b <= c`, which makes the result at most
/// `a`.
///
/// Returns `None` when `c == 0`, or when the quotient does not fit in a `u128` (possible only with `b > c`).
#[must_use]
pub const fn mul_div_floor(a: u128, b: u64, c: u64) -> Option<u128> {
    if c == 0 {
        return None;
    }
    let (a_hi, a_lo) = ((a >> 64) as u64, a as u64);
    let b = b as u128;
    // a * b = p2 * 2^128 + p1 * 2^64 + p0. Each partial product is below 2^128 - 2^65 + 1, so adding a carry
    // below 2^64 cannot overflow.
    let low = (a_lo as u128) * b;
    let high = (a_hi as u128) * b + (low >> 64);
    let limbs = [(high >> 64) as u64, high as u64, low as u64];

    let c = c as u128;
    let mut rem: u128 = 0;
    let mut quotient = [0u64; 3];
    let mut i = 0;
    while i < 3 {
        // rem < c <= 2^64 - 1, so (rem << 64) | limb fits in a u128 and the digit fits in a u64.
        let cur = (rem << 64) | limbs[i] as u128;
        quotient[i] = (cur / c) as u64;
        rem = cur % c;
        i += 1;
    }
    if quotient[0] != 0 {
        return None;
    }
    Some(((quotient[1] as u128) << 64) | quotient[2] as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_match_plain_arithmetic() {
        assert_eq!(mul_div_floor(10, 3, 4), Some(7));
        assert_eq!(mul_div_floor(0, 3, 4), Some(0));
        assert_eq!(mul_div_floor(10, 0, 4), Some(0));
        assert_eq!(mul_div_floor(10, 4, 4), Some(10));
        assert_eq!(mul_div_floor(1, 1, 3), Some(0));
    }

    #[test]
    fn zero_divisor_is_none() {
        assert_eq!(mul_div_floor(10, 3, 0), None);
    }

    #[test]
    fn max_amount_times_max_time_does_not_overflow() {
        assert_eq!(mul_div_floor(u128::MAX, u64::MAX, u64::MAX), Some(u128::MAX));
        assert_eq!(
            mul_div_floor(u128::MAX, u64::MAX - 1, u64::MAX),
            Some(u128::MAX - (u128::MAX / u64::MAX as u128))
        );
        assert_eq!(
            mul_div_floor(u128::MAX, 1, u64::MAX),
            Some(u128::MAX / u64::MAX as u128)
        );
    }

    #[test]
    fn quotient_above_u128_is_none() {
        assert_eq!(mul_div_floor(u128::MAX, 2, 1), None);
        assert_eq!(mul_div_floor(u128::MAX, 1, 1), Some(u128::MAX));
    }

    #[test]
    fn is_usable_in_const_context() {
        const HALF: Option<u128> = mul_div_floor(1_000, 1, 2);
        assert_eq!(HALF, Some(500));
    }
}
