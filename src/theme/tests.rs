use super::*;

#[test]
fn every_built_in_theme_parses_in_both_appearances() {
    for (name, text) in BUILT_IN {
        let theme =
            Theme::parse(name, text, Source::BuiltIn).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!theme.description.is_empty(), "{name} says what it is");
        assert_ne!(theme.dark, theme.light, "{name} has two different halves");
    }
}

#[test]
fn the_default_theme_is_built_in_and_listed_first() {
    assert_eq!(BUILT_IN[0].0, DEFAULT_THEME);
    let mut names: Vec<&str> = BUILT_IN.iter().map(|(n, _)| *n).collect();
    let rest = names.split_off(1);
    let mut sorted = rest.clone();
    sorted.sort();
    assert_eq!(
        rest, sorted,
        "the rest by name, so the picker reads as a list"
    );
}

// The default palette is pando's own, exactly as it was before themes were
// data: nobody who never picks a theme sees a colour change.
#[test]
fn the_default_theme_keeps_pandos_own_colours() {
    let (all, _) = themes(None);
    let pando = find(&all, DEFAULT_THEME).unwrap();
    let dark = pando.palette(Appearance::Dark);
    assert_eq!(dark.text, Color::Rgb(210, 215, 225));
    assert_eq!(dark.text_muted, Color::Rgb(92, 97, 116));
    assert_eq!(dark.surface, Color::Rgb(30, 33, 45));
    assert_eq!(dark.search_cursor_bg, Color::Rgb(230, 150, 60));
    let light = pando.palette(Appearance::Light);
    assert_eq!(light.blue, Color::Rgb(50, 95, 205));
    assert_eq!(light.search_cursor_bg, Color::Rgb(245, 190, 110));
}

#[test]
fn a_role_a_theme_leaves_out_is_mixed_from_its_background_and_text() {
    let text = r##"
        description = "test"
        [dark]
        background = "#000000"
        foreground = "#ffffff"
        red = "#ff0000"
        green = "#00ff00"
        yellow = "#ffff00"
        blue = "#0000ff"
        magenta = "#ff00ff"
        cyan = "#00ffff"
        orange = "#ff8800"
        [light]
        background = "#ffffff"
        foreground = "#000000"
        red = "#ff0000"
        green = "#00ff00"
        yellow = "#ffff00"
        blue = "#0000ff"
        magenta = "#ff00ff"
        cyan = "#00ffff"
        orange = "#ff8800"
        border = "#123456"
    "##;
    let theme = Theme::parse("t", text, Source::BuiltIn).unwrap();
    let dark = theme.palette(Appearance::Dark);
    // Between black and white, nearer black for a surface than for text.
    let grey = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            assert!(r == g && g == b, "{c:?} is a grey");
            r
        }
        other => panic!("{other:?}"),
    };
    assert!(grey(dark.surface) < grey(dark.highlight_bg));
    assert!(grey(dark.highlight_bg) < grey(dark.border));
    assert!(grey(dark.border) < grey(dark.text_muted));
    assert!(grey(dark.text_muted) < grey(dark.text_dim));
    assert_eq!(dark.search_cursor_bg, Color::Rgb(255, 136, 0), "the orange");
    // A role the file sets is taken as it is.
    assert_eq!(
        theme.palette(Appearance::Light).border,
        Color::Rgb(0x12, 0x34, 0x56)
    );
}

#[test]
fn a_colour_that_is_not_hex_says_how_to_write_it() {
    let bad = BUILT_IN[1].1.replacen("\"#", "\"", 1);
    let error = Theme::parse("bad", &bad, Source::BuiltIn).unwrap_err();
    assert!(error.contains("#rrggbb"), "{error}");
}

#[test]
fn a_file_of_ones_own_replaces_the_built_in_it_is_named_for() {
    let dir = tempfile::tempdir().unwrap();
    let mine = BUILT_IN[1]
        .1
        .replace("description = \"", "description = \"mine: ");
    std::fs::write(dir.path().join("catppuccin.toml"), &mine).unwrap();
    std::fs::write(dir.path().join("zz-new.toml"), &mine).unwrap();
    std::fs::write(dir.path().join("broken.toml"), "not = [toml").unwrap();
    let (all, warnings) = themes(Some(dir.path()));
    let catppuccin = find(&all, "catppuccin").unwrap();
    assert!(catppuccin.description.starts_with("mine: "));
    assert!(matches!(catppuccin.source, Source::File(_)));
    assert_eq!(all.len(), BUILT_IN.len() + 1, "one new, one replaced");
    assert_eq!(all.last().unwrap().name, "zz-new");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("broken.toml"), "{warnings:?}");
}

#[test]
fn a_theme_is_chosen_from_the_file_then_the_config_then_the_default() {
    // The environment is left alone: other tests run beside this one.
    if std::env::var(THEME_ENV).is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("current");
    std::fs::write(&state, "gruvbox\n").unwrap();
    let settings = Settings {
        theme: Some("github".into()),
        theme_from: Some(state.clone()),
        appearance: Some("light".into()),
    };
    assert_eq!(
        choose(&settings),
        ("gruvbox".into(), Origin::File(state.clone()))
    );
    std::fs::write(&state, "").unwrap();
    assert_eq!(
        choose(&settings),
        ("github".into(), Origin::Config),
        "an empty file says nothing"
    );
    let none = Settings::default();
    assert_eq!(choose(&none), (DEFAULT_THEME.into(), Origin::Default));
}

// A half the config pins says so, rather than passing for the system's.
#[test]
fn a_pinned_appearance_says_what_pinned_it() {
    // The environment is left alone: other tests run beside this one.
    if std::env::var(APPEARANCE_ENV).is_ok() {
        return;
    }
    let settings = Settings {
        appearance: Some("light".into()),
        ..Settings::default()
    };
    assert_eq!(
        appearance(&settings),
        (Appearance::Light, AppearanceOrigin::Config)
    );
    assert_eq!(
        resolve(&settings, None).appearance_origin,
        AppearanceOrigin::Config
    );
}

#[test]
fn an_unknown_theme_falls_back_to_the_default_and_says_so() {
    if std::env::var(THEME_ENV).is_ok() {
        return;
    }
    let settings = Settings {
        theme: Some("no-such-theme".into()),
        theme_from: None,
        appearance: Some("dark".into()),
    };
    let resolved = resolve(&settings, None);
    assert_eq!(resolved.name, DEFAULT_THEME);
    assert!(
        resolved.warnings[0].contains("no-such-theme"),
        "{:?}",
        resolved.warnings
    );
}

#[test]
fn setting_a_palette_changes_what_every_role_returns() {
    let (all, _) = themes(None);
    let gruvbox = find(&all, "gruvbox").unwrap().palette(Appearance::Dark);
    set_palette(gruvbox);
    assert_eq!(green(), gruvbox.green);
    assert_eq!(border(), gruvbox.border);
}

#[test]
fn hex_colours_parse_and_mix() {
    assert_eq!(Rgb::parse("#89b4fa"), Some(Rgb(0x89, 0xb4, 0xfa)));
    assert_eq!(Rgb::parse("89b4fa"), None);
    assert_eq!(Rgb::parse("#89b4f"), None);
    assert_eq!(Rgb::parse("#89b4fg"), None);
    assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 0.5), Rgb(100, 50, 25));
}

// One fact, one row: the files and the list that compiles them in are
// held to one set.
#[test]
fn every_file_in_builtin_has_a_row_and_every_row_a_file() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/theme/builtin");
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    files.sort();
    let mut rows: Vec<String> = BUILT_IN.iter().map(|(n, _)| n.to_string()).collect();
    rows.sort();
    assert_eq!(files, rows);
}

// A theme whose own text is soft must not make pando's labels unreadable:
// the derived greys are pushed until they reach a floor.
#[test]
fn derived_text_reads_against_what_it_sits_on_in_every_built_in() {
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => Rgb(r, g, b),
        other => panic!("{other:?}"),
    };
    for (name, text) in BUILT_IN.iter().filter(|(n, _)| *n != DEFAULT_THEME) {
        let theme = Theme::parse(name, text, Source::BuiltIn).unwrap();
        for appearance in [Appearance::Dark, Appearance::Light] {
            let p = theme.palette(appearance);
            let muted = rgb(p.text_muted).contrast(rgb(p.surface));
            assert!(muted >= 2.95, "{name} {appearance:?}: muted {muted:.2}");
        }
    }
}

#[test]
fn contrast_is_wcags_ratio() {
    let black = Rgb(0, 0, 0);
    let white = Rgb(255, 255, 255);
    assert!((black.contrast(white) - 21.0).abs() < 0.01);
    assert!((white.contrast(white) - 1.0).abs() < 0.001);
}

fn channels(color: Color) -> Rgb {
    match color {
        Color::Rgb(r, g, b) => Rgb(r, g, b),
        other => panic!("a theme colour is always RGB: {other:?}"),
    }
}

fn distance(a: Color, b: Color) -> f32 {
    let (a, b) = (channels(a), channels(b));
    let d = |x: u8, y: u8| (x as f32 - y as f32).powi(2);
    (d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)).sqrt()
}

// Decision 11: every theme has a namespaced colour of its own — readable
// on its background as text is, and never mistaken for another meaning:
// not isolated's magenta, not the cursor's blue, not any other accent, and
// not the text itself, which a theme whose text is blue would otherwise
// hand it.
#[test]
fn every_built_in_theme_has_a_readable_namespaced_colour_of_its_own() {
    let (all, _) = themes(None);
    for theme in &all {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let p = theme.palette(appearance);
            let background = match theme.source {
                Source::BuiltIn => {
                    let text = BUILT_IN.iter().find(|(n, _)| *n == theme.name).unwrap().1;
                    let file: toml::Value = toml::from_str(text).unwrap();
                    let half = match appearance {
                        Appearance::Dark => "dark",
                        Appearance::Light => "light",
                    };
                    Rgb::parse(file[half]["background"].as_str().unwrap()).unwrap()
                }
                Source::File(_) => unreachable!("built-ins only"),
            };
            let contrast = channels(p.namespaced).contrast(background);
            assert!(
                contrast >= 4.5,
                "{} {appearance:?}: namespaced reads at {contrast:.2} on its background",
                theme.name
            );
            for (meaning, other) in [
                ("magenta", p.magenta),
                ("blue", p.blue),
                ("red", p.red),
                ("green", p.green),
                ("yellow", p.yellow),
                ("cyan", p.cyan),
                ("orange", p.orange),
                ("text", p.text),
            ] {
                let apart = distance(p.namespaced, other);
                assert!(
                    apart >= 18.0,
                    "{} {appearance:?}: namespaced is {apart:.0} from {meaning}",
                    theme.name
                );
            }
        }
    }
}

// A theme may name it, as the eighth accent, and then it is exactly that;
// two themes that do not name it get two colours, from their own accents.
#[test]
fn a_theme_that_names_namespaced_gets_it_and_one_that_does_not_gets_its_own() {
    let text = |namespaced: &str| {
        format!(
            r##"
            description = "test"
            [dark]
            background = "#000000"
            foreground = "#ffffff"
            red = "#ff0000"
            green = "#00ff00"
            yellow = "#ffff00"
            blue = "#0000ff"
            magenta = "#ff00ff"
            cyan = "#00ffff"
            orange = "#ff8800"
            {namespaced}
            [light]
            background = "#ffffff"
            foreground = "#000000"
            red = "#ff0000"
            green = "#00ff00"
            yellow = "#ffff00"
            blue = "#0000ff"
            magenta = "#ff00ff"
            cyan = "#00ffff"
            orange = "#ff8800"
            "##
        )
    };
    let named = Theme::parse("named", &text("namespaced = \"#123456\""), Source::BuiltIn).unwrap();
    assert_eq!(
        named.palette(Appearance::Dark).namespaced,
        Color::Rgb(0x12, 0x34, 0x56)
    );
    let mixed = Theme::parse("mixed", &text(""), Source::BuiltIn).unwrap();
    assert_ne!(
        mixed.palette(Appearance::Dark).namespaced,
        Color::Rgb(0x12, 0x34, 0x56)
    );
    let (all, _) = themes(None);
    let pando = find(&all, "pando")
        .unwrap()
        .palette(Appearance::Dark)
        .namespaced;
    let gruvbox = find(&all, "gruvbox")
        .unwrap()
        .palette(Appearance::Dark)
        .namespaced;
    assert_ne!(pando, gruvbox, "each theme mixes its own");
}

// The TUI reads hints off a painted frame by the colour of their key, so
// that colour is no other role's in any built-in theme, either half: two
// of them give orange another accent's value, colorblind its red and Rosé
// Pine its cyan. To the eye it is still orange, one step off it.
#[test]
fn the_hint_key_is_orange_to_the_eye_and_no_other_colour() {
    use ratatui::style::Color;
    for (name, text) in BUILT_IN {
        let theme = Theme::parse(name, text, Source::BuiltIn).unwrap();
        for appearance in [Appearance::Dark, Appearance::Light] {
            let palette = theme.palette(appearance);
            assert!(
                !palette.text_colours().contains(&palette.hint_key),
                "{name} {appearance:?}: {:?} is another role's",
                palette.hint_key
            );
            let (Color::Rgb(r, g, b), Color::Rgb(or, og, ob)) = (palette.hint_key, palette.orange)
            else {
                panic!("{name} {appearance:?}: not 24-bit colours");
            };
            assert_eq!((r, g), (or, og), "{name} {appearance:?}");
            assert!(b.abs_diff(ob) <= 1, "{name} {appearance:?}: {b} from {ob}");
        }
    }
}
