use super::*;

fn cells_where(picture: &[Vec<Cell>], material: Material) -> Vec<(usize, usize)> {
    let mut at = Vec::new();
    for (y, row) in picture.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            if cell.material == material {
                at.push((x, y));
            }
        }
    }
    at
}

/// Every character a picture may use: the dither ramp, the root lines,
/// and the compact wordmark's half blocks.
fn drawable(ch: char) -> bool {
    RAMP.contains(&ch) || "┃┻━╺╸▀▄".contains(ch)
}

// The same inputs, the same picture: a project's grove is its own and
// does not change from one run to the next.
#[test]
fn a_grove_is_the_same_every_time_and_exactly_its_size() {
    let seed = seed_of("acme-shop");
    for (w, h) in [(24, 7), (40, 8), (78, 12), (160, 14)] {
        let picture = grove(w, h, 3, seed);
        assert_eq!(picture, grove(w, h, 3, seed));
        assert_eq!(picture.cells.len(), h, "{w}×{h}");
        assert!(picture.cells.iter().all(|row| row.len() == w), "{w}×{h}");
        for cell in picture.cells.iter().flatten() {
            assert!(drawable(cell.ch), "{:?} is not drawn here", cell.ch);
            assert_eq!(cell.ch == ' ', cell.material == Material::Sky, "{cell:?}");
        }
    }
}

#[test]
fn nothing_is_drawn_where_a_grove_would_not_read() {
    assert!(grove(GROVE_MIN_WIDTH - 1, 20, 0, 1).is_empty());
    assert!(grove(80, GROVE_MIN_HEIGHT - 1, 0, 1).is_empty());
    assert!(!grove(GROVE_MIN_WIDTH, GROVE_MIN_HEIGHT, 0, 1).is_empty());
}

// Aspens quake: the leaves move from frame to frame, and nothing else
// does — a stem or a root that flickered would read as a glitch.
#[test]
fn only_the_leaves_quake() {
    let seed = seed_of("acme-shop");
    let still = grove(78, 12, 0, seed).cells;
    let mut moved = 0;
    for frame in 1..8 {
        let next = grove(78, 12, frame, seed).cells;
        for (y, row) in next.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let before = still[y][x];
                if *cell != before {
                    moved += 1;
                    for m in [cell.material, before.material] {
                        assert!(
                            matches!(m, Material::Canopy | Material::Sky),
                            "({x},{y}) moved and is {m:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(moved > 20, "the canopy barely moves: {moved} cells");
}

// One organism: every stem stands on the ground with a tap root under
// it, and one root line joins them all, running on past both edges.
#[test]
fn every_stem_stands_on_one_root_line() {
    for (w, h, name) in [(78, 12, "acme-shop"), (40, 8, "bookshop"), (30, 7, "x")] {
        let picture = grove(w, h, 0, seed_of(name));
        let cells = &picture.cells;
        let text = to_text(cells);
        assert!(
            picture.stems.len() >= 3,
            "{name}: {} stems",
            picture.stems.len()
        );
        let (tap, line) = (h - 2, h - 1);
        for &x in &picture.stems {
            assert_eq!(cells[tap][x].ch, '┃', "{name}: no tap root at {x}\n{text}");
            assert_eq!(
                cells[line][x].ch, '┻',
                "{name}: stem {x} off the line\n{text}"
            );
            assert_eq!(cells[h - 3][x].material, Material::Trunk, "{name}\n{text}");
            // A stem runs unbroken from its crown to the ground.
            let top = (0..h - 3)
                .find(|&y| matches!(cells[y][x].material, Material::Trunk | Material::Eye))
                .expect("a stem above the ground");
            assert!(
                (top..h - 3).all(|y| matches!(
                    cells[y][x].material,
                    Material::Trunk | Material::Eye | Material::Canopy
                )),
                "{name}: stem {x} has a gap\n{text}"
            );
        }
        // The root line is one line, edge to edge.
        assert!(
            cells[line].iter().all(|c| c.material == Material::Root),
            "{name}: the root line breaks\n{text}"
        );
        assert_eq!(cells[line][0].ch, '╺');
        assert_eq!(cells[line][w - 1].ch, '╸');
        // Between the tap roots, nothing: the line is what joins them.
        for (x, cell) in cells[tap].iter().enumerate() {
            if !picture.stems.contains(&x) {
                assert_eq!(cell.material, Material::Sky, "{name}\n{text}");
            }
        }
    }
}

#[test]
fn each_project_has_a_grove_of_its_own() {
    let a = grove(78, 12, 0, seed_of("acme-shop"));
    let b = grove(78, 12, 0, seed_of("bookshop"));
    assert_ne!(to_text(&a.cells), to_text(&b.cells));
    assert!(
        !cells_where(&a.cells, Material::Eye).is_empty(),
        "aspen bark has eyes"
    );
}

// Dithered, not banded: a flat value between two shades lands on both,
// spread across the Bayer pattern.
#[test]
fn a_shade_between_two_is_dithered_across_the_block() {
    let levels: Vec<usize> = (0..4)
        .flat_map(|y| (0..4).map(move |x| dither(0.375, x, y)))
        .collect();
    assert!(levels.contains(&1) && levels.contains(&2), "{levels:?}");
    assert_eq!(levels.iter().filter(|&&l| l == 2).count(), 8, "{levels:?}");
    assert!((0..4).all(|x| dither(0.0, x, 0) == 0 && dither(1.0, x, 0) == 4));
}

// The big wordmark spells PANDO: each letter's pixels, two cells wide,
// are inked, with a shadow down and to the right and nothing else.
#[test]
fn the_wordmark_spells_pando() {
    let mark = wordmark(WordmarkSize::Big, 0);
    assert_eq!(mark.len(), WORDMARK_HEIGHT);
    assert!(mark.iter().all(|row| row.len() == WORDMARK_WIDTH));
    let row = |y: usize| -> String {
        mark[y]
            .iter()
            .map(|c| match c.material {
                Material::Letter => '#',
                _ => '.',
            })
            .collect()
    };
    // The top row of P, A, N, D, O, and the middle row.
    assert_eq!(
        row(0).trim_end_matches('.'),
        "##########....######....##......##..########......######"
    );
    assert_eq!(
        row(2).trim_end_matches('.'),
        "##########..##########..##..##..##..##......##..##......##"
    );
    // Solid letters, where the shine is not.
    assert!(
        mark.iter()
            .flatten()
            .filter(|c| c.material == Material::Letter)
            .all(|c| c.ch == '█')
    );
    // The shadow is dithered: both of the lightest shades, in a pattern.
    let shadow: Vec<char> = mark
        .iter()
        .flatten()
        .filter(|c| c.material == Material::Shadow)
        .map(|c| c.ch)
        .collect();
    assert!(shadow.contains(&'░') && shadow.contains(&'▒'), "{shadow:?}");

    let compact = wordmark(WordmarkSize::Compact, 0);
    assert_eq!(compact.len(), 2);
    assert!(compact.iter().all(|row| row.len() == COMPACT_WIDTH));
}

// The shine crosses the letters now and then: it changes how a letter is
// shaded, never which cells are letters.
#[test]
fn the_wordmark_shines_without_changing_its_shape() {
    let shape = |frame| -> Vec<Vec<Material>> {
        wordmark(WordmarkSize::Big, frame)
            .iter()
            .map(|row| row.iter().map(|c| c.material).collect())
            .collect()
    };
    let still = to_text(&wordmark(WordmarkSize::Big, 0));
    let mut shone = false;
    for frame in 0..80 {
        assert_eq!(shape(frame), shape(0), "frame {frame} changed the letters");
        shone |= to_text(&wordmark(WordmarkSize::Big, frame)) != still;
    }
    assert!(shone, "the shine never crossed the letters");
}

// Each name is written under a stem of its own, left to right, as many as
// fit without two touching, and none is cut.
#[test]
fn names_go_under_stems_without_touching() {
    let stems = [2, 8, 14, 20, 26, 32];
    let labels: Vec<String> = ["main", "feat/login-flow", "fix/cart", "chore/deps-bump"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let placed = label_stems(&stems, &labels, 40);
    assert_eq!(
        placed,
        [
            (2, "main".to_string()),
            (8, "feat/login-flow".to_string()),
            (26, "fix/cart".to_string()),
        ],
        "chore/deps-bump has no room left"
    );
    for pair in placed.windows(2) {
        assert!(pair[0].0 + pair[0].1.len() + 2 <= pair[1].0, "{placed:?}");
    }
    assert!(label_stems(&stems, &labels, 3).is_empty());
}

// One stem per project, in leaf where something of it is up: the home
// pane's grove names each one, so the count and the places are its.
#[test]
fn a_grove_of_projects_has_a_stem_each_and_crowns_only_where_leafed() {
    let seed = seed_of("pando");
    let leafed = [true, false, true, false];
    let picture = grove_of(60, 10, 2, seed, &leafed);
    assert_eq!(picture, grove_of(60, 10, 2, seed, &leafed));
    assert_eq!(picture.stems, vec![7, 22, 37, 52]);
    assert!(picture.cells.iter().all(|row| row.len() == 60));
    // Leaves only within reach of a leafed stem.
    for (x, _) in cells_where(&picture.cells, Material::Canopy) {
        let nearest = picture
            .stems
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.abs_diff(x))
            .map(|(i, _)| i)
            .unwrap();
        assert!(leafed[nearest], "a leaf at {x} by a bare stem");
    }
    // Every stem, bare or not, has its trunk and its tap root.
    let roots = &picture.cells[picture.cells.len() - 1];
    for &x in &picture.stems {
        assert_eq!(roots[x].ch, '┻');
        assert!(
            picture
                .cells
                .iter()
                .any(|row| row[x].material == Material::Trunk),
            "no trunk at {x}"
        );
    }
    assert!(grove_of(GROVE_MIN_WIDTH - 1, 10, 0, seed, &leafed).is_empty());
    assert!(grove_of(60, 10, 0, seed, &[]).stems.is_empty());
    assert_eq!(stems_that_fit(60), 60 / STEM_SPACING_MIN);
}

// Each label of a row has its whole where all fit, and otherwise the
// short ones keep theirs and the long ones share the rest alike, the
// last no shorter than the first.
#[test]
fn a_row_of_labels_shares_its_room_evenly_among_the_long() {
    assert_eq!(share_room(&[4, 20, 3], 40), vec![4, 20, 3]);
    assert_eq!(share_room(&[30, 4, 30], 40), vec![16, 4, 16]);
    let given = share_room(&[40, 40, 40, 40], 46);
    assert_eq!(given, vec![10, 10, 10, 10]);
    assert!(share_room(&[5, 5], 3).iter().all(|&g| g <= 1));
}

// Centred under its stem, moved only to keep two cells from a neighbour
// and one from either edge.
#[test]
fn labels_sit_centred_under_their_stems_and_never_touch() {
    assert_eq!(spread_labels(&[10, 30], &[4, 6], 50), vec![8, 27]);
    // Long labels pushed apart, inside the row.
    let widths = share_room(&[30, 30, 30], 58);
    let at = spread_labels(&[20, 30, 40], &widths, 60);
    assert!(at[0] >= 1, "{at:?}");
    for i in 1..at.len() {
        assert!(at[i] >= at[i - 1] + widths[i - 1] + 2, "{at:?} {widths:?}");
    }
    assert!(at[2] + widths[2] < 60, "{at:?} {widths:?}");
}
