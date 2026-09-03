//! Console-subsystem companion to the GUI binary. Codex invokes this to
//! condition a generated image synchronously: a GUI-subsystem process is not
//! awaited by cmd or PowerShell, and its output would go nowhere.

use anyhow::bail;

fn main() -> anyhow::Result<()> {
    if !codex_image::generation::run_condition_image_cli(std::env::args_os().skip(1))? {
        bail!("usage: codex-image-condition --condition-image <source> <output>");
    }
    Ok(())
}
