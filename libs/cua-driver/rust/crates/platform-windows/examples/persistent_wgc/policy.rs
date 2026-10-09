//! Decisions shared by the isolated probe and its hermetic tests.

/// windows-core 0.58 wraps a successful null interface as Error::empty (HRESULT 0).
/// A failing HRESULT is not an empty queue and must stop the probe.
pub(super) fn empty_frame_hresult(code: i32) -> bool {
    code == 0
}

/// Accept only a new compositor frame, not a cached answer from before this request.
pub(super) fn fresh_frame(frame: i64, request: i64, previous: i64) -> bool {
    frame > request && frame > previous
}

/// Convert QPC ticks to the 100ns clock used by WGC without intermediate overflow.
pub(super) fn qpc_100ns(ticks: i64, frequency: i64) -> Option<i64> {
    if ticks < 0 || frequency <= 0 {
        return None;
    }
    i64::try_from(i128::from(ticks) * 10_000_000 / i128::from(frequency)).ok()
}

/// Bound CPU reads to valid content, allocated texture extent and real row pitch.
pub(super) fn content_bytes(
    width: i32,
    height: i32,
    allocated_width: u32,
    allocated_height: u32,
    stride: u32,
) -> Option<usize> {
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if width == 0 || height == 0 || width > allocated_width || height > allocated_height {
        return None;
    }
    let row = width.checked_mul(4)?;
    if row > stride {
        return None;
    }
    let bytes = usize::try_from(row)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    (bytes <= 64 * 1024 * 1024).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_success_with_a_null_frame_is_an_empty_queue() {
        assert!(empty_frame_hresult(0));
        assert!(!empty_frame_hresult(0x80004003_u32 as i32)); // E_POINTER is a real failure.
        assert!(!empty_frame_hresult(0x887A0026_u32 as i32)); // DXGI access loss.
        assert!(!empty_frame_hresult(-1));
    }
    #[test]
    fn queued_or_repeated_frames_are_not_new_observations() {
        assert!(!fresh_frame(100, 100, 90));
        assert!(!fresh_frame(99, 100, 90));
        assert!(!fresh_frame(110, 100, 110));
        assert!(fresh_frame(111, 100, 110));
    }

    #[test]
    fn qpc_conversion_does_not_overflow_or_accept_an_invalid_clock() {
        assert_eq!(qpc_100ns(30, 3), Some(100_000_000));
        assert_eq!(qpc_100ns(i64::MAX, 10_000_000), Some(i64::MAX));
        assert_eq!(qpc_100ns(1, 0), None);
        assert_eq!(qpc_100ns(-1, 10), None);
        assert_eq!(qpc_100ns(i64::MAX, 1), None);
    }

    #[test]
    fn only_real_content_inside_the_allocated_texture_is_read() {
        assert_eq!(content_bytes(3, 2, 4, 4, 16), Some(24));
        assert_eq!(content_bytes(5, 2, 4, 4, 16), None);
        assert_eq!(content_bytes(3, 5, 4, 4, 16), None);
        assert_eq!(content_bytes(3, 2, 4, 4, 8), None);
        assert_eq!(content_bytes(0, 2, 4, 4, 16), None);
        assert_eq!(content_bytes(-1, 2, 4, 4, 16), None);
        assert_eq!(content_bytes(8192, 8192, 8192, 8192, 32768), None);
    }
}
