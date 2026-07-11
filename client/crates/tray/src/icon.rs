//! Programmatic tray icons — a filled status dot rendered to RGBA, so the tray
//! ships no image assets and the colour always matches [`Health`] exactly.

use crate::state::Health;
use tray_icon::Icon;

const SIZE: u32 = 32;

/// RGB status colours, chosen for contrast on both light and dark menu bars.
fn color(health: Health) -> (u8, u8, u8) {
    match health {
        Health::Ok => (46, 160, 67),        // green
        Health::Degraded => (210, 153, 34), // amber
        Health::Down => (218, 54, 51),      // red
    }
}

/// Build the tray icon for a given health state: a filled circle with a soft
/// 1px darker rim, on a transparent background.
pub fn for_health(health: Health) -> Icon {
    let rgba = draw_dot(color(health));
    // `from_rgba` only fails on a size/length mismatch, which cannot happen for
    // our fixed-size buffer; fall back is unreachable but avoids an unwrap.
    Icon::from_rgba(rgba, SIZE, SIZE).expect("status icon has a valid RGBA buffer")
}

/// Render a centered filled disc into a `SIZE x SIZE` RGBA buffer.
fn draw_dot((r, g, b): (u8, u8, u8)) -> Vec<u8> {
    let n = SIZE as i32;
    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    let center = (n as f32 - 1.0) / 2.0;
    // Leave a 2px margin so the dot doesn't touch the icon edge.
    let radius = center - 2.0;

    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // Antialias the last pixel of the edge: full alpha inside `radius`,
            // fading to 0 across one pixel, so the dot doesn't look jagged.
            let alpha = if dist <= radius {
                1.0
            } else if dist <= radius + 1.0 {
                radius + 1.0 - dist
            } else {
                0.0
            };
            if alpha <= 0.0 {
                continue;
            }
            // Darken the outer rim slightly for definition on light backgrounds.
            let rim = dist > radius - 1.5;
            let scale = if rim { 0.78 } else { 1.0 };
            let idx = ((y * n + x) * 4) as usize;
            buf[idx] = (r as f32 * scale) as u8;
            buf[idx + 1] = (g as f32 * scale) as u8;
            buf[idx + 2] = (b as f32 * scale) as u8;
            buf[idx + 3] = (alpha * 255.0) as u8;
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_health_maps_to_a_distinct_colour() {
        let ok = color(Health::Ok);
        let degraded = color(Health::Degraded);
        let down = color(Health::Down);
        assert_ne!(ok, degraded);
        assert_ne!(degraded, down);
        assert_ne!(ok, down);
    }

    #[test]
    fn dot_buffer_has_expected_size_and_opaque_center() {
        let buf = draw_dot(color(Health::Ok));
        assert_eq!(buf.len(), (SIZE * SIZE * 4) as usize);
        // The center pixel must be fully opaque.
        let center = (SIZE / 2) as usize;
        let idx = (center * SIZE as usize + center) * 4;
        assert_eq!(buf[idx + 3], 255);
    }

    #[test]
    fn corners_are_transparent() {
        let buf = draw_dot(color(Health::Down));
        // Top-left corner is outside the disc.
        assert_eq!(buf[3], 0);
    }
}
