use std::{
    env, fs,
    path::{Path, PathBuf},
};

use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg,
};

type BuildResult<T> = Result<T, Box<dyn std::error::Error>>;

const APP_NAME: &str = "Spotlit";
const EXE_NAME: &str = "spotlit.exe";
const ICON_GROUP_ID: u16 = 1;
const LANGUAGE_EN_US: u16 = 0x0409;
const MEMORY_FLAGS: u16 = 0x1030;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const VERSION_RESOURCE_ID: u16 = 1;
const LOGO_SVG: &[u8] = include_bytes!("resources/icons/spotlit-logo.svg");
const TRANSLATION_CATALOGS: [&str; 2] = [
    "resources/i18n/de/LC_MESSAGES/spotlit.po",
    "resources/i18n/zh-CN/LC_MESSAGES/spotlit.po",
];

fn main() -> BuildResult<()> {
    println!("cargo:rerun-if-changed=src/ui/app-window.slint");
    println!("cargo:rerun-if-changed=resources/icons/spotlit-logo.svg");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    verify_translation_catalogs()?;

    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("resources/i18n")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("src/ui/app-window.slint", config)?;
    generate_windows_resources()?;

    Ok(())
}

fn verify_translation_catalogs() -> BuildResult<()> {
    for catalog in TRANSLATION_CATALOGS {
        println!("cargo:rerun-if-changed={catalog}");
        if !Path::new(catalog).is_file() {
            return Err(format!("missing translation catalog: {catalog}").into());
        }
    }

    Ok(())
}

fn generate_windows_resources() -> BuildResult<()> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let resource_path = out_dir.join("spotlit.res");
    let icon_images = render_icon_images()?;

    let mut entries = Vec::with_capacity(icon_images.len() + 2);
    for image in &icon_images {
        entries.push(ResourceEntry::new(RT_ICON, image.id, image.data.clone()));
    }
    entries.push(ResourceEntry::new(
        RT_GROUP_ICON,
        ICON_GROUP_ID,
        icon_group_resource(&icon_images),
    ));
    entries.push(ResourceEntry::new(
        RT_VERSION,
        VERSION_RESOURCE_ID,
        version_info_resource(),
    ));

    fs::write(&resource_path, resource_file(&entries))?;
    println!(
        "cargo:rustc-link-arg-bin=spotlit={}",
        native_link_arg(&resource_path)
    );

    Ok(())
}

fn native_link_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct ResourceEntry {
    type_id: u16,
    name_id: u16,
    data: Vec<u8>,
}

impl ResourceEntry {
    fn new(type_id: u16, name_id: u16, data: Vec<u8>) -> Self {
        Self {
            type_id,
            name_id,
            data,
        }
    }
}

fn resource_file(entries: &[ResourceEntry]) -> Vec<u8> {
    let mut output = Vec::new();
    append_null_resource(&mut output);

    for entry in entries {
        append_resource(&mut output, entry);
    }

    output
}

fn append_null_resource(output: &mut Vec<u8>) {
    push_u32(output, 0);
    push_u32(output, 32);
    push_u16(output, 0);
    align_to_dword(output);
    push_u16(output, 0);
    align_to_dword(output);
    push_u32(output, 0);
    push_u16(output, 0);
    push_u16(output, 0);
    push_u32(output, 0);
    push_u32(output, 0);
}

fn append_resource(output: &mut Vec<u8>, entry: &ResourceEntry) {
    align_to_dword(output);
    push_u32(output, entry.data.len() as u32);
    push_u32(output, 32);
    push_ordinal(output, entry.type_id);
    push_ordinal(output, entry.name_id);
    push_u32(output, 0);
    push_u16(output, MEMORY_FLAGS);
    push_u16(output, LANGUAGE_EN_US);
    push_u32(output, 0);
    push_u32(output, 0);
    output.extend_from_slice(&entry.data);
    align_to_dword(output);
}

fn push_ordinal(output: &mut Vec<u8>, ordinal: u16) {
    push_u16(output, 0xffff);
    push_u16(output, ordinal);
}

struct IconImage {
    id: u16,
    size: u16,
    data: Vec<u8>,
}

fn render_icon_images() -> BuildResult<Vec<IconImage>> {
    let tree = usvg::Tree::from_data(LOGO_SVG, &usvg::Options::default())?;
    let sizes = [16, 24, 32, 48, 64, 128, 256];

    sizes
        .into_iter()
        .enumerate()
        .map(|(index, size)| {
            Ok(IconImage {
                id: index as u16 + 1,
                size,
                data: icon_dib(size, render_svg(&tree, size)?),
            })
        })
        .collect()
}

fn render_svg(tree: &usvg::Tree, size: u16) -> BuildResult<Vec<u8>> {
    let size = u32::from(size);
    let mut pixmap = Pixmap::new(size, size).ok_or("invalid icon size")?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());

    resvg::render(
        tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    Ok(unpremultiply_rgba(pixmap.data().to_vec()))
}

fn unpremultiply_rgba(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }

        pixel[0] = unpremultiply_channel(pixel[0], alpha);
        pixel[1] = unpremultiply_channel(pixel[1], alpha);
        pixel[2] = unpremultiply_channel(pixel[2], alpha);
    }

    rgba
}

fn unpremultiply_channel(value: u8, alpha: u16) -> u8 {
    ((u16::from(value) * 255 + alpha / 2) / alpha).min(255) as u8
}

fn icon_dib(size: u16, rgba: Vec<u8>) -> Vec<u8> {
    let size = u32::from(size);
    let pixel_bytes = size * size * 4;
    let mask_stride = size.div_ceil(32) * 4;
    let mask_bytes = mask_stride * size;
    let mut output = Vec::with_capacity((40 + pixel_bytes + mask_bytes) as usize);

    push_u32(&mut output, 40);
    push_i32(&mut output, size as i32);
    push_i32(&mut output, (size * 2) as i32);
    push_u16(&mut output, 1);
    push_u16(&mut output, 32);
    push_u32(&mut output, 0);
    push_u32(&mut output, pixel_bytes + mask_bytes);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);

    for y in (0..size).rev() {
        let row_start = (y * size * 4) as usize;
        for pixel in rgba[row_start..row_start + (size * 4) as usize].chunks_exact(4) {
            output.push(pixel[2]);
            output.push(pixel[1]);
            output.push(pixel[0]);
            output.push(pixel[3]);
        }
    }

    output.resize(output.len() + mask_bytes as usize, 0);
    output
}

fn icon_group_resource(images: &[IconImage]) -> Vec<u8> {
    let mut output = Vec::with_capacity(6 + images.len() * 14);

    push_u16(&mut output, 0);
    push_u16(&mut output, 1);
    push_u16(&mut output, images.len() as u16);

    for image in images {
        output.push(icon_dimension(image.size));
        output.push(icon_dimension(image.size));
        output.push(0);
        output.push(0);
        push_u16(&mut output, 1);
        push_u16(&mut output, 32);
        push_u32(&mut output, image.data.len() as u32);
        push_u16(&mut output, image.id);
    }

    output
}

fn icon_dimension(size: u16) -> u8 {
    u8::try_from(size).unwrap_or(0)
}

fn version_info_resource() -> Vec<u8> {
    let version = env!("CARGO_PKG_VERSION");
    let fixed_info = fixed_file_info(version);
    let strings = [
        ("CompanyName", "Spotlit"),
        ("FileDescription", APP_NAME),
        ("FileVersion", version),
        ("InternalName", "spotlit"),
        ("LegalCopyright", "Copyright (c) Spotlit contributors"),
        ("OriginalFilename", EXE_NAME),
        ("ProductName", APP_NAME),
        ("ProductVersion", version),
    ]
    .into_iter()
    .map(|(key, value)| text_version_block(key, value, Vec::new()))
    .collect();

    let string_table = version_block("040904B0", 0, 1, &[], strings);
    let string_file_info = version_block("StringFileInfo", 0, 1, &[], vec![string_table]);
    let translation = translation_value(LANGUAGE_EN_US, 0x04b0);
    let translation = version_block("Translation", 4, 0, &translation, Vec::new());
    let var_file_info = version_block("VarFileInfo", 0, 1, &[], vec![translation]);

    version_block(
        "VS_VERSION_INFO",
        fixed_info.len() as u16,
        0,
        &fixed_info,
        vec![string_file_info, var_file_info],
    )
}

fn text_version_block(key: &str, value: &str, children: Vec<Vec<u8>>) -> Vec<u8> {
    let value = utf16z_bytes(value);
    version_block(key, (value.len() / 2) as u16, 1, &value, children)
}

fn version_block(
    key: &str,
    value_length: u16,
    value_type: u16,
    value: &[u8],
    children: Vec<Vec<u8>>,
) -> Vec<u8> {
    let mut output = vec![0; 6];

    output.extend_from_slice(&utf16z_bytes(key));
    align_to_dword(&mut output);
    output.extend_from_slice(value);
    align_to_dword(&mut output);

    for child in children {
        output.extend_from_slice(&child);
    }

    let length = output.len() as u16;
    output[0..2].copy_from_slice(&length.to_le_bytes());
    output[2..4].copy_from_slice(&value_length.to_le_bytes());
    output[4..6].copy_from_slice(&value_type.to_le_bytes());

    output
}

fn fixed_file_info(version: &str) -> Vec<u8> {
    let [major, minor, patch, build] = numeric_version(version);
    let version_ms = (u32::from(major) << 16) | u32::from(minor);
    let version_ls = (u32::from(patch) << 16) | u32::from(build);
    let mut output = Vec::with_capacity(52);

    push_u32(&mut output, 0xfeef04bd);
    push_u32(&mut output, 0x0001_0000);
    push_u32(&mut output, version_ms);
    push_u32(&mut output, version_ls);
    push_u32(&mut output, version_ms);
    push_u32(&mut output, version_ls);
    push_u32(&mut output, 0x0000_003f);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0x0004_0004);
    push_u32(&mut output, 0x0000_0001);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);

    output
}

fn numeric_version(version: &str) -> [u16; 4] {
    let mut values = [0; 4];

    for (index, part) in version.split(['.', '-']).take(values.len()).enumerate() {
        let digits: String = part
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        values[index] = digits.parse().unwrap_or(0);
    }

    values
}

fn translation_value(language: u16, code_page: u16) -> Vec<u8> {
    let mut output = Vec::with_capacity(4);
    push_u16(&mut output, language);
    push_u16(&mut output, code_page);
    output
}

fn utf16z_bytes(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn align_to_dword(output: &mut Vec<u8>) {
    let padding = output.len().next_multiple_of(4) - output.len();
    output.resize(output.len() + padding, 0);
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}
