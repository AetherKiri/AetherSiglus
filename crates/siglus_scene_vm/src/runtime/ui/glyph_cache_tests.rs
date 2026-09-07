use super::*;
use crate::image_manager::ImageManager;

fn glyph() -> MwndGlyphProjection {
    MwndGlyphProjection {
        moji_type: 0,
        code: 65,
        ch: 'A',
        x: 0,
        y: 0,
        size: 24,
        color: (240, 220, 200),
        shadow_color: (10, 20, 30),
        fuchi_color: (80, 60, 40),
        shadow_mode: 3,
        shadow: true,
        fuchi: true,
        bold: false,
        reveal_index: 0,
        ruby: false,
        appeared: true,
        emoji_file: None,
        emoji_font_size: 0,
        message_button: None,
    }
}

fn font() -> FontCache {
    let mut font = FontCache::new();
    assert!(font.load_from_font_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts")));
    font
}

#[test]
fn repeated_projection_and_reveal_do_not_rasterize_or_reupload_glyphs() {
    let font = font();
    let mut images = ImageManager::new(PathBuf::new());
    let mut runtimes = Vec::new();
    let mut glyph = glyph();
    UiRuntime::refresh_projected_glyph_layers(
        &font,
        &mut images,
        &[glyph.clone()],
        &mut runtimes,
        false,
    );
    let ids = [
        runtimes[0].shadow_image.unwrap(),
        runtimes[0].fuchi_image.unwrap(),
        runtimes[0].body_image.unwrap(),
    ];
    let before: Vec<_> = ids
        .iter()
        .map(|&id| {
            let (img, version) = images.get_entry(id).unwrap();
            (std::sync::Arc::clone(img), version)
        })
        .collect();
    glyph.x = 100;
    glyph.y = 200;
    glyph.appeared = false;
    glyph.reveal_index = 9;
    for _ in 0..10 {
        UiRuntime::refresh_projected_glyph_layers(
            &font,
            &mut images,
            &[glyph.clone()],
            &mut runtimes,
            false,
        );
    }
    for (id, (pixels, version)) in ids.into_iter().zip(before) {
        let (actual, actual_version) = images.get_entry(id).unwrap();
        assert_eq!(actual_version, version);
        assert!(std::sync::Arc::ptr_eq(actual, &pixels));
    }

    // Every raster input invalidates the cache, not just character identity.
    for change in 0..10 {
        let id = runtimes[0].body_image.unwrap();
        let before_version = images.get_entry(id).unwrap().1;
        match change {
            0 => glyph.ch = 'B',
            1 => glyph.size += 3,
            2 => glyph.color.0 -= 1,
            3 => glyph.shadow_color.0 += 1,
            4 => glyph.fuchi_color.0 += 1,
            5 => glyph.shadow_mode = 2,
            6 => glyph.shadow = false,
            7 => glyph.fuchi = false,
            8 => glyph.bold = true,
            _ => {}
        }
        UiRuntime::refresh_projected_glyph_layers(
            &font,
            &mut images,
            &[glyph.clone()],
            &mut runtimes,
            change == 9,
        );
        assert!(
            images.get_entry(id).unwrap().1 > before_version,
            "input {change}"
        );
    }
    UiRuntime::refresh_projected_glyph_layers(&font, &mut images, &[], &mut runtimes, false);
    assert!(runtimes[0].source_index.is_none());
    assert!(runtimes[0].body_image.is_none());
    assert!(runtimes[0].rasterized_glyph.is_none());
    UiRuntime::refresh_projected_glyph_layers(&font, &mut images, &[glyph], &mut runtimes, false);
    assert!(runtimes[0].body_image.is_some());
}

#[test]
fn font_invalidation_refreshes_both_message_and_name_glyphs() {
    let mut ui = UiRuntime::default();
    ui.font_cache = font();
    let mut images = ImageManager::new(PathBuf::new());
    ui.mwnd.msg.glyphs = vec![glyph()];
    ui.mwnd.name.glyphs = vec![glyph()];
    ui.invalidate_mwnd_text_images();
    ui.refresh_text_images(&mut images, 1920, 1080);
    let ids = [
        ui.mwnd.msg.glyph_layers[0].body_image.unwrap(),
        ui.mwnd.name.glyph_layers[0].body_image.unwrap(),
    ];
    let versions = ids.map(|id| images.get_entry(id).unwrap().1);
    ui.invalidate_mwnd_text_images();
    assert!(ui.mwnd.msg.glyph_layers[0].rasterized_glyph.is_none());
    assert!(ui.mwnd.name.glyph_layers[0].rasterized_glyph.is_none());
    ui.refresh_text_images(&mut images, 1920, 1080);
    for (id, version) in ids.into_iter().zip(versions) {
        assert!(images.get_entry(id).unwrap().1 > version);
    }
}
