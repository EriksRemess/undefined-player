//! Owned, depth-independent access to decoded luma samples.
use crate::{decoder::VideoFrame, ffi};

pub(crate) struct Luma(pub(crate) ffi::UpLumaView);
impl Luma {
    pub(crate) fn new(frame: &VideoFrame) -> Option<Self> {
        let mut view = ffi::UpLumaView::default();
        (unsafe { ffi::up_av_frame_luma(frame.as_ptr(), &mut view) } != 0).then_some(Self(view))
    }
    pub(crate) fn sample(&self, x: usize, y: usize) -> u8 {
        let v = &self.0;
        // The adapter validates the component layout and row length. This
        // reference owns the AVFrame, including negative-stride plane storage.
        let p = unsafe {
            v.data
                .offset(y as isize * v.stride as isize)
                .add(x * v.step as usize)
        };
        let value = if v.depth + v.shift <= 8 {
            u16::from(unsafe { *p })
        } else {
            let bytes = unsafe { [*p, *p.add(1)] };
            if v.big_endian != 0 {
                u16::from_be_bytes(bytes)
            } else {
                u16::from_le_bytes(bytes)
            }
        };
        let component = (u32::from(value) >> v.shift) & ((1_u32 << v.depth) - 1);
        (component >> (v.depth - 8)) as u8
    }
}
impl Drop for Luma {
    fn drop(&mut self) {
        unsafe { ffi::up_av_frame_luma_free(&mut self.0) };
    }
}
