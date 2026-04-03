//! Color parsing for R graphics context colors.

use tiny_skia::Color;

/// Parse an R color string into a tiny-skia [`Color`].
///
/// Supports `"rgba(R,G,B,A)"` format where R,G,B are 0-255 integers and A is
/// a 0.0-1.0 float.  Returns `None` for unrecognized formats.
pub fn parse_rgba(s: &str) -> Option<Color> {
    let inner = s.strip_prefix("rgba(")?.strip_suffix(')')?;
    let mut parts = inner.split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    let a: f32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Color::from_rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_opaque_black() {
        let c = parse_rgba("rgba(0,0,0,1)").unwrap();
        assert_eq!(c.alpha(), 1.0);
        assert_eq!(c.red(), 0.0);
    }

    #[test]
    fn parse_semi_transparent_red() {
        let c = parse_rgba("rgba(255,0,0,0.5)").unwrap();
        assert!((c.red() - 1.0).abs() < 1e-3);
        assert!((c.alpha() - 0.5).abs() < 1e-3);
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert!(parse_rgba("red").is_none());
        assert!(parse_rgba("rgba(1,2)").is_none());
        assert!(parse_rgba("").is_none());
    }
}
