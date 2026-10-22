//! One definition of the player controls for rendering, SDL resize hit tests,
//! Wayland window dragging, and the playback event handler. Also owns the
//! integer-scaling policy for small video frames.
pub(crate) const TOP_BAR_HEIGHT: f32 = 42.0;
pub(crate) const CLOSE_ON_LEFT: bool = cfg!(target_os = "macos");
pub(crate) const SCRUBBER_HIT_HEIGHT: f32 = 42.0;
pub(crate) const SCRUBBER_MARGIN: f32 = 14.0;
const RESIZE_BORDER: f64 = 10.0;

// The inset square fits wholly inside the rounded outline, including at HiDPI.
pub(crate) fn corner_inset(radius: f32) -> i32 {
    (radius.max(0.0) * (1.0 - std::f32::consts::FRAC_1_SQRT_2)).ceil() as i32
}

// Return zero to retain normal aspect fitting. Limit automatic integer scaling
// to square-pixel SD sources, including portrait video, and never crop to fit.
#[unsafe(no_mangle)]
pub extern "C" fn up_video_integer_scale(
    source_width: f64,
    source_height: f64,
    sar_num: i32,
    sar_den: i32,
    rotation: u32,
    width: i32,
    height: i32,
) -> u32 {
    if !source_width.is_finite()
        || !source_height.is_finite()
        || source_width < 1.0
        || source_height < 1.0
        || source_width.fract() != 0.0
        || source_height.fract() != 0.0
        || source_width.max(source_height) > 640.0
        || source_width.min(source_height) > 480.0
        || sar_num <= 0
        || sar_num != sar_den
        || width <= 0
        || height <= 0
    {
        return 0;
    }
    let (sw, sh) = if rotation.is_multiple_of(2) {
        (source_width as u32, source_height as u32)
    } else {
        (source_height as u32, source_width as u32)
    };
    (width as u32 / sw).min(height as u32 / sh)
}

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
    radius: f64,
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
            radius: 0.0,
        })
    }

    pub(crate) fn with_radius(mut self, radius: f32) -> Self {
        self.radius = if radius.is_finite() {
            f64::from(radius.max(0.0))
                .min((self.width / self.scale_x).min(self.height / self.scale_y) * 0.5)
        } else {
            0.0
        };
        self
    }

    fn inset(self) -> f64 {
        f64::from(corner_inset(self.radius as f32))
    }

    fn contains(self, x: f64, y: f64) -> bool {
        let px = x / self.scale_x;
        let py = y / self.scale_y;
        let width = self.width / self.scale_x;
        let height = self.height / self.scale_y;
        let dx = (self.radius - px.min(width - px)).max(0.0);
        let dy = (self.radius - py.min(height - py)).max(0.0);
        dx.hypot(dy) <= self.radius
    }

    // Wayland regions use logical integer coordinates. Merge identical rows to
    // keep the region small; all corner geometry is calculated here in Rust.
    pub(crate) fn input_rectangles(self) -> Vec<[i32; 4]> {
        let mut rectangles: Vec<[i32; 4]> = Vec::new();
        let width = self.width as i32;
        for y in 0..self.height as i32 {
            let edge_y = (f64::from(y) + 0.5).min(self.height - f64::from(y) - 0.5) / self.scale_y;
            let dy = (self.radius - edge_y).max(0.0);
            let inset = ((self.radius - (self.radius * self.radius - dy * dy).max(0.0).sqrt())
                * self.scale_x)
                .ceil() as i32;
            let span = width - 2 * inset;
            if span <= 0 {
                continue;
            }
            if let Some(last) = rectangles.last_mut()
                && last[0] == inset
                && last[2] == span
                && last[1] + last[3] == y
            {
                last[3] += 1;
            } else {
                rectangles.push([inset, y, span, 1]);
            }
        }
        rectangles
    }

    pub(crate) fn hit(self, x: f64, y: f64) -> HitRegion {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= self.width
            || y >= self.height
            || !self.contains(x, y)
        {
            return HitRegion::Outside;
        }
        let inset = self.inset();
        let close_left = if CLOSE_ON_LEFT {
            inset * self.scale_x
        } else {
            self.width - (inset + f64::from(TOP_BAR_HEIGHT)) * self.scale_x
        };
        if x >= close_left
            && x < close_left + f64::from(TOP_BAR_HEIGHT) * self.scale_x
            && y >= inset * self.scale_y
            && y < (inset + f64::from(TOP_BAR_HEIGHT)) * self.scale_y
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
            _ if y >= self.height - (inset + f64::from(SCRUBBER_HIT_HEIGHT)) * self.scale_y
                && y < self.height - inset * self.scale_y =>
            {
                HitRegion::Scrubber
            }
            _ => HitRegion::Content,
        }
    }

    pub(crate) fn chapter_hover(self, x: f64, y: f64, markers: &[f32]) -> Option<usize> {
        if self.hit(x, y) == HitRegion::Outside {
            return None;
        }
        let inset = self.inset();
        let (x, y) = (x / self.scale_x - inset, y / self.scale_y - inset);
        let width = self.width / self.scale_x - 2.0 * inset;
        let height = self.height / self.scale_y - 2.0 * inset;
        if (y - (height - 18.0)).abs() > 8.0 {
            return None;
        }
        let left = f64::from(SCRUBBER_MARGIN);
        let span = (width - 2.0 * left).max(0.0);
        markers
            .iter()
            .enumerate()
            .map(|(index, marker)| (index, (x - (left + f64::from(*marker) * span)).abs()))
            .filter(|(_, distance)| *distance <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    pub(crate) fn scrubber_target(self, x: f64, y: f64, duration: f64) -> Option<f64> {
        if !duration.is_finite() || duration <= 0.0 || self.hit(x, y) != HitRegion::Scrubber {
            return None;
        }
        let margin = (f64::from(SCRUBBER_MARGIN) + self.inset()) * self.scale_x;
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

#[unsafe(no_mangle)]
pub extern "C" fn up_input_hit_test_rounded(
    x: f64,
    y: f64,
    width: i32,
    height: i32,
    pixel_width: i32,
    pixel_height: i32,
    radius: f32,
) -> u32 {
    WindowGeometry::new(width, height, pixel_width, pixel_height)
        .map_or(HitRegion::Outside, |g| g.with_radius(radius).hit(x, y)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_hover_uses_physical_dot_positions_at_all_scales() {
        for (width, height) in [(1280, 720), (1707, 960), (2560, 1440)] {
            let geometry = WindowGeometry::new(1280, 720, width, height).unwrap();
            let scale_x = 1280.0 / f64::from(width);
            let scale_y = 720.0 / f64::from(height);
            let markers = [0.0, 0.25, 0.5, 1.0];
            for (index, marker) in markers.iter().enumerate() {
                let x = 14.0 + f64::from(*marker) * f64::from(width - 28);
                let y = f64::from(height - 18);
                assert_eq!(
                    geometry.chapter_hover(x * scale_x, y * scale_y, &markers),
                    Some(index)
                );
                assert_eq!(
                    geometry.chapter_hover(x * scale_x, (y - 12.0) * scale_y, &markers),
                    None
                );
            }
            assert_eq!(geometry.chapter_hover(400.0, 300.0, &markers), None);
            assert_eq!(geometry.chapter_hover(f64::NAN, 1.0, &markers), None);
        }
        let geometry = WindowGeometry::new(320, 180, 320, 180).unwrap();
        assert_eq!(geometry.chapter_hover(161.0, 162.0, &[0.5, 0.52]), Some(0));
    }

    #[test]
    fn small_videos_use_the_largest_integer_scale_that_fits() {
        for (width, height, expected) in [
            (1280, 720, 2),
            (1920, 1080, 4),
            (3840, 2160, 8),
            (497, 497, 2),
            (495, 495, 1),
            (248, 248, 1),
            (247, 248, 0),
        ] {
            assert_eq!(
                up_video_integer_scale(248.0, 248.0, 1, 1, 0, width, height),
                expected
            );
        }
        assert_eq!(up_video_integer_scale(640.0, 480.0, 1, 1, 0, 1920, 1080), 2);
        assert_eq!(up_video_integer_scale(480.0, 640.0, 1, 1, 0, 1920, 1080), 1);
        for rotation in 0..4 {
            let expected = if rotation % 2 == 0 { 4 } else { 2 };
            assert_eq!(
                up_video_integer_scale(320.0, 180.0, 1, 1, rotation, 1280, 720),
                expected
            );
        }
    }

    #[test]
    fn other_sources_retain_normal_scaling() {
        for (sw, sh, num, den) in [
            (1280.0, 720.0, 1, 1),
            (720.0, 480.0, 1, 1),
            (640.0, 481.0, 1, 1),
            (320.0, 240.0, 4, 3),
            (248.0, 248.0, 0, 1),
            (248.0, 248.0, 1, 0),
            (248.5, 248.0, 1, 1),
            (0.0, 248.0, 1, 1),
            (f64::NAN, 248.0, 1, 1),
            (248.0, f64::INFINITY, 1, 1),
        ] {
            assert_eq!(up_video_integer_scale(sw, sh, num, den, 0, 1920, 1080), 0);
        }
        assert_eq!(up_video_integer_scale(248.0, 248.0, 1, 1, 0, 0, 720), 0);
        assert_eq!(up_video_integer_scale(248.0, 248.0, 1, 1, 0, 1280, -1), 0);
    }

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
            let close_x = if CLOSE_ON_LEFT { 1.0 } else { 1279.0 };
            assert_eq!(geometry.hit(close_x, 0.0), HitRegion::Close);
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
            (
                5.0,
                5.0,
                if CLOSE_ON_LEFT {
                    HitRegion::Close
                } else {
                    HitRegion::TopLeft
                },
            ),
            (
                1275.0,
                5.0,
                if CLOSE_ON_LEFT {
                    HitRegion::TopRight
                } else {
                    HitRegion::Close
                },
            ),
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
        let close_x = if CLOSE_ON_LEFT { 15.0 } else { 1265.0 };
        assert_eq!(hidpi.hit(close_x, 15.0), HitRegion::Close);
        assert_eq!(hidpi.hit(close_x, 25.0), HitRegion::Content);
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
