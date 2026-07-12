use anyhow::{Result, bail};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => waywarm::tui::run(),
        Some("daemon") if arguments.next().is_none() => waywarm::service_tui::run(),
        Some("--daemon") if arguments.next().is_none() => {
            waywarm::service::retire_legacy_service()?;
            waywarm::daemon::run()
        }
        Some("--help" | "-h") if arguments.next().is_none() => {
            println!(
                "Waywarm — a wlroots blue-light filter\n\nUsage:\n  waywarm           Run standalone or connect to the optional service\n  waywarm daemon    Manage the optional background service\n  waywarm --daemon  Internal service process\n  waywarm --help    Show this help"
            );
            Ok(())
        }
        Some(argument) => bail!("unknown argument {argument:?}; use --help"),
    }
}
