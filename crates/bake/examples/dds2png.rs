//! Debug helper: decode mip 0 of a DDS file to a PNG. `dds2png <in.dds> <out.png>`

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "usage: dds2png <in.dds> <out.png>");
    let img = aa_bake::dds::decode_dds(&std::fs::read(&args[0])?)?;
    let file = std::io::BufWriter::new(std::fs::File::create(&args[1])?);
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img.data)?;
    Ok(())
}
