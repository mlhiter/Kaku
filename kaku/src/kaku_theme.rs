use config::{configuration, ConfigHandle};
use std::sync::Mutex;
use wezterm_term::color::{ColorPalette, SrgbaTuple};

#[derive(Clone, Copy)]
pub struct ThemePalette {
    pub primary: SrgbaTuple,
    pub secondary: SrgbaTuple,
    pub accent: SrgbaTuple,
    pub error: SrgbaTuple,
    pub text: SrgbaTuple,
    pub muted: SrgbaTuple,
    pub bg: SrgbaTuple,
    pub is_light: bool,
}

#[derive(Clone, Copy)]
struct CachedTheme {
    palette: ThemePalette,
}

static THEME_CACHE: Mutex<Option<(usize, CachedTheme)>> = Mutex::new(None);

fn rgb(hex: &str) -> SrgbaTuple {
    let hex = hex.trim_start_matches('#');
    let parse = |range| {
        hex.get(range)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(0) as f32
            / 255.0
    };

    SrgbaTuple(parse(0..2), parse(2..4), parse(4..6), 1.0)
}

fn opaque(color: SrgbaTuple) -> SrgbaTuple {
    SrgbaTuple(color.0, color.1, color.2, 1.0)
}

fn blend(base: SrgbaTuple, overlay: SrgbaTuple, amount: f32) -> SrgbaTuple {
    let amount = amount.clamp(0.0, 1.0);
    SrgbaTuple(
        base.0 + (overlay.0 - base.0) * amount,
        base.1 + (overlay.1 - base.1) * amount,
        base.2 + (overlay.2 - base.2) * amount,
        1.0,
    )
}

fn luminance(color: SrgbaTuple) -> f32 {
    0.299 * color.0 + 0.587 * color.1 + 0.114 * color.2
}

fn is_light_color(color: SrgbaTuple) -> bool {
    luminance(color) > 0.5
}

fn color_distance(a: SrgbaTuple, b: SrgbaTuple) -> f32 {
    let dr = a.0 - b.0;
    let dg = a.1 - b.1;
    let db = a.2 - b.2;
    (dr * dr + dg * dg + db * db).sqrt()
}

fn has_enough_separation(bg: SrgbaTuple, color: SrgbaTuple) -> bool {
    color_distance(bg, color) >= 0.18 || (luminance(bg) - luminance(color)).abs() >= 0.12
}

fn pick_visible(bg: SrgbaTuple, candidates: &[SrgbaTuple]) -> SrgbaTuple {
    for candidate in candidates {
        let candidate = opaque(*candidate);
        if has_enough_separation(bg, candidate) {
            return candidate;
        }
    }

    let fallback = opaque(candidates[0]);
    blend(bg, fallback, if is_light_color(bg) { 0.55 } else { 0.45 })
}

fn pick_muted(bg: SrgbaTuple, text: SrgbaTuple, candidate: SrgbaTuple) -> SrgbaTuple {
    let candidate = opaque(candidate);
    if has_enough_separation(bg, candidate) {
        candidate
    } else {
        blend(bg, text, if is_light_color(bg) { 0.42 } else { 0.5 })
    }
}

fn approx_eq(a: SrgbaTuple, b: SrgbaTuple) -> bool {
    (a.0 - b.0).abs() <= 0.01 && (a.1 - b.1).abs() <= 0.01 && (a.2 - b.2).abs() <= 0.01
}

fn palette_matches_builtin(
    palette: &ColorPalette,
    bg: SrgbaTuple,
    text: SrgbaTuple,
    cursor_bg: SrgbaTuple,
) -> bool {
    approx_eq(opaque(palette.background), bg)
        && approx_eq(opaque(palette.foreground), text)
        && approx_eq(opaque(palette.cursor_bg), cursor_bg)
}

fn dark_palette() -> ThemePalette {
    ThemePalette {
        primary: rgb("#A277FF"),
        secondary: rgb("#61FFCA"),
        accent: rgb("#FFCA85"),
        error: rgb("#FF6767"),
        text: rgb("#EDECEE"),
        muted: rgb("#6D6D6D"),
        bg: rgb("#15141B"),
        is_light: false,
    }
}

fn light_palette() -> ThemePalette {
    ThemePalette {
        primary: rgb("#5E3DB3"),
        secondary: rgb("#24837B"),
        accent: rgb("#9A7400"),
        error: rgb("#AF3029"),
        text: rgb("#403E3C"),
        muted: rgb("#7A7872"),
        bg: rgb("#FFFCF0"),
        is_light: true,
    }
}

fn builtin_kaku_theme(config: &ConfigHandle) -> Option<ThemePalette> {
    let dark = dark_palette();
    let light = light_palette();
    let dark_terminal_text = rgb("#EDECEE");
    let light_terminal_text = rgb("#100F0F");
    let light_cursor = rgb("#343331");

    match config.color_scheme.as_deref() {
        Some("Kaku Dark") | Some("Kaku Theme") => return Some(dark),
        Some("Kaku Light") => return Some(light),
        _ => {}
    }

    let palette: ColorPalette = config.resolved_palette.clone().into();
    if palette_matches_builtin(&palette, dark.bg, dark_terminal_text, dark.primary) {
        Some(dark)
    } else if palette_matches_builtin(&palette, light.bg, light_terminal_text, light_cursor) {
        Some(light)
    } else {
        None
    }
}

fn palette_from_config(config: &ConfigHandle) -> ThemePalette {
    if let Some(theme) = builtin_kaku_theme(config) {
        return theme;
    }

    let palette: ColorPalette = config.resolved_palette.clone().into();
    let bg = opaque(palette.background);
    let text = opaque(palette.foreground);
    let is_light = is_light_color(bg);

    let primary = pick_visible(
        bg,
        &[palette.colors.0[13], palette.colors.0[5], palette.cursor_bg],
    );
    let secondary = pick_visible(
        bg,
        &[
            palette.colors.0[10],
            palette.colors.0[6],
            palette.colors.0[2],
        ],
    );
    let accent = pick_visible(
        bg,
        &[
            palette.colors.0[11],
            palette.colors.0[3],
            palette.cursor_border,
        ],
    );
    let error = pick_visible(
        bg,
        &[
            palette.colors.0[9],
            palette.colors.0[1],
            palette.cursor_border,
        ],
    );
    let muted = pick_muted(bg, text, palette.colors.0[8]);
    ThemePalette {
        primary,
        secondary,
        accent,
        error,
        text,
        muted,
        bg,
        is_light,
    }
}

fn current_theme() -> CachedTheme {
    let config = configuration();
    let generation = config.generation();

    let mut cached = THEME_CACHE.lock().unwrap();
    if let Some((cached_generation, theme)) = *cached {
        if cached_generation == generation {
            return theme;
        }
    }

    let palette = palette_from_config(&config);
    let theme = CachedTheme { palette };
    *cached = Some((generation, theme));
    theme
}

pub fn current_theme_palette() -> ThemePalette {
    current_theme().palette
}
