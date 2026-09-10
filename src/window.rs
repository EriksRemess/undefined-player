use crate::{Result, ffi, geometry};
use std::env;
use std::ffi::CStr;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Action {
    CycleAudio,
    CycleSubtitles,
    Quit,
    SeekBackward,
    SeekForward,
    ToggleFullscreen,
    ToggleCrop,
    CycleDeinterlace,
    ToggleInfo,
    TogglePause,
    ToggleSubtitles,
}

pub(crate) fn action_for_key(key: u32) -> Option<Action> {
    match key {
        ffi::UpKey_UP_KEY_D => Some(Action::CycleDeinterlace),
        ffi::UpKey_UP_KEY_C => Some(Action::ToggleCrop),
        ffi::UpKey_UP_KEY_A => Some(Action::CycleAudio),
        ffi::UpKey_UP_KEY_Q => Some(Action::Quit),
        ffi::UpKey_UP_KEY_J => Some(Action::CycleSubtitles),
        ffi::UpKey_UP_KEY_LEFT => Some(Action::SeekBackward),
        ffi::UpKey_UP_KEY_RIGHT => Some(Action::SeekForward),
        ffi::UpKey_UP_KEY_F => Some(Action::ToggleFullscreen),
        ffi::UpKey_UP_KEY_I => Some(Action::ToggleInfo),
        ffi::UpKey_UP_KEY_SPACE => Some(Action::TogglePause),
        ffi::UpKey_UP_KEY_S => Some(Action::ToggleSubtitles),
        _ => None,
    }
}

pub(crate) unsafe fn sdl_error() -> String {
    let error = unsafe { ffi::up_platform_error() };
    if error.is_null() {
        "unknown SDL error".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

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
    pub(crate) unsafe fn init() -> Result<Self> {
        // This player deliberately has no X11 or non-PipeWire runtime path.
        unsafe {
            env::set_var("SDL_VIDEODRIVER", "wayland");
            env::set_var("SDL_AUDIODRIVER", "pipewire");
        }
        if unsafe { ffi::up_platform_init() } == 0 {
            return Err(format!("SDL initialization failed: {}", unsafe {
                sdl_error()
            }));
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

impl Window {
    pub(crate) fn as_ptr(&self) -> *mut ffi::UpWindow {
        self.0
    }

    pub(crate) fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        if unsafe { ffi::up_window_set_fullscreen(self.0, i32::from(fullscreen)) } == 0 {
            return Err(format!("could not toggle fullscreen: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(())
    }

    pub(crate) unsafe fn create(title: &CStr) -> Result<Self> {
        let window = unsafe { ffi::up_window_create(title.as_ptr(), 1280, 720) };
        if window.is_null() {
            return Err(format!("could not create the Wayland window: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(Self(window))
    }

    pub(crate) fn pixel_size(&self) -> Result<(i32, i32)> {
        let mut width = 0;
        let mut height = 0;
        if unsafe { ffi::up_window_pixel_size(self.0, &mut width, &mut height) } == 0 {
            return Err(format!("could not query window size: {}", unsafe {
                sdl_error()
            }));
        }
        Ok((width, height))
    }

    pub(crate) fn close_button_contains(&self, x: f32, y: f32) -> bool {
        let mut logical_width = 0;
        let mut logical_height = 0;
        let mut pixel_width = 0;
        let mut pixel_height = 0;
        if unsafe { ffi::up_window_size(self.0, &mut logical_width, &mut logical_height) } == 0
            || unsafe { ffi::up_window_pixel_size(self.0, &mut pixel_width, &mut pixel_height) }
                == 0
        {
            return false;
        }
        close_button_contains(
            x,
            y,
            logical_width,
            logical_height,
            pixel_width,
            pixel_height,
        )
    }

    pub(crate) fn scrubber_target(&self, x: f32, y: f32, duration: Option<f64>) -> Option<f64> {
        let duration = duration?;
        let mut logical_width = 0;
        let mut logical_height = 0;
        let mut pixel_width = 0;
        let mut pixel_height = 0;
        if unsafe { ffi::up_window_size(self.0, &mut logical_width, &mut logical_height) } == 0
            || unsafe { ffi::up_window_pixel_size(self.0, &mut pixel_width, &mut pixel_height) }
                == 0
        {
            return None;
        }
        scrubber_target(
            x,
            y,
            logical_width,
            logical_height,
            pixel_width,
            pixel_height,
            duration,
        )
    }

    pub(crate) fn set_minimum_size(&self) -> Result<()> {
        if unsafe { ffi::up_window_set_minimum_size(self.0, 320, 180) } == 0 {
            return Err(format!(
                "could not set the minimum window size: {}",
                unsafe { sdl_error() }
            ));
        }
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { ffi::up_window_destroy(self.0) };
    }
}

pub(crate) struct WaylandInput<'window> {
    native: *mut ffi::UpWaylandInput,
    _window: &'window Window,
}

impl<'window> WaylandInput<'window> {
    pub(crate) fn create(window: &'window Window) -> Result<Self> {
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
        Ok(Self {
            native: input,
            _window: window,
        })
    }
}

impl Drop for WaylandInput<'_> {
    fn drop(&mut self) {
        unsafe { ffi::up_wayland_input_destroy(self.native) };
    }
}
