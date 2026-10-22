use crate::{Result, ffi, geometry};
#[cfg(target_os = "linux")]
use std::env;
use std::ffi::CStr;
use std::time::{Duration, Instant};

pub(crate) struct IdleCursor {
    last_activity: Instant,
    can_hide: bool,
    visible: bool,
}

impl IdleCursor {
    pub(crate) fn new() -> Self {
        Self {
            last_activity: Instant::now(),
            can_hide: false,
            visible: true,
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if self.visible != visible
            && unsafe { ffi::up_platform_show_cursor(i32::from(visible)) } != 0
        {
            self.visible = visible;
        }
    }

    pub(crate) fn activity(&mut self) {
        self.last_activity = Instant::now();
        self.set_visible(true);
    }

    pub(crate) fn update(&mut self, window: &Window) {
        // Read SDL's actual state, including compositor-driven fullscreen changes.
        let can_hide = unsafe { ffi::up_window_can_hide_cursor(window.as_ptr()) } != 0;
        if !can_hide || !self.can_hide {
            self.last_activity = Instant::now();
        }
        self.can_hide = can_hide;
        self.set_visible(!can_hide || self.last_activity.elapsed() < Duration::from_millis(1500));
    }
}

impl Drop for IdleCursor {
    fn drop(&mut self) {
        self.set_visible(true);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    CycleAudio,
    CycleSubtitles,
    Quit,
    SeekBackward,
    SeekForward,
    PreviousChapter,
    NextChapter,
    ToggleFullscreen,
    ToggleCrop,
    CycleDeinterlace,
    ToggleZoom,
    ToggleInfo,
    ToggleLoop,
    ToggleMute,
    TogglePause,
    ToggleSubtitles,
    #[cfg(target_os = "macos")]
    ToggleUpscale,
}

pub(crate) fn action_shortcut(action: &Action) -> &'static str {
    match action {
        Action::CycleAudio => "A",
        Action::CycleSubtitles => "J",
        Action::Quit => "Q",
        Action::SeekBackward => "LEFT",
        Action::SeekForward => "RIGHT",
        Action::PreviousChapter => ",",
        Action::NextChapter => ".",
        Action::ToggleFullscreen => "F",
        Action::ToggleCrop => "C",
        Action::CycleDeinterlace => "D",
        Action::ToggleZoom => "Z",
        Action::ToggleInfo => "I",
        Action::ToggleLoop => "L",
        Action::ToggleMute => "M",
        Action::TogglePause => "SPACE",
        Action::ToggleSubtitles => "S",
        #[cfg(target_os = "macos")]
        Action::ToggleUpscale => "U",
    }
}

pub(crate) fn action_for_key(key: u32) -> Option<Action> {
    match key {
        ffi::UpKey_UP_KEY_COMMA => Some(Action::PreviousChapter),
        ffi::UpKey_UP_KEY_PERIOD => Some(Action::NextChapter),
        ffi::UpKey_UP_KEY_Z => Some(Action::ToggleZoom),
        ffi::UpKey_UP_KEY_D => Some(Action::CycleDeinterlace),
        ffi::UpKey_UP_KEY_C => Some(Action::ToggleCrop),
        ffi::UpKey_UP_KEY_A => Some(Action::CycleAudio),
        ffi::UpKey_UP_KEY_M => Some(Action::ToggleMute),
        ffi::UpKey_UP_KEY_L => Some(Action::ToggleLoop),
        ffi::UpKey_UP_KEY_Q => Some(Action::Quit),
        ffi::UpKey_UP_KEY_J => Some(Action::CycleSubtitles),
        ffi::UpKey_UP_KEY_LEFT => Some(Action::SeekBackward),
        ffi::UpKey_UP_KEY_RIGHT => Some(Action::SeekForward),
        ffi::UpKey_UP_KEY_F => Some(Action::ToggleFullscreen),
        ffi::UpKey_UP_KEY_I => Some(Action::ToggleInfo),
        ffi::UpKey_UP_KEY_SPACE => Some(Action::TogglePause),
        ffi::UpKey_UP_KEY_S => Some(Action::ToggleSubtitles),
        #[cfg(target_os = "macos")]
        ffi::UpKey_UP_KEY_U => Some(Action::ToggleUpscale),
        _ => None,
    }
}

pub(crate) fn sdl_error() -> String {
    // SAFETY: SDL exposes a thread-local error string; copy it before another call.
    let error = unsafe { ffi::up_platform_error() };
    if error.is_null() {
        "unknown SDL error".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
pub(crate) fn close_button_contains(
    x: f32,
    y: f32,
    logical_width: i32,
    logical_height: i32,
    pixel_width: i32,
    pixel_height: i32,
) -> bool {
    geometry::WindowGeometry::new(logical_width, logical_height, pixel_width, pixel_height)
        .is_some_and(|geometry| {
            geometry.hit(f64::from(x), f64::from(y)) == geometry::HitRegion::Close
        })
}

#[cfg(test)]
pub(crate) fn scrubber_target(
    x: f32,
    y: f32,
    logical_width: i32,
    logical_height: i32,
    pixel_width: i32,
    pixel_height: i32,
    duration: f64,
) -> Option<f64> {
    geometry::WindowGeometry::new(logical_width, logical_height, pixel_width, pixel_height)?
        .scrubber_target(f64::from(x), f64::from(y), duration)
}

pub(crate) struct Sdl;

impl Sdl {
    /// # Safety
    /// Call once on the main thread, before spawning threads that may read the
    /// environment. Drop this guard on that thread after all SDL resources.
    pub(crate) unsafe fn init() -> Result<Self> {
        // Keep Linux backend selection explicit; SDL selects native macOS drivers.
        #[cfg(target_os = "linux")]
        unsafe {
            env::set_var("SDL_VIDEODRIVER", "wayland");
            env::set_var("SDL_AUDIODRIVER", "pipewire");
        }
        if unsafe { ffi::up_platform_init() } == 0 {
            return Err(format!("SDL initialization failed: {}", sdl_error()));
        }
        Ok(Self)
    }
}

impl Drop for Sdl {
    fn drop(&mut self) {
        unsafe { ffi::up_platform_quit() };
    }
}

pub(crate) struct Window(*mut ffi::UpWindow);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WindowDimensions {
    pub(crate) logical_width: i32,
    pub(crate) logical_height: i32,
    pub(crate) pixel_width: i32,
    pub(crate) pixel_height: i32,
}

impl Window {
    pub(crate) fn as_ptr(&self) -> *mut ffi::UpWindow {
        self.0
    }

    pub(crate) fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        if unsafe { ffi::up_window_set_fullscreen(self.0, i32::from(fullscreen)) } == 0 {
            return Err(format!("could not toggle fullscreen: {}", sdl_error()));
        }
        Ok(())
    }

    /// # Safety
    /// SDL must be initialized on this main thread and remain alive until the
    /// window and its dependent resources have been dropped.
    pub(crate) unsafe fn create(title: &CStr) -> Result<Self> {
        let window = unsafe { ffi::up_window_create(title.as_ptr(), 1280, 720) };
        if window.is_null() {
            return Err(format!(
                "could not create the video window: {}",
                sdl_error()
            ));
        }
        Ok(Self(window))
    }

    pub(crate) fn pixel_size(&self) -> Result<(i32, i32)> {
        let mut width = 0;
        let mut height = 0;
        if unsafe { ffi::up_window_pixel_size(self.0, &mut width, &mut height) } == 0 {
            return Err(format!("could not query window size: {}", sdl_error()));
        }
        Ok((width, height))
    }

    fn geometry(&self) -> Option<geometry::WindowGeometry> {
        let WindowDimensions {
            logical_width: width,
            logical_height: height,
            pixel_width,
            pixel_height,
        } = self.dimensions().ok()?;
        let radius = unsafe { ffi::up_window_corner_radius(self.0) };
        Some(
            geometry::WindowGeometry::new(width, height, pixel_width, pixel_height)?
                .with_radius(radius),
        )
    }

    pub(crate) fn dimensions(&self) -> Result<WindowDimensions> {
        let (mut width, mut height) = (0, 0);
        if unsafe { ffi::up_window_size(self.0, &mut width, &mut height) } == 0 {
            return Err(format!("could not query window size: {}", sdl_error()));
        }
        let (pw, ph) = self.pixel_size()?;
        Ok(WindowDimensions {
            logical_width: width,
            logical_height: height,
            pixel_width: pw,
            pixel_height: ph,
        })
    }

    pub(crate) fn close_button_contains(&self, x: f32, y: f32) -> bool {
        self.geometry()
            .is_some_and(|g| g.hit(f64::from(x), f64::from(y)) == geometry::HitRegion::Close)
    }

    pub(crate) fn scrubber_target(&self, x: f32, y: f32, duration: Option<f64>) -> Option<f64> {
        self.geometry()?
            .scrubber_target(f64::from(x), f64::from(y), duration?)
    }

    pub(crate) fn set_minimum_size(&self) -> Result<()> {
        if unsafe { ffi::up_window_set_minimum_size(self.0, 320, 180) } == 0 {
            return Err(format!(
                "could not set the minimum window size: {}",
                sdl_error()
            ));
        }
        Ok(())
    }

    pub(crate) fn chapter_hover(&self, x: f32, y: f32, markers: &[f32]) -> Option<usize> {
        self.geometry()?
            .chapter_hover(f64::from(x), f64::from(y), markers)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { ffi::up_window_destroy(self.0) };
    }
}

pub(crate) struct WindowShape<'window> {
    #[cfg(target_os = "linux")]
    native: *mut ffi::UpWaylandInput,
    _window: &'window Window,
}

impl<'window> WindowShape<'window> {
    #[cfg(target_os = "linux")]
    pub(crate) fn set_region(&self, rectangles: &[[i32; 4]], radius: f32) -> Result<()> {
        if !unsafe {
            ffi::up_wayland_input_set_region(
                self.native,
                rectangles.as_ptr().cast(),
                rectangles.len(),
                radius,
            )
        } {
            return Err("could not update the Wayland input region".into());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn create(window: &'window Window) -> Result<Option<Self>> {
        let input = unsafe { ffi::up_wayland_input_create(window.0.cast()) };
        if input.is_null() {
            return Err("out of memory while creating Wayland input".into());
        }
        if !unsafe { ffi::up_wayland_input_ready(input) } {
            let message = unsafe { CStr::from_ptr(ffi::up_wayland_input_error(input)) }
                .to_string_lossy()
                .into_owned();
            unsafe { ffi::up_wayland_input_destroy(input) };
            return Err(message);
        }
        Ok(Some(Self {
            native: input,
            _window: window,
        }))
    }
}

#[cfg(target_os = "macos")]
impl<'window> WindowShape<'window> {
    pub(crate) fn create(window: &'window Window) -> Result<Option<Self>> {
        Ok(Some(Self { _window: window }))
    }

    pub(crate) fn set_region(&self, _rectangles: &[[i32; 4]], radius: f32) -> Result<()> {
        if unsafe { ffi::up_window_set_corner_radius(self._window.as_ptr(), radius) } == 0 {
            return Err(format!(
                "could not update the macOS window shape: {}",
                sdl_error()
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for WindowShape<'_> {
    fn drop(&mut self) {
        unsafe { ffi::up_wayland_input_destroy(self.native) };
    }
}
