//! 封面解码：把内嵌的 JPEG/PNG/WebP 变成 GPUI 能直接上屏的 `RenderImage`。
//!
//! 为什么不用 `ImageSource::from_bytes` 那条路：它内部还会再解一次，而且失败时
//! 只给一个空白贴图，看不出是「没有封面」还是「封面坏了」。这里自己解，
//! 解不开就当没有封面，UI 用渐变占位。

use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{Frame, ImageBuffer, Rgba};

/// 解码一张封面。超过 `max_side` 的按比例缩小——封面最大也就显示到 320pt，
/// 而内嵌图常常是 1000×1000 甚至 3000×3000，原样上传纹理纯属浪费显存。
pub fn decode(bytes: &[u8], max_side: u32) -> Option<Arc<RenderImage>> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > max_side || image.height() > max_side {
        image.thumbnail(max_side, max_side)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = rgba;
    let frame = Frame::new(buffer);
    Some(Arc::new(RenderImage::new([frame])))
}

#[cfg(test)]
mod tests {
    // `use gpui_kit::*` 会把它的属性宏 `test` 带进来，盖掉 std 的 `#[test]`，
    // 不显式写回 std 的版本会把测试函数当成属性宏递归展开。
    use core::prelude::v1::test;

    use super::decode;

    fn encoded_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("写入 PNG");
        bytes
    }

    #[test]
    fn decodes_an_embedded_png() {
        let bytes = encoded_png(64, 64);
        let image = decode(&bytes, 640).expect("应能解码");
        assert_eq!(image.size(0).width.0, 64);
        assert_eq!(image.size(0).height.0, 64);
    }

    #[test]
    fn shrinks_oversized_input() {
        let bytes = encoded_png(1200, 600);
        let image = decode(&bytes, 640).expect("应能解码");
        // 长边被压到 640，另一边按比例。
        assert_eq!(image.size(0).width.0, 640);
        assert_eq!(image.size(0).height.0, 320);
    }

    #[test]
    fn garbage_is_not_a_cover() {
        assert!(decode(b"not an image at all", 640).is_none());
    }
}
