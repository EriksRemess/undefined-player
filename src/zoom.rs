//! Fill the viewport after black-border detection, without stretching video.

#[derive(Default)]
pub(crate) struct Zoom {
    saved_crop: Option<bool>,
}

impl Zoom {
    // Return the desired autocrop setting. Manual C changes during zoom are
    // temporary; leaving zoom restores the setting from before it was enabled.
    pub(crate) fn toggle(&mut self, cropping: bool) -> bool {
        if let Some(saved) = self.saved_crop.take() {
            saved
        } else {
            self.saved_crop = Some(cropping);
            true
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.saved_crop.is_some()
    }

    pub(crate) fn fill(&self, cropping: bool, crop_ready: bool, unavailable: bool) -> bool {
        self.enabled() && (!cropping || crop_ready || unavailable)
    }

    pub(crate) fn status(&self, fill: bool) -> &'static str {
        if fill {
            "FILL"
        } else if self.enabled() {
            "FILL (DETECTING)"
        } else {
            "FIT"
        }
    }
}

fn fill_crop(
    rect: [f32; 4],
    sar_num: i32,
    sar_den: i32,
    rotation: u32,
    width: i32,
    height: i32,
) -> [f32; 4] {
    if width <= 0 || height <= 0 || !rect.iter().all(|v| v.is_finite()) {
        return rect;
    }
    let [x0, y0, x1, y1] = rect.map(f64::from);
    let sw = (x1 - x0).abs();
    let sh = (y1 - y0).abs();
    if sw <= 0.0 || sh <= 0.0 {
        return rect;
    }
    let sar = if sar_num > 0 && sar_den > 0 {
        f64::from(sar_num) / f64::from(sar_den)
    } else {
        1.0
    };
    // Express the viewport ratio in unrotated source pixels, including SAR.
    let ratio = if rotation.is_multiple_of(2) {
        f64::from(width) / f64::from(height)
    } else {
        f64::from(height) / f64::from(width)
    } / sar;
    let cw = sw.min(sh * ratio).copysign(x1 - x0);
    let ch = sh.min(sw / ratio).copysign(y1 - y0);
    [
        ((x0 + x1 - cw) * 0.5) as f32,
        ((y0 + y1 - ch) * 0.5) as f32,
        ((x0 + x1 + cw) * 0.5) as f32,
        ((y0 + y1 + ch) * 0.5) as f32,
    ]
}

// The native adapter passes the libplacebo source rectangle after autocrop.
// SAFETY: rect must point to four writable floats for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn up_video_fill_crop(
    rect: *mut f32,
    sar_num: i32,
    sar_den: i32,
    rotation: u32,
    width: i32,
    height: i32,
) {
    let rect = unsafe { &mut *rect.cast::<[f32; 4]>() };
    *rect = fill_crop(*rect, sar_num, sar_den, rotation, width, height);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_crop_preference_and_waits_for_detection() {
        for original in [false, true] {
            let mut zoom = Zoom::default();
            assert!(zoom.toggle(original));
            assert!(!zoom.fill(true, false, false));
            assert_eq!(zoom.status(false), "FILL (DETECTING)");
            assert!(zoom.fill(true, true, false));
            assert!(zoom.fill(true, false, true));
            assert!(zoom.fill(false, false, false));
            // Even an explicit C change while zoomed must not lose the saved setting.
            assert_eq!(zoom.toggle(!original), original);
            assert!(!zoom.enabled());
            assert!(!zoom.fill(true, true, false));
            assert_eq!(zoom.status(false), "FIT");
        }
    }

    #[test]
    fn fills_simpsons_and_letterboxed_tokyo_without_stretching() {
        assert_eq!(
            fill_crop([0.0, 0.0, 720.0, 576.0], 16, 15, 0, 1920, 1080),
            [0.0, 72.0, 720.0, 504.0]
        );
        assert_eq!(
            fill_crop([0.0, 108.0, 720.0, 376.0], 8, 9, 0, 1920, 1080),
            [92.0, 108.0, 628.0, 376.0]
        );
    }

    #[test]
    fn fill_preserves_center_aspect_and_bounds_after_rotation_and_resize() {
        let rect = [12.0, 108.0, 708.0, 376.0];
        for rotation in 0..4 {
            for (width, height) in [(1920, 1080), (1080, 1920), (1000, 1000), (320, 180)] {
                let out = fill_crop(rect, 8, 9, rotation, width, height);
                assert!(out[0] >= rect[0] && out[1] >= rect[1]);
                assert!(out[2] <= rect[2] && out[3] <= rect[3]);
                assert!((out[0] + out[2] - rect[0] - rect[2]).abs() < 0.001);
                assert!((out[1] + out[3] - rect[1] - rect[3]).abs() < 0.001);
                let aspect = (out[2] - out[0]) * 8.0 / 9.0 / (out[3] - out[1]);
                let aspect = if rotation % 2 == 0 {
                    aspect
                } else {
                    1.0 / aspect
                };
                assert!((aspect - width as f32 / height as f32).abs() < 0.00001);
            }
        }
        assert_eq!(fill_crop(rect, 1, 1, 0, 0, 1080), rect);
        assert_eq!(fill_crop([0.0; 4], 1, 1, 0, 1920, 1080), [0.0; 4]);
        assert_eq!(
            fill_crop([720.0, 576.0, 0.0, 0.0], 16, 15, 0, 1920, 1080),
            [720.0, 504.0, 0.0, 72.0]
        );
    }
}
