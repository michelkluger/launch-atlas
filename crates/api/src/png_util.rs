//! Encode flood results as a translucent red mask PNG, for Leaflet
//! ImageOverlay. Reached cells are filled with a red→orange gradient based on
//! AGL clearance. Unreached cells are fully transparent. The frontend pairs
//! this with `image-rendering: pixelated` so cells appear as discrete chunks.

use std::io::Cursor;

use hikefly_core::Dem;
use hikefly_glide::FloodResult;

pub fn reachability_mask_png(dem: &Dem, flood: &FloodResult) -> Vec<u8> {
    let w = dem.cols as u32;
    let h = dem.rows as u32;
    let mut img = image::RgbaImage::new(w, h);

    let mut max_agl: f32 = 0.0;
    for r in 0..dem.rows {
        for c in 0..dem.cols {
            let i = dem.idx(r, c);
            let alt = flood.max_alt_msl[i];
            let terrain = dem.data[i];
            if alt.is_finite() && terrain.is_finite() {
                let agl = alt - terrain;
                if agl > max_agl {
                    max_agl = agl;
                }
            }
        }
    }
    let max_agl = max_agl.max(1.0);

    for r in 0..dem.rows {
        for c in 0..dem.cols {
            let i = dem.idx(r, c);
            let alt = flood.max_alt_msl[i];
            let terrain = dem.data[i];
            if alt.is_finite() && terrain.is_finite() {
                let agl = (alt - terrain).max(0.0);
                let t = (agl / max_agl).clamp(0.0, 1.0);
                // Red → orange ramp. High AGL = saturated red. Low AGL =
                // pinkish-orange (just barely reachable).
                let red = (215.0 + 30.0 * t).clamp(0.0, 255.0) as u8;
                let grn = (130.0 - 90.0 * t).clamp(0.0, 255.0) as u8;
                let blu = (60.0 - 30.0 * t).clamp(0.0, 255.0) as u8;
                let alpha = (110.0 + 90.0 * t).clamp(0.0, 255.0) as u8;
                img.put_pixel(c, r, image::Rgba([red, grn, blu, alpha]));
            } else {
                img.put_pixel(c, r, image::Rgba([0, 0, 0, 0]));
            }
        }
    }

    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("png encode");
    buf
}
