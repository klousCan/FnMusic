use eframe::egui::{self, Color32, ColorImage, CornerRadius, Rect, TextureHandle, Vec2};

const THUMBNAIL: u32 = 320;

pub fn decode(data: &[u8]) -> Option<ColorImage> {
    if data
        .iter()
        .take(512)
        .copied()
        .collect::<Vec<_>>()
        .windows(4)
        .any(|tag| tag == b"<svg")
    {
        let mut options = resvg::usvg::Options::default();
        options.image_href_resolver.resolve_string = Box::new(|_, _| None);
        options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
        let tree = resvg::usvg::Tree::from_data(data, &options).ok()?;
        let size = tree.size();
        let scale = THUMBNAIL as f32 / size.width().max(size.height());
        let width = (size.width() * scale).round().max(1.0) as u32;
        let height = (size.height() * scale).round().max(1.0) as u32;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        return Some(ColorImage::from_rgba_premultiplied(
            [width as usize, height as usize],
            pixmap.data(),
        ));
    }

    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(THUMBNAIL, THUMBNAIL)
        .into_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        &image,
    ))
}

pub fn paint(ui: &egui::Ui, rect: Rect, texture: Option<&TextureHandle>, seed: &str) {
    let rounding = CornerRadius::same(9);
    if let Some(texture) = texture {
        let image = egui::Image::new((texture.id(), texture.size_vec2()))
            .corner_radius(rounding)
            .fit_to_exact_size(rect.size());
        image.paint_at(ui, rect);
        return;
    }
    let palettes = [
        Color32::from_rgb(171, 72, 99),
        Color32::from_rgb(72, 97, 147),
        Color32::from_rgb(140, 84, 146),
        Color32::from_rgb(55, 117, 111),
        Color32::from_rgb(142, 90, 71),
    ];
    let hash = seed.bytes().fold(0usize, |h, value| {
        h.wrapping_mul(31).wrapping_add(value.into())
    });
    let painter = ui.painter();
    painter.rect_filled(rect, rounding, palettes[hash % palettes.len()]);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "♪",
        egui::FontId::proportional((rect.width() * 0.4).clamp(12.0, 48.0)),
        Color32::from_white_alpha(215),
    );
}

pub fn square(size: f32) -> Vec2 {
    Vec2::splat(size)
}
