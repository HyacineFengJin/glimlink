// SPDX-License-Identifier: MIT
fn main() {
    let _interrupt = runtime_operator::interrupt::install().ok();
    let result =
        runtime_operator::Options::parse(std::env::args().skip(1)).and_then(
            |options| match options {
                Some(options) => runtime_operator::run_with_exit_code(options),
                None => {
                    println!("{}", runtime_operator::HELP);
                    Ok(0)
                }
            },
        );
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("agentctl: {error}");
            std::process::exit(if runtime_operator::interrupt::is_interrupted() {
                130
            } else {
                1
            });
        }
    }
}
