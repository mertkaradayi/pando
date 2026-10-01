//! Stops a build for anything but Unix with one line that says where pando
//! runs, rather than the dozens of errors a Windows build ends in: its
//! process control is Unix's throughout, from sessions and process groups
//! to the signals that stop them.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_UNIX").is_none() {
        let target = std::env::var("TARGET").unwrap_or_default();
        eprintln!(
            "pando builds for macOS and Linux, not for {target}. On Windows, build and run it \
             inside WSL 2, with the repository in WSL's own filesystem: \
             https://github.com/mertkaradayi/pando#install"
        );
        std::process::exit(1);
    }
}
