use std::process::ExitCode;

fn main() -> ExitCode {
    match smartcrab_release::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:?}");
            ExitCode::FAILURE
        }
    }
}
