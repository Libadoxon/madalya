use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

/// Wrap tightly-packed BGRA bytes as a `RenderImage` (which is itself BGRA;
/// `RgbaImage` is used purely as a byte container, per gpui's own video path).
pub fn to_render_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    let image = RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        Frame::new(image),
        1,
    ))))
}
