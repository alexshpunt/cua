// SPDX-License-Identifier: FSL-1.1-MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Resize Windows stream pixels without changing the native input geometry.

use std::sync::Arc;

use cua_spacesd_provider_api::{CaptureEvent, CaptureSink, OwnedFrame, ProviderError};
use cua_spacesd_session::media::encoder::downscale_bgra;

pub(super) fn resize_bgra(
    frame: OwnedFrame,
    (width, height): (u32, u32),
) -> Result<OwnedFrame, ProviderError> {
    frame.validate()?;
    let stride = frame.width_px * 4;
    Ok(OwnedFrame {
        bytes: downscale_bgra(
            &frame.bytes,
            frame.width_px,
            frame.height_px,
            stride,
            width,
            height,
        )
        .into(),
        width_px: width,
        height_px: height,
        bytes_per_row: Some(width * 4),
        ..frame
    })
}

fn fit_bgra(width: u32, height: u32, cap: u32) -> (u32, u32) {
    let long = width.max(height);
    if cap == 0 || long <= cap {
        return (width, height);
    }
    let scale = |size| ((u64::from(size) * u64::from(cap) / u64::from(long)) as u32).max(1);
    (scale(width), scale(height))
}

pub(super) fn cap_bgra(cap: u32, sink: Arc<dyn CaptureSink>) -> Arc<dyn CaptureSink> {
    if cap == 0 {
        sink
    } else {
        Arc::new(CappedBgraSink { cap, sink })
    }
}

struct CappedBgraSink {
    cap: u32,
    sink: Arc<dyn CaptureSink>,
}

impl CaptureSink for CappedBgraSink {
    fn on_event(&self, event: CaptureEvent) {
        let event = match event {
            CaptureEvent::GeometryChanged(mut geometry) => {
                (geometry.width_px, geometry.height_px) =
                    fit_bgra(geometry.width_px, geometry.height_px, self.cap);
                CaptureEvent::GeometryChanged(geometry)
            }
            CaptureEvent::Frame(frame) if frame.width_px.max(frame.height_px) > self.cap => {
                let dimensions = fit_bgra(frame.width_px, frame.height_px, self.cap);
                match resize_bgra(frame, dimensions) {
                    Ok(frame) => CaptureEvent::Frame(frame),
                    Err(error) => CaptureEvent::Suspended(error.to_string()),
                }
            }
            other => other,
        };
        self.sink.on_event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cua_spacesd_provider_api::PixelFormat;
    use std::sync::mpsc::{channel, Sender};

    struct Sink(Sender<CaptureEvent>);
    impl CaptureSink for Sink {
        fn on_event(&self, event: CaptureEvent) {
            self.0.send(event).unwrap();
        }
    }

    #[test]
    fn bgra_cap_resamples_the_frame_and_updates_its_geometry() {
        let (sender, receiver) = channel();
        let pixels: Vec<u8> = (0..15).flat_map(|i| [i, 0, 0, 255]).collect();
        let frame = OwnedFrame {
            bytes: pixels.into(),
            format: PixelFormat::Bgra8,
            width_px: 5,
            height_px: 3,
            bytes_per_row: Some(20),
            capture_timestamp_us: 7,
            encode_duration_us: None,
            codec_epoch: 1,
            keyframe: true,
        };
        let sink = Arc::new(Sink(sender));
        cap_bgra(0, sink.clone()).on_event(CaptureEvent::Frame(frame.clone()));
        let CaptureEvent::Frame(native) = receiver.try_recv().unwrap() else {
            panic!("expected native frame");
        };
        assert!(Arc::ptr_eq(&native.bytes, &frame.bytes));
        assert_eq!((native.width_px, native.height_px), (5, 3));

        cap_bgra(4, sink).on_event(CaptureEvent::Frame(frame));
        let CaptureEvent::Frame(scaled) = receiver.try_recv().unwrap() else {
            panic!("expected scaled frame");
        };
        scaled.validate().unwrap();
        assert_eq!((scaled.width_px, scaled.height_px), (4, 2));
        assert_eq!(scaled.capture_timestamp_us, 7);
        assert_eq!(
            scaled.bytes.iter().step_by(4).copied().collect::<Vec<_>>(),
            [0, 1, 2, 3, 5, 6, 7, 8]
        );
    }
}
