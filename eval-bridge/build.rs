/// PyO3 build script — emits the required `cargo:rustc-cfg` directives
/// so the proc-macro machinery knows how to link against the Python
/// interpreter found on the build host.
///
/// See: <https://pyo3.rs/main/building-and-distribution/multiple-python-versions>
fn main() {
    // Let pyo3's build helper detect the Python installation.
    // On Windows this reads the registry, on Unix it shells out to
    // `python3-config`.
    pyo3_build_config::add_extension_module_link_args();
}
