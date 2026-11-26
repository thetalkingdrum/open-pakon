use pakon_image::psi24::tables;
fn main() {
    let a: Vec<f32> = std::env::args()
        .skip(1)
        .map(|v| v.parse().unwrap())
        .collect();
    let t = tables();
    let r = t
        .rpd_romm
        .lookup([a[0] / 4095.0, a[1] / 4095.0, a[2] / 4095.0]);
    let s = t.romm_srgb.lookup(r);
    println!(
        "romm8 {:.1} {:.1} {:.1} | srgb8 {:.1} {:.1} {:.1}",
        r[0] * 255.0,
        r[1] * 255.0,
        r[2] * 255.0,
        s[0] * 255.0,
        s[1] * 255.0,
        s[2] * 255.0
    );
}
