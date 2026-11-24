//! `image-id <user_elf> <out.bin>`: wraps a guest user ELF with the RISC Zero v1compat kernel into the `.bin`
//! (`ProgramBinary`) that `wallet program-loader deploy --elf` takes, as risc0-build does, and prints the image id.
//! `image-id <program.bin>`: prints the image id of an already wrapped `.bin`.
//! No LEZ dependencies, so CI builds it in seconds.

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    match a.as_slice() {
        [_, bin_path] => {
            let bin = std::fs::read(bin_path)?;
            let id = risc0_binfmt::compute_image_id(&bin).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{bin_path}: bin {} bytes, image id {id}", bin.len());
        }
        [_, elf_path, out] => {
            let elf = std::fs::read(elf_path)?;
            let bin = risc0_binfmt::ProgramBinary::new(&elf, risc0_zkos_v1compat::V1COMPAT_ELF).encode();
            std::fs::write(out, &bin)?;
            let id = risc0_binfmt::compute_image_id(&bin).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "{out}: user ELF {} bytes, bin {} bytes, image id {id}",
                elf.len(),
                bin.len()
            );
        }
        _ => anyhow::bail!("usage: image-id <user_elf> <out.bin> | image-id <program.bin>"),
    }
    Ok(())
}
