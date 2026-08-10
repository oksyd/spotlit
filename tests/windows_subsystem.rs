use std::{
    env, fs,
    path::{Path, PathBuf},
};

const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;

#[test]
#[ignore]
fn assert_windows_gui_subsystem_artifact() {
    let path = env::var("SPOTLIT_ASSERT_WINDOWS_GUI_SUBSYSTEM")
        .expect("SPOTLIT_ASSERT_WINDOWS_GUI_SUBSYSTEM must point to an executable");
    let artifact = resolve_artifact_path(&path);
    assert_eq!(
        read_pe_subsystem(&artifact),
        IMAGE_SUBSYSTEM_WINDOWS_GUI,
        "expected a Windows GUI subsystem executable at {}",
        artifact.display()
    );
}

fn resolve_artifact_path(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.exists() || path.is_absolute() {
        return path;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package_relative = manifest_dir.join(&path);
    if package_relative.exists() {
        return package_relative;
    }

    path
}

fn read_pe_subsystem(path: &Path) -> u16 {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"));
    assert!(bytes.len() > 0x40, "{path:?} is too small to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "{path:?} does not have an MZ header");

    let pe_offset = read_u32(&bytes, 0x3c) as usize;
    assert!(
        bytes.len() > pe_offset + 0x5c,
        "{path:?} is too small for a PE optional header"
    );
    assert_eq!(
        &bytes[pe_offset..pe_offset + 4],
        b"PE\0\0",
        "{path:?} does not have a PE signature"
    );

    let optional_header_offset = pe_offset + 4 + 20;
    read_u16(&bytes, optional_header_offset + 68)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 slice"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 slice"))
}
