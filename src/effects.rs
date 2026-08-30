//! Custom tachyonfx effects, beyond the library's built-ins — see
//! `app.rs`'s `trigger_*_fx` methods for where these get used.

use tachyonfx::{fx, Effect, EffectTimer};

/// First code point of the Braille Patterns block (U+2800..=U+28FF, one
/// bit per dot) — 256 glyphs, from all-dots-off (which renders as blank,
/// hence `+ 1 +` below to skip it) to all-dots-on.
const BRAILLE_BASE: u32 = 0x2800;

/// A cell-by-cell "materializing" reveal, in random order: each cell not
/// yet revealed flickers through random Braille glyphs instead of just
/// sitting blank (which is what the built-in `fx::coalesce` does) — the
/// "terminal decoding text" look, and structurally different from a
/// `fade_from` wash: no single frame ever paints one uniform color across
/// the whole area, so unlike that (see `main.rs`'s `MAX_EFFECT_STEP`
/// doc — the bug that made transitions read as a flash, not just this
/// effect's replacing it), there's nothing for a mistimed frame to read
/// as a "flash" in the first place.
pub fn materialize<T: Into<EffectTimer>>(timer: T) -> Effect {
    fx::effect_fn_buf(0u32, timer, |elapsed_ms, ctx, buf| {
        *elapsed_ms += ctx.last_tick.as_millis();
        let alpha = ctx.timer.alpha();
        // Advance the flicker roughly every 40ms — fast enough to read as
        // noise, slow enough not to just look like static.
        let flicker_tick = *elapsed_ms / 40;

        for pos in buf.area.positions() {
            let cell = &mut buf[pos];
            if cell.symbol() == " " {
                continue; // nothing there to reveal
            }
            if alpha < reveal_threshold(pos.x, pos.y) {
                let n = hash(pos.x, pos.y, flicker_tick);
                let glyph = char::from_u32(BRAILLE_BASE + 1 + (n % 255)).unwrap_or(' ');
                cell.set_char(glyph);
            }
            // else: leave the real character `ui::draw` already put
            // there alone — this cell's "revealed".
        }
    })
}

/// Deterministic per-cell reveal point in `0.0..1.0`, compared against
/// the effect's overall `alpha` — no two cells reveal at exactly the
/// same moment, which is the entire "materializing" look, but there's a
/// full spread across the whole duration rather than everything
/// clustering near one end (see the distribution test below).
fn reveal_threshold(x: u16, y: u16) -> f32 {
    (hash(x, y, 0) % 10_000) as f32 / 10_000.0
}

/// Cheap, non-cryptographic position+tick hash (a few rounds of
/// multiply-xor-shift) — plenty for "looks random," no need for a real
/// RNG or the state that would come with keeping one seeded across calls.
fn hash(x: u16, y: u16, salt: u32) -> u32 {
    let mut h = (x as u32)
        .wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ salt.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_thresholds_spread_across_the_full_range() {
        // Not a statistical rigor test — just a guard against the hash
        // degenerating into "everything reveals at roughly the same
        // alpha," which would make the effect look like fade_from's
        // uniform wash again instead of a staggered materialize.
        let thresholds: Vec<f32> = (0..40u16).flat_map(|x| (0..40u16).map(move |y| (x, y))).map(|(x, y)| reveal_threshold(x, y)).collect();
        let early = thresholds.iter().filter(|&&t| t < 0.33).count();
        let mid = thresholds.iter().filter(|&&t| (0.33..0.66).contains(&t)).count();
        let late = thresholds.iter().filter(|&&t| t >= 0.66).count();
        for (name, count) in [("early", early), ("mid", mid), ("late", late)] {
            assert!(count > thresholds.len() / 10, "{name} bucket suspiciously empty: {count}/{}", thresholds.len());
        }
    }

    #[test]
    fn same_cell_same_salt_is_deterministic() {
        assert_eq!(hash(5, 7, 100), hash(5, 7, 100));
    }

    #[test]
    fn braille_base_plus_one_is_not_the_blank_glyph() {
        // U+2800 itself renders as blank — skipping straight to +1 is
        // what keeps a not-yet-revealed cell looking like noise instead
        // of occasionally flickering invisible.
        let blank = char::from_u32(BRAILLE_BASE).unwrap();
        let first_noise_glyph = char::from_u32(BRAILLE_BASE + 1).unwrap();
        assert_ne!(blank, first_noise_glyph);
    }
}
