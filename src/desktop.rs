//! Export desktop entries that a guest creates at runtime, such as games a
//! launcher installs, as host application launchers.
//!
//! Everything the guest writes is untrusted. The host therefore never copies
//! a guest entry: it generates each launcher from scratch, with an `Exec` that
//! only asks Nook to launch the entry's validated desktop ID inside the
//! sandbox, where the guest resolves and runs it. From the guest it takes only
//! sanitized display text, registered categories, a window class, and a PNG
//! icon, which is decoded here in memory-safe Rust and re-encoded as a plain
//! RGBA image so that the desktop's image loaders never parse guest bytes.
//! Keys that would let the guest act
//! on the host (MimeType handlers, actions, D-Bus activation, working
//! directories, paths to host files) are dropped.
//!
//! Guest files are read as the owner through directory descriptors with
//! openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV), so a
//! guest cannot redirect the reader to host files with symlinks or mounts,
//! and FIFOs or oversized files cannot stall or exhaust it.

use crate::config::{Profile, Result};
use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, fstat, open, openat2};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Directory below the host's applications and data directories that holds
/// every exported entry and icon. Nook owns it completely.
const NAMESPACE: &str = "nook-sandbox";
const MAX_ENTRIES: usize = 256;
/// Directory entries examined per sync; the guest controls how many exist.
const MAX_SCANNED: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
const MAX_ICON_BYTES: u64 = 1024 * 1024;
const MAX_ICON_DIMENSION: u32 = 1024;
const MAX_TEXT_CHARS: usize = 80;
const FALLBACK_ICON: &str = "application-x-executable";
/// Icon theme sizes searched in the guest, largest first.
const ICON_SIZES: [&str; 8] = [
    "512x512", "256x256", "192x192", "128x128", "96x96", "64x64", "48x48", "32x32",
];
/// Registered main categories of the desktop menu specification.
const CATEGORIES: [&str; 13] = [
    "AudioVideo",
    "Audio",
    "Video",
    "Development",
    "Education",
    "Game",
    "Graphics",
    "Network",
    "Office",
    "Science",
    "Settings",
    "System",
    "Utility",
];

/// A desktop entry ID that is safe as a file name on both sides and as an
/// unquoted Exec argument: no path separators, no field codes, no quoting.
pub fn valid_desktop_id(id: &str) -> bool {
    let Some(stem) = id.strip_suffix(".desktop") else {
        return false;
    };
    !stem.is_empty()
        && id.len() <= 128
        && stem
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Characters that could reorder, hide, or spoof the displayed text, such as
/// bidirectional overrides that would move the sandbox suffix.
fn invisible(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// Decode a desktop-entry string value, then reduce it to plain, single-line,
/// visible text of bounded length.
fn display_text(raw: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut characters = raw.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => decoded.push('\\'),
            Some(';') => decoded.push(';'),
            // \s, \n, \t, \r and anything unknown become separators.
            Some(_) | None => decoded.push(' '),
        }
    }
    // Control characters separate words; other invisible characters vanish.
    let visible: String = decoded
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                Some(' ')
            } else if invisible(character) {
                None
            } else {
                Some(character)
            }
        })
        .collect();
    let words: Vec<&str> = visible.split_whitespace().collect();
    let text: String = words.join(" ").chars().take(MAX_TEXT_CHARS).collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Escape plain text for a desktop-entry string value.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
}

#[derive(Debug, PartialEq)]
struct GuestEntry {
    name: String,
    comment: Option<String>,
    icon: Option<String>,
    categories: Vec<&'static str>,
    window_class: Option<String>,
}

/// Parse the main group of an untrusted desktop entry. Returns None for
/// anything that should not become a host launcher.
fn parse_entry(contents: &str) -> Option<GuestEntry> {
    let mut group = None;
    let mut keys = std::collections::BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[') {
            let name = name.trim_end().strip_suffix(']')?;
            if name == "Desktop Entry" && keys.contains_key("Type") {
                // A second main group is malformed; refuse to guess.
                return None;
            }
            group = Some(name.to_owned());
            continue;
        }
        if group.as_deref() != Some("Desktop Entry") {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim_end();
        // Localized variants are ignored; only the untranslated key is used.
        if key.contains('[') {
            continue;
        }
        if keys
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return None;
        }
    }
    let flag = |key: &str| keys.get(key).is_some_and(|value| value == "true");
    if keys.get("Type").map(String::as_str) != Some("Application")
        || flag("Hidden")
        || flag("NoDisplay")
        || flag("Terminal")
    {
        return None;
    }
    let name = display_text(keys.get("Name")?)?;
    let comment = keys.get("Comment").and_then(|value| display_text(value));
    let icon = keys.get("Icon").filter(|value| valid_token(value)).cloned();
    let mut categories = Vec::new();
    for category in keys
        .get("Categories")
        .into_iter()
        .flat_map(|value| value.split(';'))
    {
        if let Some(known) = CATEGORIES.iter().find(|known| **known == category)
            && !categories.contains(known)
        {
            categories.push(*known);
        }
    }
    let window_class = keys
        .get("StartupWMClass")
        .filter(|value| valid_token(value))
        .cloned();
    Some(GuestEntry {
        name,
        comment,
        icon,
        categories,
        window_class,
    })
}

/// Cheaply reject anything that is not a PNG within the size limits before
/// decoding it.
fn valid_png_header(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || !bytes.starts_with(SIGNATURE) || &bytes[8..16] != b"\0\0\0\x0dIHDR" {
        return false;
    }
    let dimension = |offset: usize| {
        u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    };
    let (width, height) = (dimension(16), dimension(20));
    (1..=MAX_ICON_DIMENSION).contains(&width) && (1..=MAX_ICON_DIMENSION).contains(&height)
}

/// Decode an untrusted PNG with the pure-Rust decoder, within allocation
/// limits, and encode its first image afresh as 8-bit RGBA. The result holds
/// only header, image data and end chunks: no text, colour profiles,
/// animation or other ancillary data from the guest, and no encoding quirks
/// for the host desktop's loaders to trip over.
fn reencode_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if !valid_png_header(bytes) {
        return None;
    }
    let side = MAX_ICON_DIMENSION as usize;
    let limits = png::Limits {
        bytes: side * side * 8,
    };
    let mut decoder = png::Decoder::new_with_limits(io::Cursor::new(bytes), limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width > MAX_ICON_DIMENSION || height > MAX_ICON_DIMENSION {
        return None;
    }
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let pixels = &pixels[..frame.buffer_size()];
    if frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|[red, green, blue]| [*red, *green, *blue, 0xff])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|[gray, alpha]| [*gray, *gray, *gray, *alpha])
            .collect(),
        png::ColorType::Grayscale => pixels
            .iter()
            .flat_map(|gray| [*gray, *gray, *gray, 0xff])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    if rgba.len() != width as usize * height as usize * 4 {
        return None;
    }
    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&rgba).ok()?;
    writer.finish().ok()?;
    Some(encoded)
}

fn resolve_beneath(directory: &File, path: &Path, flags: OFlags) -> io::Result<File> {
    Ok(File::from(openat2(
        directory,
        path,
        flags | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )?))
}

/// Read a regular file below `directory`, refusing links, mounts, special
/// files, and anything larger than `limit`.
fn read_beneath(directory: &File, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = resolve_beneath(
        directory,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY,
    )?;
    let metadata = fstat(&file)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(io::Error::other("not a regular file"));
    }
    if u64::try_from(metadata.st_size).unwrap_or(u64::MAX) > limit {
        return Err(io::Error::other("file too large"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("file too large"));
    }
    Ok(bytes)
}

fn find_icon(home: &File, name: &str) -> Option<Vec<u8>> {
    ICON_SIZES.iter().find_map(|size| {
        let path = Path::new(".local/share/icons/hicolor")
            .join(size)
            .join("apps")
            .join(format!("{name}.png"));
        read_beneath(home, &path, MAX_ICON_BYTES)
            .ok()
            .and_then(|bytes| reencode_png(&bytes))
    })
}

struct Export {
    id: String,
    entry: GuestEntry,
    icon: Option<Vec<u8>>,
}

/// Collect exportable entries from the guest's main home. Problems with
/// individual guest files skip those files; the guest controls them.
fn guest_entries(home_path: &Path) -> Result<Vec<Export>> {
    let home = File::from(open(
        home_path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let applications = match resolve_beneath(
        &home,
        Path::new(".local/share/applications"),
        OFlags::RDONLY | OFlags::DIRECTORY,
    ) {
        Ok(directory) => directory,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!("nk: ignoring guest applications directory: {error}");
            }
            return Ok(Vec::new());
        }
    };
    let mut ids = BTreeSet::new();
    for entry in rustix::fs::Dir::read_from(&applications)?.take(MAX_SCANNED) {
        let entry = entry?;
        if let Ok(name) = entry.file_name().to_str()
            && valid_desktop_id(name)
        {
            ids.insert(name.to_owned());
        }
    }
    let mut exports = Vec::new();
    for id in ids.into_iter().take(MAX_ENTRIES) {
        let Ok(bytes) = read_beneath(&applications, Path::new(&id), MAX_ENTRY_BYTES) else {
            continue;
        };
        let Some(entry) = String::from_utf8(bytes)
            .ok()
            .and_then(|text| parse_entry(&text))
        else {
            continue;
        };
        let icon = entry
            .icon
            .as_deref()
            .and_then(|name| find_icon(&home, name));
        exports.push(Export { id, entry, icon });
    }
    Ok(exports)
}

fn data_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        && path.is_absolute()
    {
        return Ok(path);
    }
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share"))
}

fn render(profile: &str, launcher: &str, export: &Export, icon: &str) -> String {
    let entry = &export.entry;
    let mut text = String::from("[Desktop Entry]\nType=Application\n");
    text += &format!("Name={} ({profile})\n", escape(&entry.name));
    if let Some(comment) = &entry.comment {
        text += &format!("Comment={}\n", escape(comment));
    }
    text += &format!("Icon={icon}\n");
    text += &format!("TryExec={launcher}\n");
    text += &format!("Exec={launcher} launch-desktop {profile} {}\n", export.id);
    text += "Terminal=false\n";
    if !entry.categories.is_empty() {
        text += &format!("Categories={};\n", entry.categories.join(";"));
    }
    if let Some(class) = &entry.window_class {
        text += &format!("StartupWMClass={class}\n");
    }
    text += &format!(
        "X-Nook-Sandbox={profile}\nX-Nook-Desktop-Id={}\n",
        export.id
    );
    text
}

/// Replace a file only when its contents change, so the desktop's directory
/// watches do not fire for no-op syncs.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    let directory = path.parent().ok_or("exported file has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Remove files in a Nook-owned directory that are not in `keep`.
fn prune(directory: &Path, keep: &BTreeSet<String>) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| keep.contains(name)) {
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
    }
    if keep.is_empty() {
        let _ = fs::remove_dir(directory);
    }
    Ok(())
}

fn sync_profile(profile: &Profile, data: &Path) -> Result<()> {
    let name = profile.get("name")?;
    let applications = data.join("applications").join(NAMESPACE).join(name);
    let icons = data.join(NAMESPACE).join("icons").join(name);
    let exports = if profile.flag("exportDesktopEntries") {
        match profile.persistent_home("main")? {
            Some(home) => guest_entries(&home)?,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let launcher = profile.get("desktopExec").unwrap_or_default();
    let exports = if launcher.starts_with('/')
        && launcher
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-".contains(&byte))
    {
        exports
    } else {
        Vec::new()
    };
    let mut kept_entries = BTreeSet::new();
    let mut kept_icons = BTreeSet::new();
    if !exports.is_empty() {
        fs::create_dir_all(&applications)?;
    }
    for export in &exports {
        let icon = match &export.icon {
            Some(bytes) => {
                fs::create_dir_all(&icons)?;
                let file = format!("{}.png", export.id.trim_end_matches(".desktop"));
                let path = icons.join(&file);
                write_if_changed(&path, bytes)?;
                kept_icons.insert(file);
                path.to_str()
                    .filter(|path| !path.contains(['\n', '\r']))
                    .unwrap_or(FALLBACK_ICON)
                    .to_owned()
            }
            None => FALLBACK_ICON.to_owned(),
        };
        let contents = render(name, launcher, export, &escape(&icon));
        write_if_changed(&applications.join(&export.id), contents.as_bytes())?;
        kept_entries.insert(export.id.clone());
    }
    prune(&applications, &kept_entries)?;
    prune(&icons, &kept_icons)?;
    Ok(())
}

/// Bring the exported launchers of `profiles` in line with their guests, and
/// remove launchers of profiles that are no longer configured.
pub fn sync(profiles: &[Profile], only: Option<&str>) -> Result<()> {
    let data = data_home()?;
    let mut configured = BTreeSet::new();
    let mut failures = 0;
    for profile in profiles {
        let name = profile.get("name")?;
        configured.insert(name.to_owned());
        if only.is_some_and(|only| only != name) {
            continue;
        }
        if let Err(error) = sync_profile(profile, &data) {
            eprintln!("nk: cannot export desktop entries of {name}: {error}");
            failures += 1;
        }
    }
    if let Some(only) = only {
        if !configured.contains(only) {
            return Err(format!("unknown profile {only}").into());
        }
    } else {
        prune(&data.join("applications").join(NAMESPACE), &configured)?;
        prune(&data.join(NAMESPACE).join("icons"), &configured)?;
    }
    if failures > 0 {
        return Err("some desktop entries could not be exported".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, net::UnixListener};

    fn encode_png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_text_chunk("Comment".into(), "guest-text".into())
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        encoded
    }

    fn decode_rgba(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(
            (frame.color_type, frame.bit_depth),
            (png::ColorType::Rgba, png::BitDepth::Eight)
        );
        pixels.truncate(frame.buffer_size());
        (frame.width, frame.height, pixels)
    }

    #[test]
    fn desktop_ids_are_plain_file_names() {
        for good in [
            "steam_app_10.desktop",
            "org.example.Game.desktop",
            "a-b.desktop",
        ] {
            assert!(valid_desktop_id(good), "{good}");
        }
        for bad in [
            ".desktop",
            ".hidden.desktop",
            "-flag.desktop",
            "../x.desktop",
            "a/b.desktop",
            "a b.desktop",
            "a%U.desktop",
            "a\"b.desktop",
            "game",
            "game.desktop.bak",
        ] {
            assert!(!valid_desktop_id(bad), "{bad}");
        }
    }

    #[test]
    fn display_text_cannot_inject_lines_or_reorder_the_suffix() {
        assert_eq!(
            display_text(r"Game\nExec=/bin/sh").as_deref(),
            Some("Game Exec=/bin/sh")
        );
        assert_eq!(
            display_text("Ga\u{202E}me\u{200B} \t x").as_deref(),
            Some("Game x")
        );
        assert_eq!(display_text(r"back\\slash").as_deref(), Some(r"back\slash"));
        assert_eq!(display_text("\u{202E}\u{2066}"), None);
        assert_eq!(display_text(&"x".repeat(500)).unwrap().chars().count(), 80);
    }

    #[test]
    fn only_display_keys_survive_parsing() {
        let entry = parse_entry(
            "[Desktop Entry]\nType=Application\nName=Game\nName[de]=Spiel\nExec=game\n\
             Icon=game_icon\nCategories=Game;X-Evil;Game;\nStartupWMClass=game.bin\n\
             MimeType=x-scheme-handler/https;\nDBusActivatable=true\nActions=a;\n\
             [Desktop Action a]\nExec=/bin/sh\n",
        )
        .unwrap();
        assert_eq!(
            entry,
            GuestEntry {
                name: "Game".into(),
                comment: None,
                icon: Some("game_icon".into()),
                categories: vec!["Game"],
                window_class: Some("game.bin".into()),
            }
        );
        let rendered = render(
            "dev",
            "/nix/profile/bin/nk",
            &Export {
                id: "game.desktop".into(),
                entry,
                icon: None,
            },
            FALLBACK_ICON,
        );
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("Exec="))
                .collect::<Vec<_>>(),
            ["Exec=/nix/profile/bin/nk launch-desktop dev game.desktop"]
        );
        for key in ["MimeType", "DBusActivatable", "Actions", "[Desktop Action"] {
            assert!(!rendered.contains(key), "{key}");
        }
        assert!(rendered.contains("Name=Game (dev)\n"));
    }

    #[test]
    fn unsafe_or_hidden_entries_are_refused() {
        for contents in [
            "[Desktop Entry]\nType=Link\nName=x\n",
            "[Desktop Entry]\nType=Application\nName=x\nNoDisplay=true\n",
            "[Desktop Entry]\nType=Application\nName=x\nHidden=true\n",
            "[Desktop Entry]\nType=Application\nName=x\nTerminal=true\n",
            "[Desktop Entry]\nType=Application\nName=x\nName=y\n",
            "[Desktop Entry]\nType=Application\nName=\u{202E}\n",
            "[Desktop Entry]\nType=Application\n[Desktop Entry]\nType=Application\nName=x\n",
        ] {
            assert_eq!(parse_entry(contents), None, "{contents:?}");
        }
        let entry =
            parse_entry("[Desktop Entry]\nType=Application\nName=x\nIcon=/etc/passwd\n").unwrap();
        assert_eq!(entry.icon, None);
    }

    #[test]
    fn icons_are_reencoded_without_guest_data() {
        let rgb = encode_png(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        assert!(rgb.windows(10).any(|window| window == b"guest-text"));
        let clean = reencode_png(&rgb).unwrap();
        assert!(!clean.windows(4).any(|window| window == b"tEXt"));
        assert!(!clean.windows(10).any(|window| window == b"guest-text"));
        assert_eq!(
            decode_rgba(&clean),
            (2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])
        );
        let gray = encode_png(1, 1, png::ColorType::GrayscaleAlpha, &[9, 128]);
        assert_eq!(
            decode_rgba(&reencode_png(&gray).unwrap()),
            (1, 1, vec![9, 9, 9, 128])
        );

        let mut indexed = Vec::new();
        let mut encoder = png::Encoder::new(&mut indexed, 2, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(vec![10, 20, 30, 40, 50, 60]);
        encoder.set_trns(vec![255, 0]);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 1]).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            decode_rgba(&reencode_png(&indexed).unwrap()),
            (2, 1, vec![10, 20, 30, 255, 40, 50, 60, 0])
        );

        let mut deep = Vec::new();
        let mut encoder = png::Encoder::new(&mut deep, 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc])
            .unwrap();
        writer.finish().unwrap();
        assert_eq!(
            decode_rgba(&reencode_png(&deep).unwrap()),
            (1, 1, vec![0x12, 0x56, 0x9a, 255])
        );

        assert_eq!(reencode_png(b"<svg/>"), None);
        assert_eq!(reencode_png(&rgb[..rgb.len() - 20]), None);
        let mut huge = rgb.clone();
        huge[16..20].copy_from_slice(&4096u32.to_be_bytes());
        assert_eq!(reencode_png(&huge), None);
    }

    #[test]
    fn guest_links_special_files_and_escapes_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let applications = home.join(".local/share/applications");
        let icons = home.join(".local/share/icons/hicolor/64x64/apps");
        fs::create_dir_all(&applications).unwrap();
        fs::create_dir_all(&icons).unwrap();
        let outside = root.path().join("host-secret.desktop");
        fs::write(&outside, "[Desktop Entry]\nType=Application\nName=Secret\n").unwrap();
        fs::write(
            applications.join("game.desktop"),
            "[Desktop Entry]\nType=Application\nName=Game\nIcon=game\n",
        )
        .unwrap();
        fs::write(
            applications.join("linked-icon.desktop"),
            "[Desktop Entry]\nType=Application\nName=Linked\nIcon=linked\n",
        )
        .unwrap();
        symlink(&outside, applications.join("leak.desktop")).unwrap();
        let _listener = UnixListener::bind(applications.join("socket.desktop")).unwrap();
        nix::unistd::mkfifo(
            &applications.join("fifo.desktop"),
            nix::sys::stat::Mode::S_IRWXU,
        )
        .unwrap();
        let icon = encode_png(1, 1, png::ColorType::Rgba, &[1, 2, 3, 4]);
        fs::write(icons.join("game.png"), &icon).unwrap();
        fs::write(root.path().join("host.png"), &icon).unwrap();
        symlink(root.path().join("host.png"), icons.join("linked.png")).unwrap();
        let exports = guest_entries(&home).unwrap();
        let ids: Vec<_> = exports.iter().map(|export| export.id.as_str()).collect();
        assert_eq!(ids, ["game.desktop", "linked-icon.desktop"]);
        assert_eq!(
            decode_rgba(exports[0].icon.as_deref().unwrap()),
            (1, 1, vec![1, 2, 3, 4])
        );
        assert_eq!(exports[1].icon, None);

        // A symlinked applications directory is refused as a whole.
        let other = root.path().join("other");
        fs::create_dir_all(other.join(".local/share")).unwrap();
        symlink(&applications, other.join(".local/share/applications")).unwrap();
        assert!(guest_entries(&other).unwrap().is_empty());
    }
}
