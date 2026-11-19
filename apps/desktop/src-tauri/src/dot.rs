//! The tray icon's status dot (owner's request 12 of wave three): a small
//! filled circle with a thin dark ring at the top right of the base icon,
//! drawn into a copy of its RGBA pixels. The dark ring cuts the dot apart
//! from the icon's white wave and from a light taskbar; the bright fill
//! stands out on a dark one. Sizes follow the icon, so the dot looks the
//! same at any scale. Pure pixel arithmetic, tested on tiny images.

use serde::Deserialize;

/// Which dot the window asks for, computed from the snapshot (`trayDot` in
/// `view.ts`): none, green when every monitored tunnel is fine, red when one
/// is in a fault.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Dot {
    #[default]
    None,
    Green,
    Red,
}

impl Dot {
    /// The fill color, the bright green and red of the window's dark theme
    /// so they read at 16 px next to the icon's dark teal; `None` draws
    /// nothing.
    fn fill(self) -> Option<[u8; 3]> {
        match self {
            Dot::None => None,
            Dot::Green => Some(GREEN),
            Dot::Red => Some(RED),
        }
    }
}

const GREEN: [u8; 3] = [0x3f, 0xb9, 0x50];
const RED: [u8; 3] = [0xf8, 0x51, 0x49];

/// Diameter of the dot, ring included, as a share of the icon's shorter
/// side.
const DIAMETER: f32 = 0.54;
/// Width of the ring as a share of the shorter side, never under a pixel.
const RING: f32 = 0.08;
const RING_COLOR: [u8; 3] = [0x0d, 0x11, 0x17];
/// Samples per pixel along each axis, for a smooth edge.
const SAMPLES: u32 = 4;

/// A copy of `rgba` (`width` × `height`, row-major, not premultiplied, as
/// `tauri::image::Image` holds it) with `dot` drawn over it. A buffer of the
/// wrong length, or no dot, is returned unchanged.
pub(crate) fn with_dot(rgba: &[u8], width: u32, height: u32, dot: Dot) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let Some(fill) = dot.fill() else {
        return out;
    };
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
        return out;
    }
    let side = width.min(height) as f32;
    let outer = side * DIAMETER / 2.0;
    let inner = (outer - (side * RING).max(1.0)).max(0.0);
    let (cx, cy) = (width as f32 - outer, outer);
    let span = ((2.0 * outer).ceil() as u32).min(width).min(height);
    let total = (SAMPLES * SAMPLES) as f32;
    for y in 0..span {
        for x in width - span..width {
            // How much of this pixel the dot covers, and how much of that is
            // the fill rather than the ring.
            let (mut covered, mut filled) = (0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let d2 = (px - cx).powi(2) + (py - cy).powi(2);
                    if d2 <= outer * outer {
                        covered += 1;
                        if d2 <= inner * inner {
                            filled += 1;
                        }
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let src_alpha = covered as f32 / total;
            let fill_share = filled as f32 / total;
            let ring_share = src_alpha - fill_share;
            let i = (y as usize * w + x as usize) * 4;
            let Some(pixel) = out.get_mut(i..i + 4) else {
                continue;
            };
            // "Source over": the dot on top of the icon.
            let dst_alpha = f32::from(pixel[3]) / 255.0;
            let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
            for ((channel, f), r) in pixel.iter_mut().zip(fill).zip(RING_COLOR) {
                let src = f32::from(f) * fill_share + f32::from(r) * ring_share;
                let dst = f32::from(*channel) * dst_alpha * (1.0 - src_alpha);
                *channel = ((src + dst) / out_alpha).round().clamp(0.0, 255.0) as u8;
            }
            pixel[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    fn image(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        color.repeat((width * height) as usize)
    }

    #[test]
    fn the_dot_is_filled_at_the_top_right_and_leaves_the_rest_alone() {
        // 16 × 16, fully transparent: the dot's center is near (11.7, 4.3).
        let base = image(16, 16, [0, 0, 0, 0]);
        let red = with_dot(&base, 16, 16, Dot::Red);
        assert_eq!(pixel(&red, 16, 12, 4), [0xf8, 0x51, 0x49, 255]);
        let green = with_dot(&base, 16, 16, Dot::Green);
        assert_eq!(pixel(&green, 16, 12, 4), [0x3f, 0xb9, 0x50, 255]);
        // Outside the circle nothing changes.
        for (x, y) in [(0, 0), (0, 15), (7, 7), (15, 15), (7, 0)] {
            assert_eq!(pixel(&red, 16, x, y), [0, 0, 0, 0], "({x}, {y})");
        }
    }

    #[test]
    fn a_dark_ring_separates_the_dot_from_white_artwork() {
        // Opaque white base, 32 × 32: outer radius 8.64 around (23.36, 8.64),
        // ring 2.56 px wide.
        let base = image(32, 32, [255, 255, 255, 255]);
        let red = with_dot(&base, 32, 32, Dot::Red);
        // Straight above the center, about a pixel inside the edge: ring.
        assert_eq!(pixel(&red, 32, 24, 1), [0x0d, 0x11, 0x17, 255]);
        // The center is the fill, opaque.
        assert_eq!(pixel(&red, 32, 24, 8), [0xf8, 0x51, 0x49, 255]);
        // The icon outside stays as it was.
        assert_eq!(pixel(&red, 32, 4, 28), [255, 255, 255, 255]);
        assert_eq!(pixel(&red, 32, 31, 31), [255, 255, 255, 255]);
    }

    #[test]
    fn the_edge_is_blended_not_cut() {
        let base = image(32, 32, [0, 0, 0, 0]);
        let red = with_dot(&base, 32, 32, Dot::Red);
        // The corner pixel of the dot's square lies outside the circle.
        assert_eq!(pixel(&red, 32, 16, 0), [0, 0, 0, 0]);
        // Some pixel on the edge is partly covered.
        let partial = (16..32)
            .flat_map(|x| (0..16).map(move |y| (x, y)))
            .map(|(x, y)| pixel(&red, 32, x, y)[3])
            .any(|a| a > 0 && a < 255);
        assert!(partial);
    }

    #[test]
    fn it_scales_with_the_icon() {
        for side in [16u32, 20, 24, 32, 48, 64, 256] {
            let base = image(side, side, [0, 0, 0, 0]);
            let red = with_dot(&base, side, side, Dot::Red);
            let covered: f32 = red.chunks(4).map(|p| f32::from(p[3]) / 255.0).sum();
            let share = covered / (side * side) as f32;
            // A circle 0.54 of the side across covers π × 0.27² (0.229) of
            // the icon.
            assert!((0.21..0.25).contains(&share), "{side}: {share}");
        }
    }

    #[test]
    fn no_dot_or_a_wrong_buffer_leaves_the_icon_unchanged() {
        let base = image(16, 16, [10, 20, 30, 40]);
        assert_eq!(with_dot(&base, 16, 16, Dot::None), base);
        assert_eq!(with_dot(&base[..100], 16, 16, Dot::Red), &base[..100]);
        assert_eq!(with_dot(&base, 0, 16, Dot::Red), base);
        assert_eq!(with_dot(&base, 17, 16, Dot::Red), base);
    }

    #[test]
    fn a_wide_icon_gets_a_dot_sized_by_its_shorter_side() {
        let base = image(32, 16, [0, 0, 0, 0]);
        let red = with_dot(&base, 32, 16, Dot::Red);
        assert_eq!(pixel(&red, 32, 28, 4), [0xf8, 0x51, 0x49, 255]);
        assert_eq!(pixel(&red, 32, 20, 4), [0, 0, 0, 0]);
    }
}
