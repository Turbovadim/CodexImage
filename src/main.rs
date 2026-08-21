use anyhow::{Context, bail};
use std::ffi::OsStr;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(OsStr::new("--condition-image")) {
        let source = PathBuf::from(arguments.next().context("missing source image path")?);
        let destination = PathBuf::from(arguments.next().context("missing output image path")?);
        if arguments.next().is_some() {
            bail!("--condition-image accepts exactly a source and output path");
        }
        let applied =
            codex_image::generation::condition_image_for_reingestion(&source, &destination)?;
        println!("{}", if applied { "conditioned" } else { "unchanged" });
        return Ok(());
    }
    codex_image::ui::run()
}
