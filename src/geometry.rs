//! One definition of the player controls for rendering, SDL resize hit tests,
//! Wayland window dragging, and the playback event handler.
pub(crate) const TOP_BAR_HEIGHT: f32 = 42.0;
pub(crate) const SCRUBBER_HIT_HEIGHT: f32 = 42.0;
pub(crate) const SCRUBBER_MARGIN: f32 = 14.0;
const RESIZE_BORDER: f64 = 10.0;

// Keep these discriminants in sync with native/input_geometry.h.
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum HitRegion {
    Content = 0,
    Close,
    Scrubber,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
    Outside,
}

#[derive(Clone, Copy)]
pub(crate) struct WindowGeometry {
    width: f64,
    height: f64,
    scale_x: f64,
    scale_y: f64,
}
impl WindowGeometry {
    pub(crate) fn new(
        width: i32,
        height: i32,
        pixel_width: i32,
        pixel_height: i32,
    ) -> Option<Self> {
        if width <= 0 || height <= 0 || pixel_width <= 0 || pixel_height <= 0 {
            return None;
        }
        Some(Self {
            width: f64::from(width),
            height: f64::from(height),
            scale_x: f64::from(width) / f64::from(pixel_width),
            scale_y: f64::from(height) / f64::from(pixel_height),
        })
    }

    pub(crate) fn hit(self, x: f64, y: f64) -> HitRegion {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= self.width
            || y >= self.height
        {
            return HitRegion::Outside;
        }
        if x >= self.width - f64::from(TOP_BAR_HEIGHT) * self.scale_x
            && y < f64::from(TOP_BAR_HEIGHT) * self.scale_y
        {
            return HitRegion::Close;
        }
        let left = x <= RESIZE_BORDER;
        let right = x >= self.width - RESIZE_BORDER;
        let top = y <= RESIZE_BORDER;
        let bottom = y >= self.height - RESIZE_BORDER;
        match (left, right, top, bottom) {
            (true, _, true, _) => HitRegion::TopLeft,
            (_, true, true, _) => HitRegion::TopRight,
            (true, _, _, true) => HitRegion::BottomLeft,
            (_, true, _, true) => HitRegion::BottomRight,
            (_, _, true, _) => HitRegion::Top,
            (_, _, _, true) => HitRegion::Bottom,
            (true, _, _, _) => HitRegion::Left,
            (_, true, _, _) => HitRegion::Right,
            _ if y >= self.height - f64::from(SCRUBBER_HIT_HEIGHT) * self.scale_y => {
                HitRegion::Scrubber
            }
            _ => HitRegion::Content,
        }
    }

    pub(crate) fn scrubber_target(self, x: f64, y: f64, duration: f64) -> Option<f64> {
        if !duration.is_finite() || duration <= 0.0 || self.hit(x, y) != HitRegion::Scrubber {
            return None;
        }
        let margin = f64::from(SCRUBBER_MARGIN) * self.scale_x;
        let track_width = self.width - 2.0 * margin;
        (track_width > 0.0).then(|| ((x - margin) / track_width).clamp(0.0, 1.0) * duration)
    }
}

// No pointers, allocation, or panicking operations cross this callback.
#[unsafe(no_mangle)]
pub extern "C" fn up_input_hit_test(
    x: f64,
    y: f64,
    width: i32,
    height: i32,
    pixel_width: i32,
    pixel_height: i32,
) -> u32 {
    WindowGeometry::new(width, height, pixel_width, pixel_height)
        .map_or(HitRegion::Outside, |geometry| geometry.hit(x, y)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_and_rust_hit_regions_agree_at_all_scales() {
        for (pw, ph) in [(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)] {
            let geometry = WindowGeometry::new(1280, 720, pw, ph).unwrap();
            for (x, y) in [
                (0.0, 0.0),
                (640.0, 0.0),
                (1279.0, 0.0),
                (0.0, 719.0),
                (1279.0, 719.0),
                (640.0, 719.0),
                (640.0, 701.0),
                (640.0, 500.0),
            ] {
                assert_eq!(
                    up_input_hit_test(x, y, 1280, 720, pw, ph),
                    geometry.hit(x, y) as u32
                );
            }
            assert_eq!(geometry.hit(1279.0, 0.0), HitRegion::Close);
            assert_eq!(geometry.hit(640.0, 719.0), HitRegion::Bottom);
            assert_eq!(geometry.hit(640.0, 701.0), HitRegion::Scrubber);
            assert_eq!(geometry.scrubber_target(640.0, 701.0, 100.0), Some(50.0));
            assert_eq!(geometry.scrubber_target(640.0, 710.0, 100.0), None);
        }
    }

    #[test]
    fn close_button_and_resize_edges_have_consistent_precedence() {
        let geometry = WindowGeometry::new(1280, 720, 1280, 720).unwrap();
        for (x, y, expected) in [
            (5.0, 5.0, HitRegion::TopLeft),
            (1275.0, 5.0, HitRegion::Close),
            (1275.0, 50.0, HitRegion::Right),
            (5.0, 715.0, HitRegion::BottomLeft),
            (1275.0, 715.0, HitRegion::BottomRight),
            (640.0, 5.0, HitRegion::Top),
            (640.0, 715.0, HitRegion::Bottom),
            (5.0, 360.0, HitRegion::Left),
        ] {
            assert_eq!(geometry.hit(x, y), expected);
        }
        let hidpi = WindowGeometry::new(1280, 720, 2560, 1440).unwrap();
        assert_eq!(hidpi.hit(1265.0, 15.0), HitRegion::Close);
        assert_eq!(hidpi.hit(1265.0, 25.0), HitRegion::Content);
    }

    #[test]
    fn invalid_sizes_and_coordinates_cannot_start_window_moves() {
        assert_eq!(
            up_input_hit_test(0.0, 0.0, 0, 0, 0, 0),
            HitRegion::Outside as u32
        );
        let geometry = WindowGeometry::new(1280, 720, 1280, 720).unwrap();
        for (x, y) in [
            (f64::NAN, 1.0),
            (1.0, f64::INFINITY),
            (-1.0, 0.0),
            (1280.0, 720.0),
        ] {
            assert_eq!(geometry.hit(x, y), HitRegion::Outside);
            assert_eq!(geometry.scrubber_target(x, y, 100.0), None);
        }
    }
}
