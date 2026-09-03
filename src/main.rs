#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() -> anyhow::Result<()> {
    if codex_image::generation::run_condition_image_cli(std::env::args_os().skip(1))? {
        return Ok(());
    }
    codex_image::ui::run()
}
