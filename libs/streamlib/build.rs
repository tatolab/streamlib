fn main() {
    // Link Metal framework on macOS for MP4 writer
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=Metal");
    }
}
