mod gen;

fn main() -> nih_plug_xtask::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("gen-sources")) {
        let outdir = args.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: cargo xtask gen-sources <outdir>",
            )
        })?;
        if args.next().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: cargo xtask gen-sources <outdir>",
            )
            .into());
        }
        gen::generate(std::path::Path::new(&outdir))?;
        return Ok(());
    }
    nih_plug_xtask::main()
}
