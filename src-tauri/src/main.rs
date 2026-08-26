#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = agent_workspace_lib::initialize_from_cli() {
        eprintln!("{error}");
        std::process::exit(1);
    }

    match agent_workspace_lib::maybe_handle_cli_subcommand() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    agent_workspace_lib::run();
}
