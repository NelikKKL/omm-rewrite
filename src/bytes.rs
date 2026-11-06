//! Безопасное чтение little-endian чисел из байтового среза.
//!
//! Ни одна функция не паникует: выход за границы даёт `None`.

fn take<const N: usize>(b: &[u8], off: usize) -> Option<[u8; N]> {
    let end = off.checked_add(N)?;
    b.get(off..end)?.try_into().ok()
}

pub fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    take::<2>(b, off).map(u16::from_le_bytes)
}

pub fn i16_at(b: &[u8], off: usize) -> Option<i16> {
    take::<2>(b, off).map(i16::from_le_bytes)
}

pub fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    take::<4>(b, off).map(u32::from_le_bytes)
}

pub fn f32_at(b: &[u8], off: usize) -> Option<f32> {
    take::<4>(b, off).map(f32::from_le_bytes)
}

pub fn f64_at(b: &[u8], off: usize) -> Option<f64> {
    take::<8>(b, off).map(f64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_bounds() {
        let b = [1u8, 0, 0, 0, 0xFF, 0xFF];
        assert_eq!(u32_at(&b, 0), Some(1));
        assert_eq!(u16_at(&b, 4), Some(0xFFFF));
        assert_eq!(i16_at(&b, 4), Some(-1));
        assert_eq!(u32_at(&b, 3), None);
        assert_eq!(u32_at(&b, usize::MAX), None);
        assert_eq!(f32_at(&b, 100), None);
        assert_eq!(f64_at(&1.5f64.to_le_bytes(), 0), Some(1.5));
        assert_eq!(f64_at(&b, 0), None);
    }
}
