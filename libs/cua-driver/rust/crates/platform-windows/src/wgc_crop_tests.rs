use super::*;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::json;
use std::io::Cursor;

fn target() -> Target {
    Target {
        session: "owned".into(),
        pid: 42,
        window: 7,
    }
}
#[test]
fn readonly_crop_requires_an_explicit_target_and_dispatch_owned_session() {
    let valid = json!({"pid":42,"window_id":7,"_session_id":"owned","x1":0,"y1":0,"x2":4,"y2":3});
    let (target, region) = request(&valid).unwrap();
    assert_eq!(
        (target.pid, target.window, target.session.as_str()),
        (42, 7, "owned")
    );
    assert_eq!(
        region,
        Region {
            x: 0,
            y: 0,
            width: 4,
            height: 3
        }
    );
    for (field, value, code) in [
        ("pid", json!(null), "wgc_crop_explicit_pid_required"),
        ("pid", json!(0), "wgc_crop_explicit_pid_required"),
        (
            "pid",
            json!(2147483648_u64),
            "wgc_crop_explicit_pid_required",
        ),
        ("window_id", json!(0), "wgc_crop_explicit_window_required"),
        ("_session_id", json!(""), "wgc_runtime_session_required"),
        ("_session_id", json!(null), "wgc_runtime_session_required"),
    ] {
        let mut args = valid.clone();
        args[field] = value;
        assert_eq!(request(&args).unwrap_err(), code);
    }
    let mut untrusted = valid.clone();
    untrusted.as_object_mut().unwrap().remove("_session_id");
    untrusted["session"] = json!("owned");
    assert_eq!(
        request(&untrusted).unwrap_err(),
        "wgc_runtime_session_required"
    );
}
fn args(x1: Value, y1: Value, x2: Value, y2: Value) -> Value {
    json!({"x1":x1,"y1":y1,"x2":x2,"y2":y2})
}
fn frame(x: i32, y: i32) -> Frame {
    let image = RgbaImage::from_fn(8, 6, |x, y| Rgba([x as u8, y as u8, (x + y) as u8, 255]));
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    Frame {
        png: png.into_inner(),
        width: 8,
        height: 6,
        metadata: json!({
            "backend":"wgc", "paint_freshness":"unverified",
            "frame_bounds":{"x":x,"y":y,"width":8,"height":6},
            "resources_reused":true,"setup_ms":0.0,"frame_timestamp_100ns":99
        }),
    }
}

#[test]
fn integer_screen_edges_are_not_rounded_or_scaled() {
    assert_eq!(
        Region::from_args(&args(json!(-99), json!(22), json!(-95), json!(25))).unwrap(),
        Region {
            x: -99,
            y: 22,
            width: 4,
            height: 3
        }
    );
    for bad in [json!(1.5), json!(null), json!("1"), json!(i64::MAX)] {
        assert_eq!(
            Region::from_args(&args(bad, json!(0), json!(8), json!(4))).unwrap_err(),
            "wgc_crop_invalid_region"
        );
    }
    for value in [
        args(json!(4), json!(0), json!(4), json!(1)),
        args(json!(0), json!(3), json!(4), json!(2)),
        args(json!(i32::MIN), json!(0), json!(i32::MAX), json!(5)),
        args(json!(0), json!(0), json!(8192), json!(8192)),
    ] {
        assert_eq!(
            Region::from_args(&value).unwrap_err(),
            "wgc_crop_invalid_region"
        );
    }
}

#[test]
fn nonzero_and_negative_capture_origins_preserve_the_original_pixels() {
    for (x, y) in [(70, 61), (-100, -80)] {
        let original = frame(x, y);
        let source = image::load_from_memory(&original.png).unwrap().to_rgba8();
        let metadata = original.metadata.clone();
        let region = Region {
            x: x + 1,
            y: y + 2,
            width: 4,
            height: 3,
        };
        let result = crop(original, region, &target()).unwrap();
        let pixels = image::load_from_memory(&result.png).unwrap().to_rgba8();
        assert_eq!(pixels.dimensions(), (4, 3));
        for row in 0..3 {
            for col in 0..4 {
                assert_eq!(
                    pixels.get_pixel(col, row),
                    source.get_pixel(col + 1, row + 2)
                );
            }
        }
        assert_eq!(
            result.metadata,
            json!({
                "kind":"wgc_crop_v1","pid":42,"window_id":7,
                "coordinate_space":"screen_physical",
                "frame_bounds":{"x":x,"y":y,"width":8,"height":6},
                "source_dimensions":{"width":8,"height":6},
                "crop_bounds":{"x":x+1,"y":y+2,"width":4,"height":3},
                "crop_offset":{"x":1,"y":2},"width":4,"height":3,
                "format":"png","mime_type":"image/png","scale":1,"padding":0,
                "input_authority":false,"paint_freshness":"unverified","capture_metadata":metadata
            })
        );
    }
}

#[test]
fn a_crop_never_substitutes_or_clips_an_unmapped_capture() {
    let region = Region {
        x: 71,
        y: 63,
        width: 4,
        height: 3,
    };
    for (field, value) in [
        ("backend", json!("default")),
        ("paint_freshness", json!("verified")),
        ("frame_bounds", json!({"x":70,"y":61,"width":9,"height":6})),
        (
            "frame_bounds",
            json!({"x":70.5,"y":61,"width":8,"height":6}),
        ),
        ("scale", json!(2)),
    ] {
        let mut source = frame(70, 61);
        source.metadata[field] = value;
        assert_eq!(
            crop(source, region, &target()).unwrap_err(),
            "wgc_crop_frame_unmapped"
        );
    }
    let mut source = frame(70, 61);
    source.width = 4;
    assert_eq!(
        crop(source, region, &target()).unwrap_err(),
        "wgc_crop_frame_unmapped"
    );
    let mut source = frame(70, 61);
    source.width = 4;
    source.metadata["frame_bounds"]["width"] = json!(4);
    assert_eq!(
        crop(
            source,
            Region {
                x: 71,
                y: 63,
                width: 2,
                height: 3
            },
            &target()
        )
        .unwrap_err(),
        "wgc_crop_frame_unmapped"
    );
    let mut source = frame(70, 61);
    source.metadata["frame_bounds"] = json!({"x":70,"y":61,"unknown":8,"height":6});
    assert_eq!(
        crop(source, region, &target()).unwrap_err(),
        "wgc_crop_frame_unmapped"
    );
    let mut source = frame(70, 61);
    source.png = b"not png".to_vec();
    assert_eq!(
        crop(source, region, &target()).unwrap_err(),
        "wgc_crop_invalid_png"
    );
    for outside in [
        Region {
            x: 69,
            y: 61,
            width: 4,
            height: 3,
        },
        Region {
            x: 75,
            y: 61,
            width: 4,
            height: 3,
        },
        Region {
            x: 71,
            y: 65,
            width: 4,
            height: 3,
        },
        Region {
            x: 71,
            y: 61,
            width: 0,
            height: 1,
        },
    ] {
        assert_eq!(
            crop(frame(70, 61), outside, &target()).unwrap_err(),
            "wgc_crop_region_outside_frame"
        );
    }
}
