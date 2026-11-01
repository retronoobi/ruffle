//! CPU implementations for cached 2D filters. Pixels use premultiplied RGBA.
use ruffle_render::filters::Filter;

pub fn supported(f: &Filter) -> bool {
    !matches!(f, Filter::ShaderFilter(_))
}
fn blur(bytes: &mut Vec<u8>, w: usize, h: usize, bx: f64, by: f64, passes: u8) {
    // Sliding windows keep large Flash blur radii linear in the image size.
    let mut temp = vec![0; bytes.len()];
    for _ in 0..passes.min(15) {
        for (horizontal, radius) in [
            (true, (bx / 2.).floor() as usize),
            (false, (by / 2.).floor() as usize),
        ] {
            let radius = radius.min(256);
            if radius == 0 {
                continue;
            }
            let (length, lines) = if horizontal { (w, h) } else { (h, w) };
            for line in 0..lines {
                for channel in 0..4 {
                    let index = |p: usize| {
                        if horizontal {
                            (line * w + p) * 4 + channel
                        } else {
                            (p * w + line) * 4 + channel
                        }
                    };
                    let mut sum = 0u32;
                    for p in 0..=radius.min(length - 1) {
                        sum += bytes[index(p)] as u32;
                    }
                    for p in 0..length {
                        temp[index(p)] = (sum / (radius * 2 + 1) as u32) as u8;
                        if p >= radius {
                            sum -= bytes[index(p - radius)] as u32;
                        }
                        if p + radius + 1 < length {
                            sum += bytes[index(p + radius + 1)] as u32;
                        }
                    }
                }
            }
            std::mem::swap(bytes, &mut temp);
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Mirrors the Flash shadow filter parameters"
)]
fn shadow(
    bytes: &mut Vec<u8>,
    w: usize,
    h: usize,
    bx: f64,
    by: f64,
    passes: u8,
    color: swf::Color,
    strength: f32,
    offset: (i32, i32),
    inner: bool,
    knockout: bool,
    hide: bool,
) {
    let source = bytes.clone();
    blur(bytes, w, h, bx, by, passes);
    let alpha = bytes.clone();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let sx = x as i32 - offset.0;
            let sy = y as i32 - offset.1;
            let a = if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                alpha[(sy as usize * w + sx as usize) * 4 + 3] as f32 / 255.
            } else {
                0.
            };
            let original = source[i + 3] as f32 / 255.;
            let effect =
                ((if inner { 1. - a } else { a }) * strength).clamp(0., 1.) * color.a as f32 / 255.;
            let mask = if inner {
                original
            } else if hide && !knockout {
                1.
            } else {
                1. - original
            };
            let source_factor = if hide || knockout {
                0.
            } else if inner {
                1. - effect
            } else {
                1.
            };
            for (c, value) in [color.r, color.g, color.b, 255].into_iter().enumerate() {
                bytes[i + c] = (value as f32 * effect * mask + source[i + c] as f32 * source_factor)
                    .clamp(0., 255.)
                    .round() as u8;
            }
        }
    }
}
fn rgba(c: swf::Color) -> [f32; 4] {
    let a = c.a as f32 / 255.;
    [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32]
}
fn alpha(bytes: &[u8], w: usize, h: usize, x: i32, y: i32) -> f32 {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        0.
    } else {
        bytes[(y as usize * w + x as usize) * 4 + 3] as f32 / 255.
    }
}
fn ramp(records: &[swf::GradientRecord], ratio: f32) -> [f32; 4] {
    if records.is_empty() {
        return [0.; 4];
    }
    let hi = records
        .iter()
        .position(|r| r.ratio as f32 >= ratio)
        .unwrap_or(records.len() - 1);
    let lo = hi.saturating_sub(1);
    let (a, b) = (&records[lo], &records[hi]);
    let t = if a.ratio == b.ratio {
        0.
    } else {
        ((ratio - a.ratio as f32) / (b.ratio as f32 - a.ratio as f32)).clamp(0., 1.)
    };
    let a = rgba(a.color);
    let b = rgba(b.color);
    std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
}
#[allow(
    clippy::too_many_arguments,
    reason = "Mirrors the Flash bevel and gradient filter parameters"
)]
fn relief(
    bytes: &mut Vec<u8>,
    w: usize,
    h: usize,
    bx: f64,
    by: f64,
    passes: u8,
    angle: f64,
    distance: f64,
    strength: f32,
    inner: bool,
    full: bool,
    knockout: bool,
    colors: impl Fn(f32) -> [f32; 4],
    bevel: bool,
) {
    let source = bytes.clone();
    blur(bytes, w, h, bx, by, passes);
    let blurred = bytes.clone();
    let dx = (angle.cos() * distance).round() as i32;
    let dy = (angle.sin() * distance).round() as i32;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let original = source[i + 3] as f32 / 255.;
            let left = alpha(&blurred, w, h, x as i32 - dx, y as i32 - dy);
            let right = alpha(&blurred, w, h, x as i32 + dx, y as i32 + dy);
            let value = if bevel {
                (left - right) * strength
            } else {
                (if inner { 1. - left } else { left }) * strength
            };
            let mut effect = colors(value.clamp(-1., 1.));
            if !full {
                let mask = if inner { original } else { 1. - original };
                for c in &mut effect {
                    *c *= mask;
                }
            }
            for c in 0..4 {
                let src = source[i + c] as f32;
                let value = if knockout {
                    effect[c]
                } else if full {
                    src * (1. - effect[3] / 255.) + effect[c]
                } else if inner {
                    src * (1.
                        - if original > 0. {
                            effect[3] / (original * 255.)
                        } else {
                            0.
                        })
                        + effect[c]
                } else {
                    src + effect[c]
                };
                bytes[i + c] = value.clamp(0., 255.).round() as u8;
            }
        }
    }
}
fn convolution(bytes: &mut [u8], w: usize, h: usize, f: &swf::ConvolutionFilter) {
    let source = bytes.to_vec();
    let cols = f.num_matrix_cols as usize;
    let rows = f.num_matrix_rows as usize;
    if rows == 0 || cols == 0 || f.matrix.len() < rows * cols {
        return;
    }
    let divisor = if f.divisor == 0. { 1. } else { f.divisor };
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let mut out = [0.; 4];
            for ky in 0..rows {
                for kx in 0..cols {
                    let sx = x as i32 + kx as i32 - cols as i32 / 2;
                    let sy = y as i32 + ky as i32 - rows as i32 / 2;
                    let pixel = if f.is_clamped()
                        || (sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32)
                    {
                        let j = (sy.clamp(0, h as i32 - 1) as usize * w
                            + sx.clamp(0, w as i32 - 1) as usize)
                            * 4;
                        [
                            source[j] as f32,
                            source[j + 1] as f32,
                            source[j + 2] as f32,
                            source[j + 3] as f32,
                        ]
                    } else {
                        rgba(f.default_color)
                    };
                    for c in 0..4 {
                        let value = if c < 3 {
                            if pixel[3] > 0. {
                                pixel[c] * 255. / pixel[3]
                            } else {
                                0.
                            }
                        } else {
                            pixel[c]
                        };
                        out[c] += value * f.matrix[ky * cols + kx];
                    }
                }
            }
            for c in &mut out {
                *c = (*c / divisor + f.bias).clamp(0., 255.);
            }
            if f.is_preserve_alpha() {
                out[3] = source[i + 3] as f32;
            }
            for c in 0..3 {
                bytes[i + c] = (out[c] * out[3] / 255.).round() as u8;
            }
            bytes[i + 3] = out[3].round() as u8;
        }
    }
}
pub fn apply(
    bytes: &mut Vec<u8>,
    w: usize,
    h: usize,
    f: &Filter,
    map: Option<(&[u8], usize, usize)>,
) {
    if w == 0 || h == 0 {
        return;
    }
    let gradient_bevel = matches!(f, Filter::GradientBevelFilter(_));
    match f {
        Filter::BlurFilter(f) => blur(
            bytes,
            w,
            h,
            f.blur_x.to_f64(),
            f.blur_y.to_f64(),
            f.num_passes(),
        ),
        Filter::ColorMatrixFilter(f) => {
            for pixel in bytes.as_chunks_mut::<4>().0 {
                let a = pixel[3] as f32;
                let rgb = if a > 0. { 255. / a } else { 0. };
                let input = [
                    pixel[0] as f32 * rgb,
                    pixel[1] as f32 * rgb,
                    pixel[2] as f32 * rgb,
                    a,
                ];
                let mut result = [0.; 4];
                for (c, channel) in result.iter_mut().enumerate() {
                    *channel = (f.matrix[c * 5..c * 5 + 4]
                        .iter()
                        .zip(input)
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
                        + f.matrix[c * 5 + 4])
                        .clamp(0., 255.);
                }
                for c in 0..3 {
                    pixel[c] = (result[c] * result[3] / 255.) as u8;
                }
                pixel[3] = result[3] as u8;
            }
        }
        Filter::GlowFilter(f) => shadow(
            bytes,
            w,
            h,
            f.blur_x.to_f64(),
            f.blur_y.to_f64(),
            f.num_passes(),
            f.color,
            f.strength.to_f32(),
            (0, 0),
            f.is_inner(),
            f.is_knockout(),
            false,
        ),
        Filter::DropShadowFilter(f) => shadow(
            bytes,
            w,
            h,
            f.blur_x.to_f64(),
            f.blur_y.to_f64(),
            f.num_passes(),
            f.color,
            f.strength.to_f32(),
            (
                (f.angle.to_f64().cos() * f.distance.to_f64()).round() as i32,
                (f.angle.to_f64().sin() * f.distance.to_f64()).round() as i32,
            ),
            f.is_inner(),
            f.is_knockout(),
            f.hide_object(),
        ),
        Filter::ConvolutionFilter(f) => convolution(bytes, w, h, f),
        Filter::BevelFilter(f) => {
            let hi = rgba(f.highlight_color);
            let lo = rgba(f.shadow_color);
            relief(
                bytes,
                w,
                h,
                f.blur_x.to_f64(),
                f.blur_y.to_f64(),
                f.num_passes(),
                f.angle.to_f64(),
                f.distance.to_f64(),
                f.strength.to_f32(),
                f.is_inner(),
                f.is_on_top(),
                f.is_knockout(),
                |v| {
                    let c = if v >= 0. { hi } else { lo };
                    c.map(|c| c * v.abs())
                },
                true,
            );
        }
        Filter::GradientBevelFilter(f) | Filter::GradientGlowFilter(f) => {
            relief(
                bytes,
                w,
                h,
                f.blur_x.to_f64(),
                f.blur_y.to_f64(),
                f.num_passes(),
                f.angle.to_f64(),
                f.distance.to_f64(),
                f.strength.to_f32(),
                f.is_inner(),
                f.is_on_top(),
                f.is_knockout(),
                |v| {
                    ramp(
                        &f.colors,
                        if gradient_bevel {
                            (v + 1.) * 127.5
                        } else {
                            v * 255.
                        },
                    )
                },
                gradient_bevel,
            );
        }
        Filter::DisplacementMapFilter(f) => {
            use ruffle_render::filters::DisplacementMapFilterMode as Mode;
            let Some((map, mw, mh)) = map else {
                return;
            };
            if mw == 0 || mh == 0 {
                return;
            }
            let source = bytes.clone();
            let component = |p: &[u8], c: u8| match c {
                1 => {
                    if p[3] > 0 {
                        p[0] as f32 * 255. / p[3] as f32
                    } else {
                        0.
                    }
                }
                2 => {
                    if p[3] > 0 {
                        p[1] as f32 * 255. / p[3] as f32
                    } else {
                        0.
                    }
                }
                4 => {
                    if p[3] > 0 {
                        p[2] as f32 * 255. / p[3] as f32
                    } else {
                        0.
                    }
                }
                8 => p[3] as f32,
                _ => 128.,
            };
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let mx = ((x as f32 - f.map_point.0 as f32) / f.viewscale_x.max(0.00001))
                        .floor() as i32;
                    let my = ((y as f32 - f.map_point.1 as f32) / f.viewscale_y.max(0.00001))
                        .floor() as i32;
                    if mx < 0 || my < 0 || mx >= mw as i32 || my >= mh as i32 {
                        continue;
                    }
                    let j = (my as usize * mw + mx as usize) * 4;
                    let sx = x as i32
                        + ((component(&map[j..j + 4], f.component_x) - 128.)
                            * f.scale_x
                            * f.viewscale_x
                            / 256.)
                            .floor() as i32;
                    let sy = y as i32
                        + ((component(&map[j..j + 4], f.component_y) - 128.)
                            * f.scale_y
                            * f.viewscale_y
                            / 256.)
                            .floor() as i32;
                    let outside = sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32;
                    if outside && f.mode == Mode::Ignore {
                        continue;
                    }
                    if outside && f.mode == Mode::Color {
                        for c in 0..4 {
                            bytes[i + c] = rgba(f.color)[c].round() as u8;
                        }
                        continue;
                    }
                    let (sx, sy) = if f.mode == Mode::Wrap {
                        (sx.rem_euclid(w as i32), sy.rem_euclid(h as i32))
                    } else {
                        (sx.clamp(0, w as i32 - 1), sy.clamp(0, h as i32 - 1))
                    };
                    let j = (sy as usize * w + sx as usize) * 4;
                    bytes[i..i + 4].copy_from_slice(&source[j..j + 4]);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn convolution_identity_keeps_premultiplied_alpha() {
        let mut image = vec![64, 32, 16, 128, 0, 0, 0, 0];
        let expected = image.clone();
        let f = swf::ConvolutionFilter {
            num_matrix_rows: 1,
            num_matrix_cols: 1,
            matrix: vec![1.],
            divisor: 1.,
            bias: 0.,
            default_color: swf::Color::TRANSPARENT,
            flags: swf::ConvolutionFilterFlags::PRESERVE_ALPHA,
        };
        apply(&mut image, 2, 1, &Filter::ConvolutionFilter(f), None);
        assert_eq!(image, expected);
    }
    #[test]
    fn convolution_samples_clamped_edges() {
        let mut image = vec![255, 0, 0, 255, 0, 0, 255, 255];
        let f = swf::ConvolutionFilter {
            num_matrix_rows: 1,
            num_matrix_cols: 3,
            matrix: vec![1., 0., 0.],
            divisor: 1.,
            bias: 0.,
            default_color: swf::Color::TRANSPARENT,
            flags: swf::ConvolutionFilterFlags::CLAMP,
        };
        apply(&mut image, 2, 1, &Filter::ConvolutionFilter(f), None);
        assert_eq!(image, [255, 0, 0, 255, 255, 0, 0, 255]);
    }
    #[test]
    fn displacement_wraps_and_preserves_unmapped_pixels() {
        use ruffle_render::filters::{DisplacementMapFilter, DisplacementMapFilterMode};
        let original = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255];
        let map = vec![0, 128, 0, 255];
        let f = DisplacementMapFilter {
            component_x: 1,
            component_y: 2,
            scale_x: 2.,
            viewscale_x: 1.,
            viewscale_y: 1.,
            mode: DisplacementMapFilterMode::Wrap,
            ..Default::default()
        };
        let mut image = original.clone();
        apply(
            &mut image,
            3,
            1,
            &Filter::DisplacementMapFilter(f),
            Some((&map, 1, 1)),
        );
        assert_eq!(&image[..4], &original[8..]);
        assert_eq!(&image[4..], &original[4..]);
    }
    #[test]
    fn gradient_ramp_interpolates_premultiplied_colors() {
        let records = [
            swf::GradientRecord {
                ratio: 0,
                color: swf::Color::RED,
            },
            swf::GradientRecord {
                ratio: 255,
                color: swf::Color::BLUE,
            },
        ];
        assert_eq!(ramp(&records, 127.5), [127.5, 0., 127.5, 255.]);
        assert_eq!(ramp(&[], 127.5), [0.; 4]);
    }
    #[test]
    fn bevel_has_opposite_lit_and_shadow_edges() {
        let mut image = vec![0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0];
        let f = swf::BevelFilter {
            shadow_color: swf::Color::BLUE,
            highlight_color: swf::Color::RED,
            blur_x: swf::Fixed16::ZERO,
            blur_y: swf::Fixed16::ZERO,
            angle: swf::Fixed16::ZERO,
            distance: swf::Fixed16::ONE,
            strength: swf::Fixed8::ONE,
            flags: swf::BevelFilterFlags::KNOCKOUT,
        };
        apply(&mut image, 3, 1, &Filter::BevelFilter(f), None);
        assert_eq!(&image[..4], &[0, 0, 255, 255]);
        assert_eq!(&image[8..], &[255, 0, 0, 255]);
    }
    #[test]
    fn empty_filter_input_is_safe() {
        let mut image = vec![];
        apply(&mut image, 0, 0, &Filter::default(), None);
        assert!(image.is_empty());
    }
}
