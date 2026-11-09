use eframe::egui::{self, Color32, ColorImage, CornerRadius, Rect, TextureHandle, Vec2};

const THUMBNAIL: u32 = 320;

/// Sample a small histogram instead of averaging a cover into muddy gray.
/// Near-white/black borders and transparent pixels do not dominate the result.
pub fn dominant_color(image: &ColorImage) -> Color32 {
    let mut bins = vec![[0_u64; 4]; 4096];
    let step = image.pixels.len().div_ceil(4096).max(1);
    for pixel in image
        .pixels
        .iter()
        .step_by(step)
        .filter(|pixel| pixel.a() >= 200)
    {
        let [r, g, b, _] = pixel.to_srgba_unmultiplied();
        let high = r.max(g).max(b);
        let low = r.min(g).min(b);
        if high < 28 || low > 235 {
            continue;
        }
        let weight = 1 + u64::from(high - low) / 24;
        let key = ((r as usize >> 4) << 8) | ((g as usize >> 4) << 4) | (b as usize >> 4);
        let bin = &mut bins[key];
        bin[0] += weight;
        bin[1] += u64::from(r) * weight;
        bin[2] += u64::from(g) * weight;
        bin[3] += u64::from(b) * weight;
    }
    let bin = bins.iter().max_by_key(|bin| bin[0]).unwrap();
    if bin[0] == 0 {
        return Color32::from_gray(128);
    }
    Color32::from_rgb(
        (bin[1] / bin[0]) as u8,
        (bin[2] / bin[0]) as u8,
        (bin[3] / bin[0]) as u8,
    )
}

pub fn decode(data: &[u8]) -> Option<ColorImage> {
    if data
        .iter()
        .take(512)
        .copied()
        .collect::<Vec<_>>()
        .windows(4)
        .any(|tag| tag == b"<svg")
    {
        return rasterize_svg(data, THUMBNAIL);
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

/// Rasterize SVGs directly at their destination size, especially for small icons.
pub fn rasterize_svg(data: &[u8], edge_pixels: u32) -> Option<ColorImage> {
    let mut options = resvg::usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    let tree = resvg::usvg::Tree::from_data(data, &options).ok()?;
    let size = tree.size();
    let scale = edge_pixels.clamp(1, 2048) as f32 / size.width().max(size.height());
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(ColorImage::from_rgba_premultiplied(
        [width as usize, height as usize],
        pixmap.data(),
    ))
}

pub fn paint(ui: &egui::Ui, rect: Rect, texture: Option<&TextureHandle>, seed: &str) {
    let rounding = CornerRadius::same(9);
    if let Some(texture) = texture {
        let image = egui::Image::new((texture.id(), texture.size_vec2()))
            .tint(Color32::WHITE)
            .corner_radius(rounding)
            .fit_to_exact_size(rect.size());
        image.paint_at(ui, rect);
        return;
    }
    let (light_background, light_foreground) = placeholder_colors(false, seed);
    let (dark_background, dark_foreground) = placeholder_colors(true, seed);
    let background = super::motion::themed_color(ui.ctx(), light_background, dark_background);
    let foreground = super::motion::themed_color(ui.ctx(), light_foreground, dark_foreground);
    let painter = ui.painter();
    painter.rect_filled(rect, rounding, background);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "♪",
        egui::FontId::proportional((rect.width() * 0.4).clamp(12.0, 48.0)),
        foreground,
    );
}

fn placeholder_colors(dark: bool, seed: &str) -> (Color32, Color32) {
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
    let color = palettes[hash % palettes.len()];
    if dark {
        (color, Color32::WHITE)
    } else {
        (
            Color32::from_rgb(244, 245, 248).lerp_to_gamma(color, 0.16),
            color.lerp_to_gamma(Color32::BLACK, 0.28),
        )
    }
}

pub fn square(size: f32) -> Vec2 {
    Vec2::splat(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_color_ignores_borders_and_transparency() {
        let blue = Color32::from_rgb(32, 108, 192);
        let mut image = ColorImage::new([10, 10], Color32::WHITE);
        image.pixels[..25].fill(Color32::TRANSPARENT);
        image.pixels[25..40].fill(Color32::BLACK);
        image.pixels[40..60].fill(blue);
        assert_eq!(dominant_color(&image), blue);
        assert_eq!(
            dominant_color(&ColorImage::new([2, 2], Color32::TRANSPARENT)),
            Color32::from_gray(128)
        );
    }

    #[test]
    fn default_covers_keep_readable_contrast_in_both_themes() {
        let luminance = |color: Color32| {
            let channel = |value: u8| {
                let value = f64::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        };
        for dark in [false, true] {
            for seed in ["a", "b", "c", "d", "e"] {
                let (background, foreground) = placeholder_colors(dark, seed);
                let a = luminance(background);
                let b = luminance(foreground);
                let contrast = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(
                    contrast >= 4.5,
                    "unreadable default cover: {dark} {seed} {contrast}"
                );
            }
        }
    }

    #[test]
    fn actual_cover_textures_are_never_tinted_by_the_theme() {
        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals);
            let texture = ctx.load_texture(
                "test-cover",
                ColorImage::new([2, 2], Color32::from_rgb(17, 126, 231)),
                egui::TextureOptions::LINEAR,
            );
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    paint(
                        ui,
                        Rect::from_min_size(egui::pos2(10.0, 10.0), square(80.0)),
                        Some(&texture),
                        "cover",
                    );
                });
            });
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            let mut found = false;
            for primitive in primitives {
                if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive
                    && mesh.texture_id == texture.id()
                {
                    found = true;
                    assert!(
                        mesh.vertices
                            .iter()
                            .any(|vertex| vertex.color == Color32::WHITE)
                    );
                    assert!(
                        mesh.vertices.iter().all(|vertex| {
                            vertex.color.r() == vertex.color.g()
                                && vertex.color.g() == vertex.color.b()
                        }),
                        "cover received a color tint"
                    );
                }
            }
            assert!(found, "cover was not painted");
        }
    }
}
