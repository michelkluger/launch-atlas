//! Render a hillshade PNG of a DEM using sun azimuth=315°, altitude=45°.
//!
//! Standard formula (Burrough & McDonnell):
//!   shade = cos(zenith) * cos(slope) + sin(zenith) * sin(slope) * cos(az_sun - aspect)
//!
//! We additionally tint by altitude (low = green, high = white) so the user
//! can read elevation at a glance.

use std::io::Cursor;

use hikefly_core::Dem;

const SUN_AZIMUTH_DEG: f32 = 315.0;
const SUN_ALTITUDE_DEG: f32 = 45.0;

pub fn render(dem: &Dem, slope_deg: &[f32], aspect_deg: &[f32]) -> Vec<u8> {
    let zenith = (90.0 - SUN_ALTITUDE_DEG).to_radians();
    let az_sun = SUN_AZIMUTH_DEG.to_radians();

    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for &z in &dem.data {
        if z.is_finite() {
            if z < min_z {
                min_z = z;
            }
            if z > max_z {
                max_z = z;
            }
        }
    }
    let range = (max_z - min_z).max(1.0);

    let w = dem.cols as u32;
    let h = dem.rows as u32;
    let mut img = image::RgbaImage::new(w, h);

    for r in 0..dem.rows {
        for c in 0..dem.cols {
            let i = dem.idx(r, c);
            let z = dem.data[i];
            if !z.is_finite() {
                img.put_pixel(c, r, image::Rgba([0, 0, 0, 0]));
                continue;
            }
            let s = slope_deg[i];
            let a = aspect_deg[i];

            // Hillshade in [0, 1]. If slope/aspect undefined, treat as flat.
            let shade = if s.is_finite() && a.is_finite() {
                let slope_rad = s.to_radians();
                let aspect_rad = a.to_radians();
                let v = zenith.cos() * slope_rad.cos()
                    + zenith.sin() * slope_rad.sin() * (az_sun - aspect_rad).cos();
                v.clamp(0.0, 1.0)
            } else {
                zenith.cos()
            };

            // Elevation tint: low altitude (valley) -> mossy green,
            // mid -> tan, high -> snowy white.
            let t = ((z - min_z) / range).clamp(0.0, 1.0);
            let (rg, gg, bg) = if t < 0.4 {
                let k = t / 0.4;
                lerp_rgb((90.0, 130.0, 80.0), (160.0, 150.0, 110.0), k)
            } else if t < 0.8 {
                let k = (t - 0.4) / 0.4;
                lerp_rgb((160.0, 150.0, 110.0), (200.0, 195.0, 185.0), k)
            } else {
                let k = (t - 0.8) / 0.2;
                lerp_rgb((200.0, 195.0, 185.0), (250.0, 250.0, 252.0), k)
            };

            // Combine: multiply tint by hillshade (with a floor so shadows
            // aren't pitch black).
            let mix = 0.3 + 0.7 * shade;
            let red = (rg * mix).clamp(0.0, 255.0) as u8;
            let grn = (gg * mix).clamp(0.0, 255.0) as u8;
            let blu = (bg * mix).clamp(0.0, 255.0) as u8;
            img.put_pixel(c, r, image::Rgba([red, grn, blu, 255]));
        }
    }

    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("png encode");
    buf
}

fn lerp_rgb(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}
