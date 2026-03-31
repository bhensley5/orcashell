fn main() {
    #[cfg(windows)]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let icon_path = std::path::Path::new(&manifest_dir).join("../../assets/AppIcon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon_path.to_str().expect("icon path should be valid UTF-8"));
        res.compile().expect("failed to compile Windows resources");
    }
}
