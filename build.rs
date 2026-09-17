use std::env;
use std::fs::File;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let png_path = manifest_dir.join("assets/icon.png");
    println!("cargo:rerun-if-changed={}", png_path.display());

    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        return;
    }

    let ico_path = write_windows_ico(&png_path);
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(ico_path.to_str().expect("icon path must be UTF-8"));
    resource.set("ProductName", "League Accounts");
    resource.set("FileDescription", "League of Legends account manager");
    resource.set("OriginalFilename", "LeagueAccounts.exe");
    resource.set("InternalName", "LeagueAccounts");
    resource
        .compile()
        .expect("failed to embed the Windows icon resource");
}

fn write_windows_ico(png_path: &std::path::Path) -> PathBuf {
    let source = image::open(png_path).expect("assets/icon.png must be a valid PNG");
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16_u32, 32, 48, 256] {
        let resized = source.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let icon_image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        icon_dir.add_entry(
            ico::IconDirEntry::encode(&icon_image).expect("failed to encode an ICO size"),
        );
    }

    let ico_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icon.ico");
    icon_dir
        .write(File::create(&ico_path).expect("failed to create the generated ICO"))
        .expect("failed to write the generated ICO");
    ico_path
}
