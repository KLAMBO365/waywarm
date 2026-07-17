use anyhow::{Result, bail};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => waywarm::tui::run(),
        Some("daemon") if arguments.len() == 0 => waywarm::service_tui::run(),
        Some("--daemon") if arguments.len() == 0 => {
            waywarm::service::retire_legacy_service()?;
            waywarm::daemon::run()
        }
        Some(command @ ("status" | "set" | "enable" | "disable" | "toggle")) => {
            waywarm::cli::run(command, arguments)
        }
        Some("--help" | "-h" | "help") if arguments.len() == 0 => {
            print_help();
            Ok(())
        }
        Some(argument) => bail!("unknown argument {argument:?}; use --help"),
    }
}

fn print_help() {
    println!(
        "\
Waywarm — a wlroots blue-light filter

Usage:
  waywarm                 Open the settings interface
  waywarm daemon          Manage the optional background service
  waywarm status [--json] Show filter state (requires a running daemon)
  waywarm enable|disable|toggle [--json]
  waywarm set [options]   Change settings from scripts
  waywarm --daemon        Internal service process
  waywarm --help          Show this help

Set options:
  --on | --off
  --mode automatic|manual
  --warmth <0-100>  --brightness <10-100>
  --day-warmth <0-100>  --day-brightness <10-100>
  --night-warmth <0-100>  --night-brightness <10-100>
  --night-start <HH:MM>  --day-start <HH:MM>
  --transition <0-240>
  --timing fixed|location  --latitude <deg>  --longitude <deg>
  --json

CLI commands talk to a running daemon. Install the service with
`waywarm daemon`, or keep the settings UI open."
    );
}
